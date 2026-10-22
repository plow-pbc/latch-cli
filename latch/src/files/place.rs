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

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::exec::{check_path, resolve};
use crate::failure::{Exit, Failure};
use crate::sandbox::opened_path;
use crate::worker::WorkerSetup;

/// The file a get of `remote` reads.
pub fn source(remote: &str, workers: &WorkerSetup) -> Result<PathBuf, Failure> {
    let path = resolve(&workers.home, Path::new(remote))?;
    check_path(workers, &path, false)?;
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
            return Err(Failure::failed(
                "is_a_directory",
                format!("{remote} is a folder, and the file has no plain name to go under"),
            ));
        };
        path.push(name);
        if path.is_dir() {
            let message = format!("{} is a folder", path.display());
            return Err(Failure::failed("is_a_directory", message));
        }
    }
    if !path.parent().is_some_and(Path::is_dir) {
        let message = format!("{remote}: no folder to put it in");
        return Err(Failure::failed("not_found", message));
    }
    check_path(workers, &path, true)?;
    Ok(path)
}

/// Moves the file at `staged` to `path`, which `destination` resolved.
pub fn place(staged: &Path, path: &Path, workers: &WorkerSetup) -> Result<(), Failure> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        let message = format!("{} names no file", path.display());
        return Err(Failure::failed("bad_path", message));
    };
    let failed = |err: io::Error| Failure::failed("io_error", format!("{}: {err}", path.display()));
    // `O_NOFOLLOW`: a folder that became a symlink is not opened at all.
    let dir = open(
        None,
        folder.as_os_str().as_bytes(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
    .map_err(|_| changed(folder))?;
    // A folder above it swapped for a symlink would still be followed:
    // the handle must be the folder that was judged, and pass again.
    if opened_path(&dir).map_err(failed)? != folder {
        return Err(changed(folder));
    }
    check_path(workers, path, true)?;

    let temp = format!(
        ".{}.latch-{}-{:08x}.part",
        name.to_string_lossy(),
        std::process::id(),
        rand::random::<u32>()
    );
    // `O_EXCL | O_NOFOLLOW`: never an existing name, nor a link at it.
    let mut file = open(
        Some(&dir),
        temp.as_bytes(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o666,
    )
    .map_err(failed)?;
    let written = File::open(staged)
        .and_then(|mut staged| io::copy(&mut staged, &mut file))
        .and_then(|_| file.sync_all())
        .and_then(|()| rename(&dir, &temp, name.as_bytes()));
    if let Err(err) = written {
        let _ = unlink(&dir, &temp);
        return Err(failed(err));
    }
    Ok(())
}

fn changed(folder: &Path) -> Failure {
    Failure::new(
        Exit::Denied,
        "destination_changed",
        format!(
            "{} changed after the put was checked; nothing was written",
            folder.display()
        ),
    )
}

fn c_name(name: &[u8]) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `openat(dir, name, flags)`, or `open` with no `dir`; close-on-exec.
fn open(dir: Option<&File>, name: &[u8], flags: i32, mode: libc::c_uint) -> io::Result<File> {
    let name = c_name(name)?;
    let at = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `name` is a NUL-terminated string that outlives the call, and
    // `at` is a descriptor we hold open (or AT_FDCWD).
    let fd = unsafe { libc::openat(at, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat just returned this descriptor, and nothing else owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn rename(dir: &File, from: &str, to: &[u8]) -> io::Result<()> {
    let (from, to) = (c_name(from.as_bytes())?, c_name(to)?);
    let fd = dir.as_raw_fd();
    // SAFETY: both names are NUL-terminated and outlive the call; `fd` is open.
    if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn unlink(dir: &File, name: &str) -> io::Result<()> {
    let name = c_name(name.as_bytes())?;
    // SAFETY: `name` is NUL-terminated and outlives the call; `dir` is open.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A home folder of its own, symlinks resolved, with latch's state
    /// folder in it, as a host's workers see it.
    fn home() -> (tempfile::TempDir, WorkerSetup) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir(home.join(".latch")).unwrap();
        let workers = WorkerSetup {
            hidden: crate::sandbox::never_readable(&home),
            state_folders: crate::state_folders::StateFolders::none(),
            state_dir: home.join(".latch"),
            home,
            areas: vec![],
            reads: vec![],
            program: PathBuf::from("/nonexistent"),
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
        place(&staged(dir.path(), b"new"), &path, &workers).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
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
