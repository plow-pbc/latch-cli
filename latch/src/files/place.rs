//! The host paths of `file get` and `file put`, and putting an upload in
//! place.
//!
//! `source` and `destination` resolve the caller's path on the host before
//! the call is judged, as exec's paths are (`exec::resolve`, `check_path`):
//! `~` expanded, symlinks resolved, refused if it is not under home or is
//! hidden; a put's folder is given the file's name. The judge and the
//! audit see that path.
//!
//! `place` moves the bytes the confined worker staged to the destination,
//! outside the sandbox. It opens the destination's folder once (refusing a
//! symlink there), checks that handle is still the folder that was judged
//! and passes the checks again, then works only relative to it: a staging
//! file beside the destination, written and synced, then renamed over it.
//! A folder swapped for a symlink after judging cannot redirect the write,
//! and a put that breaks off leaves the old file as it was.

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::exec::{check_path, is_hidden, resolve};
use crate::failure::{Failure, code};
#[cfg(unix)]
use crate::sandbox::{open_at, open_folder};
use crate::worker::WorkerSetup;

/// The file a get of `remote` reads: in home, or a system file exec can
/// read too (the worker's sandbox limits it to those), never a hidden one.
pub fn source(remote: &str, workers: &WorkerSetup) -> Result<PathBuf, Failure> {
    let path = resolve(&workers.home, Path::new(remote))?;
    if path.starts_with(&workers.home) {
        check_path(workers, &path, false)?;
    } else if is_hidden(workers, &path) {
        let message = format!("{remote} is latch state or a hidden path");
        return Err(Failure::new(code::bad_path, message));
    }
    Ok(path)
}

/// The file a put of the client's file `name` to `remote` writes.
pub fn destination(
    remote: &str,
    name: Option<&str>,
    workers: &WorkerSetup,
) -> Result<PathBuf, Failure> {
    let mut path = resolve(&workers.home, Path::new(remote))?;
    // A folder takes the file under its own name, as `cp` does.
    if path.is_dir() {
        let plain = name.filter(|name| Path::new(name).file_name() == Some(name.as_ref()));
        let Some(name) = plain else {
            return Err(Failure::new(
                code::is_a_directory,
                format!("{remote} is a folder, and the file has no plain name to go under"),
            ));
        };
        // Resolved too: a link at that name is followed and checked.
        path = resolve(&workers.home, &path.join(name))?;
        if path.is_dir() {
            let message = format!("{} is a folder", path.display());
            return Err(Failure::new(code::is_a_directory, message));
        }
    }
    if !path.parent().is_some_and(Path::is_dir) {
        let message = format!("{remote}: no folder to put it in");
        return Err(Failure::new(code::not_found, message));
    }
    check_path(workers, &path, true)?;
    // The latch this host runs stays as it is.
    let program = workers.program.canonicalize();
    if program.as_ref().is_ok_and(|program| *program == path) {
        let message = format!("{remote} is the latch this host runs");
        return Err(Failure::new(code::bad_path, message));
    }
    Ok(path)
}

/// Moves the file at `staged` to `path`, which `destination` resolved.
#[cfg(unix)]
pub fn place(staged: &Path, path: &Path, workers: &WorkerSetup) -> Result<(), Failure> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        let message = format!("{} names no file", path.display());
        return Err(Failure::new(code::bad_path, message));
    };
    let failed =
        |err: io::Error| Failure::new(code::io_error, format!("{}: {err}", path.display()));
    // The folder that was judged, or nothing: not one swapped for a symlink,
    // nor one under a folder swapped for one. It must pass again.
    let dir = open_folder(folder).map_err(|_| changed(folder))?;
    check_path(workers, path, true)?;

    let temp = format!(
        ".{}.latch-{}-{:08x}.part",
        name.to_string_lossy(),
        std::process::id(),
        rand::random::<u32>()
    );
    // A file it replaces keeps its permissions, from the moment the new one
    // exists: a 0600 key stays 0600, a script stays executable.
    let kept = mode_of(&dir, name.as_bytes());
    // `O_EXCL | O_NOFOLLOW`: never an existing name, nor a link at it.
    let mut file = open_at(
        &dir,
        temp.as_ref(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        kept.unwrap_or(0o666),
    )
    .map_err(failed)?;
    // The umask may have taken bits off the kept mode; put them back.
    let restored = kept
        .map(|mode| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .transpose();
    let written = restored
        .and_then(|_| File::open(staged))
        .and_then(|mut staged| io::copy(&mut staged, &mut file))
        .and_then(|_| file.sync_all())
        .and_then(|()| rename(&dir, &temp, name.as_bytes()));
    if let Err(err) = written {
        let _ = unlink(&dir, &temp);
        return Err(failed(err));
    }
    Ok(())
}

/// The permission bits of the regular file `name` in `dir`, if there is
/// one (a link there is not followed).
// `mode_t` is a u16 on macOS and a u32 on Linux.
#[allow(clippy::unnecessary_cast)]
#[cfg(unix)]
fn mode_of(dir: &File, name: &[u8]) -> Option<u32> {
    let name = c_name(name).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `name` is NUL-terminated and outlives the call, `dir` is open,
    // and fstatat fills `stat` when it returns 0.
    let found = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0;
    // SAFETY: fstatat succeeded, so `stat` is initialized.
    let stat = found.then(|| unsafe { stat.assume_init() })?;
    (stat.st_mode & libc::S_IFMT == libc::S_IFREG).then_some(stat.st_mode as u32 & 0o7777)
}

fn changed(folder: &Path) -> Failure {
    Failure::new(
        code::destination_changed,
        format!(
            "{} changed after the put was checked; nothing was written",
            folder.display()
        ),
    )
}

#[cfg(unix)]
fn c_name(name: &[u8]) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(unix)]
fn rename(dir: &File, from: &str, to: &[u8]) -> io::Result<()> {
    let (from, to) = (c_name(from.as_bytes())?, c_name(to)?);
    let fd = dir.as_raw_fd();
    // SAFETY: both names are NUL-terminated and outlive the call; `fd` is open.
    if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn unlink(dir: &File, name: &str) -> io::Result<()> {
    let name = c_name(name.as_bytes())?;
    // SAFETY: `name` is NUL-terminated and outlives the call; `dir` is open.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Windows: the same steps without `*at` calls. The folder is opened once,
/// not through a link at it, and must still be the folder that was judged;
/// the new file is made in it and checked by its handle to be there; and it
/// is renamed over the destination relative to that open folder, so a folder
/// swapped meanwhile cannot redirect it. A file it replaces keeps its
/// access list: the new one is made with it.
#[cfg(windows)]
pub fn place(staged: &Path, path: &Path, workers: &WorkerSetup) -> Result<(), Failure> {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };

    use crate::sandbox::final_path;

    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        let message = format!("{} names no file", path.display());
        return Err(Failure::new(code::bad_path, message));
    };
    let failed =
        |err: io::Error| Failure::new(code::io_error, format!("{}: {err}", path.display()));
    // The folder that was judged, or nothing: not a link, nor reached
    // through a folder swapped for one. It must pass again.
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(folder)
        .ok()
        .filter(|dir| final_path(dir).is_ok_and(|real| real == folder))
        .ok_or_else(|| changed(folder))?;
    check_path(workers, path, true)?;

    let temp = folder.join(format!(
        ".{}.latch-{}-{:08x}.part",
        name.to_string_lossy(),
        std::process::id(),
        rand::random::<u32>()
    ));
    // Made with the access list of a file it replaces, protection included,
    // and shared with no one until it is in place: an owner-only key in a
    // folder others read is never theirs to open, not even in between.
    let like = AccessOf::file(path).map_err(failed)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: like
            .as_ref()
            .map_or(std::ptr::null_mut(), AccessOf::descriptor),
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = temp.as_os_str().encode_wide().chain([0]).collect();
    // Never an existing name (CREATE_NEW), nor a link at it; DELETE, so it
    // can be renamed; no sharing.
    // SAFETY: `wide` is NUL-terminated; `attributes` and the descriptor it
    // points at (in `like`) outlive the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | DELETE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    drop(like);
    if handle == INVALID_HANDLE_VALUE {
        return Err(failed(io::Error::last_os_error()));
    }
    // SAFETY: a new handle, owned by the File from here on.
    let mut file = unsafe { File::from_raw_handle(handle) };
    let written = final_path(&file)
        .and_then(|real| match real.parent() == Some(folder) {
            true => Ok(()),
            false => Err(io::Error::other(
                "the folder changed while the file was made",
            )),
        })
        .and_then(|()| File::open(staged))
        .and_then(|mut staged| io::copy(&mut staged, &mut file))
        .and_then(|_| file.sync_all())
        .and_then(|()| rename_into(&file, &dir, name));
    drop((file, dir));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(failed(err));
    }
    Ok(())
}

/// The access list of the file at `path`, and whether it is protected from
/// inheriting, as a descriptor to make another file with.
#[cfg(windows)]
struct AccessOf {
    /// As Windows gave it: holds the list `made` points at.
    read: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    made: Box<windows_sys::Win32::Security::SECURITY_DESCRIPTOR>,
}

#[cfg(windows)]
impl AccessOf {
    /// None when no file is there.
    fn file(path: &Path) -> std::io::Result<Option<AccessOf>> {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use std::ptr::null_mut;
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
            InitializeSecurityDescriptor, SE_DACL_PROTECTED, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl,
        };
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL};
        use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

        let old = match std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(old) => old,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let (mut dacl, mut read): (*mut ACL, _) = (null_mut(), null_mut());
        // SAFETY: a live handle with READ_CONTROL; `read`, which holds
        // `dacl`, is freed when the AccessOf is dropped.
        let status = unsafe {
            GetSecurityInfo(
                old.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut read,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let mut access = AccessOf {
            read,
            // SAFETY: zeroes are a valid start; Initialize sets it up.
            made: Box::new(unsafe { std::mem::zeroed() }),
        };
        let made = (&raw mut *access.made).cast();
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: `read` is live; `made` is ours, pointing at `dacl` in `read`,
        // which lives as long.
        let done = unsafe {
            GetSecurityDescriptorControl(read, &mut control, &mut revision) != 0
                && InitializeSecurityDescriptor(made, SECURITY_DESCRIPTOR_REVISION) != 0
                && SetSecurityDescriptorDacl(made, 1, dacl, 0) != 0
                && SetSecurityDescriptorControl(
                    made,
                    SE_DACL_PROTECTED,
                    control & SE_DACL_PROTECTED,
                ) != 0
        };
        // On failure `access` frees what it holds as it drops, after the
        // error is read.
        if !done {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Some(access))
    }

    fn descriptor(&self) -> *mut std::ffi::c_void {
        (&raw const *self.made).cast_mut().cast()
    }
}

#[cfg(windows)]
impl Drop for AccessOf {
    fn drop(&mut self) {
        // SAFETY: allocated by GetSecurityInfo, freed once.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.read) };
    }
}

/// Renames the open `file` to `name` in the open folder `dir`, replacing
/// what is there. Windows' own rename (`NtSetInformationFile`) takes the
/// name relative to `dir`; the Win32 one (`SetFileInformationByHandle`)
/// makes a bare name relative to this process's working folder instead.
#[cfg(windows)]
fn rename_into(
    file: &std::fs::File,
    dir: &std::fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let wide: Vec<u16> = name.encode_wide().collect();
    let head = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let size = (head + wide.len() * 2).max(size_of::<FILE_RENAME_INFORMATION>());
    // u64s, so the structure is aligned.
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr() as *mut FILE_RENAME_INFORMATION;
    // SAFETY: `buffer` holds the structure, and the name from `FileName` on.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = dir.as_raw_handle();
        (*info).FileNameLength = (wide.len() * 2) as u32;
        let to = (info as *mut u8).add(head) as *mut u16;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), to, wide.len());
    }
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: a live handle opened with DELETE, a live folder handle in the
    // information, and `size` bytes of it.
    let result = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            info.cast(),
            size as u32,
            FileRenameInformation,
        )
    };
    if result < 0 {
        // SAFETY: converts a status code; no memory is touched.
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    use std::fs;

    /// A home folder of its own, symlinks resolved, with latch's state
    /// folder in it, as a host's workers see it.
    fn home() -> (tempfile::TempDir, WorkerSetup) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir(home.join(".latch")).unwrap();
        fs::write(home.join("latch"), "the latch this host runs").unwrap();
        let workers = WorkerSetup {
            hidden: crate::sandbox::never_readable(&home),
            state_dir: home.join(".latch"),
            program: home.join("latch"),
            home,
            areas: vec![],
            reads: vec![],
        };
        (dir, workers)
    }

    fn staged(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("staged");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_destination_is_resolved_on_the_host_before_judging() {
        let (_dir, workers) = home();
        let home = &workers.home;
        fs::create_dir_all(home.join("docs")).unwrap();
        std::os::unix::fs::symlink(home.join("docs"), home.join("linked")).unwrap();
        let resolve = |remote: &str| destination(remote, Some("report.pdf"), &workers);
        assert_eq!(resolve("~/docs/a.txt").unwrap(), home.join("docs/a.txt"));
        assert_eq!(resolve("docs/a.txt").unwrap(), home.join("docs/a.txt"));
        // A folder takes the file's name; a symlinked folder is resolved.
        assert_eq!(resolve("~/docs").unwrap(), home.join("docs/report.pdf"));
        assert_eq!(resolve("~/linked/a.txt").unwrap(), home.join("docs/a.txt"));
        let refused = |remote: &str| {
            let failure = resolve(remote).unwrap_err();
            (failure.exit, failure.code)
        };
        for hidden in ["~/.latch/key", "~/linked/../.latch/key", "/etc/hosts"] {
            assert_eq!(
                refused(hidden),
                (Exit::Denied, "bad_path".into()),
                "{hidden}"
            );
        }
        assert_eq!(refused("~/missing/a.txt").1, "not_found");
        // A link at the file's name inside a folder is followed and checked.
        fs::write(home.join(".latch/key"), "made-up key").unwrap();
        std::os::unix::fs::symlink(home.join(".latch/key"), home.join("docs/report.pdf")).unwrap();
        assert_eq!(refused("~/docs"), (Exit::Denied, "bad_path".into()));
        // The latch this host runs is never written over.
        assert_eq!(refused("~/latch"), (Exit::Denied, "bad_path".into()));
        // A folder with no plain name for the file is not guessed at.
        let sneaky = destination("~/docs", Some("../x"), &workers).unwrap_err();
        assert_eq!(sneaky.code, "is_a_directory");
        // A get resolves the same way, and refuses the same places.
        fs::write(home.join("docs/a.txt"), "a").unwrap();
        assert_eq!(
            source("~/linked/a.txt", &workers).unwrap(),
            home.join("docs/a.txt")
        );
        assert_eq!(
            source("~/.latch/key", &workers).unwrap_err().code,
            "bad_path"
        );
        // Outside home, a system file is a get's too, as exec can read it.
        assert_eq!(
            source("/etc/hosts", &workers).unwrap(),
            PathBuf::from("/etc/hosts").canonicalize().unwrap()
        );
    }

    #[test]
    fn a_folder_swapped_for_a_symlink_after_judging_gets_nothing() {
        let (dir, workers) = home();
        let home = &workers.home;
        fs::create_dir(home.join("docs")).unwrap();
        let path = destination("~/docs/key", None, &workers).unwrap();
        // After judging: the folder becomes a link into the state folder.
        fs::rename(home.join("docs"), home.join("docs-moved")).unwrap();
        std::os::unix::fs::symlink(home.join(".latch"), home.join("docs")).unwrap();
        let staged = staged(dir.path(), b"not a key");
        let refused = place(&staged, &path, &workers).unwrap_err();
        assert_eq!(refused.code, "destination_changed", "{refused:?}");
        assert_eq!(fs::read_dir(home.join(".latch")).unwrap().count(), 0);
        // A folder above it swapped is caught by the handle's own path.
        fs::create_dir_all(home.join("a/b")).unwrap();
        let path = destination("~/a/b/key", None, &workers).unwrap();
        fs::create_dir(home.join(".latch/b")).unwrap();
        fs::rename(home.join("a"), home.join("a-moved")).unwrap();
        std::os::unix::fs::symlink(home.join(".latch"), home.join("a")).unwrap();
        let refused = place(&staged, &path, &workers).unwrap_err();
        assert_eq!(refused.code, "destination_changed", "{refused:?}");
        assert_eq!(fs::read_dir(home.join(".latch/b")).unwrap().count(), 0);
    }

    #[test]
    fn a_put_replaces_the_file_whole_and_leaves_no_staging_file() {
        let (dir, workers) = home();
        let home = &workers.home;
        fs::create_dir(home.join("docs")).unwrap();
        fs::write(home.join("docs/a.txt"), "old").unwrap();
        let path = destination("~/docs/a.txt", None, &workers).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        place(&staged(dir.path(), b"new"), &path, &workers).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        // It keeps the file's permissions.
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = fs::read_dir(home.join("docs")).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // A staged file that is gone fails, and the old file stays.
        let refused = place(&dir.path().join("gone"), &path, &workers).unwrap_err();
        assert_eq!(refused.code, "io_error");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_dir(home.join("docs")).unwrap().count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_library_is_hidden_but_its_caches_are_not() {
        let (_dir, workers) = home();
        let home = &workers.home;
        fs::create_dir_all(home.join("Library/Caches")).unwrap();
        fs::create_dir_all(home.join("Library/Messages")).unwrap();
        let put = |remote: &str| destination(remote, None, &workers).map_err(|f| f.code);
        assert_eq!(put("~/Library/Messages/chat.db").unwrap_err(), "bad_path");
        assert!(put("~/Library/Caches/x").is_ok());
    }
}
