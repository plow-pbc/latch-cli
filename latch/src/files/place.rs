//! Where a `file put` lands, and putting it there.
//!
//! `destination` resolves the caller's path on the host before the put is
//! judged: `~` expanded, symlinks resolved, a folder given the file's name,
//! and the result refused if it is not under home or is in a hidden place.
//! The judge and the audit see that path.
//!
//! `place` moves the bytes the confined worker staged into it, outside the
//! sandbox. It opens the destination's folder once, checks that handle is
//! still the folder that was judged, and then works only relative to it: a
//! staging file beside the destination, written and synced, then renamed
//! over it. A folder swapped for a symlink after judging cannot redirect
//! the write, and a put that breaks off leaves the old file as it was.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::state;

/// Why a destination is refused, as a code and a message for the caller.
#[derive(Debug)]
pub struct Refused {
    pub code: &'static str,
    pub message: String,
}

impl From<Refused> for crate::worker::WorkerError {
    fn from(refused: Refused) -> Self {
        crate::worker::WorkerError {
            code: refused.code.to_string(),
            message: refused.message,
        }
    }
}

fn refused(code: &'static str, message: impl Into<String>) -> Refused {
    Refused {
        code,
        message: message.into(),
    }
}

/// What a put may never land in, besides everything outside `home`.
pub struct Places {
    /// The home folder, symlinks resolved.
    pub home: PathBuf,
    /// The host's own state folder; any other latch state folder is found
    /// by looking.
    pub state_dir: PathBuf,
}

impl Places {
    /// This user's home and the host's state folder, resolved now; `None`
    /// with no home folder.
    pub fn here(state_dir: &Path) -> Option<Places> {
        Some(Places {
            home: std::env::home_dir()?.canonicalize().ok()?,
            state_dir: state_dir.to_path_buf(),
        })
    }

    /// Why `path` (symlinks resolved) is not a place a put may write, if
    /// it is not: outside home, home itself, `~/.latch`, any latch state
    /// folder, or on macOS `~/Library` apart from `~/Library/Caches`.
    pub fn check(&self, path: &Path) -> Result<(), Refused> {
        let home = &self.home;
        if !path.starts_with(home) || path == home {
            return Err(refused(
                "outside_home",
                format!("{} is not inside the home folder", path.display()),
            ));
        }
        let library = home.join("Library");
        let hidden = path.starts_with(home.join(".latch"))
            || path.starts_with(&self.state_dir)
            || cfg!(target_os = "macos")
                && path.starts_with(&library)
                && !path.starts_with(library.join("Caches"))
            || path
                .ancestors()
                .take_while(|dir| *dir != home.as_path())
                .any(state::is_state_folder);
        if hidden {
            return Err(refused(
                "hidden_path",
                format!("{} is in a place latch never writes", path.display()),
            ));
        }
        Ok(())
    }
}

/// The path a put of the client's file `name` to `remote` writes. `remote`
/// may start with `~`; a relative one is under home.
pub fn destination(remote: &str, name: Option<&str>, places: &Places) -> Result<PathBuf, Refused> {
    let home = &places.home;
    let asked = match remote.strip_prefix('~') {
        Some("") => home.clone(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ if Path::new(remote).is_absolute() => PathBuf::from(remote),
        _ => home.join(remote),
    };
    // A folder takes the file under its own name, as `cp` does.
    let asked = if asked.is_dir() {
        let plain = name.filter(|name| Path::new(name).file_name() == Some(name.as_ref()));
        let Some(name) = plain else {
            return Err(refused(
                "is_a_directory",
                format!("{remote} is a folder, and the file has no plain name to go under"),
            ));
        };
        asked.join(name)
    } else {
        asked
    };
    let (Some(parent), Some(Component::Normal(file))) =
        (asked.parent(), asked.components().next_back())
    else {
        return Err(refused("bad_path", format!("{remote} names no file")));
    };
    let parent = parent
        .canonicalize()
        .map_err(|_| refused("not_found", format!("{}: no such folder", parent.display())))?;
    let path = parent.join(file);
    if path.is_dir() {
        return Err(refused(
            "is_a_directory",
            format!("{} is a folder", path.display()),
        ));
    }
    places.check(&path)?;
    Ok(path)
}

/// Moves the file at `staged` to `path`, which `destination` resolved.
pub fn place(staged: &Path, path: &Path, places: &Places) -> Result<(), Refused> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refused(
            "bad_path",
            format!("{} names no file", path.display()),
        ));
    };
    let failed = |err: io::Error| refused("io_error", format!("{}: {err}", path.display()));
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
    if fd_path(&dir).map_err(failed)? != folder {
        return Err(changed(folder));
    }
    places.check(path)?;

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

fn changed(folder: &Path) -> Refused {
    refused(
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

/// The path the kernel has for the folder `dir` is open on.
#[cfg(target_os = "macos")]
fn fd_path(dir: &File) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL-terminated, into
    // the buffer, which is that long.
    if unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    buffer.truncate(end);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

#[cfg(target_os = "linux")]
fn fd_path(dir: &File) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A home folder of its own, symlinks resolved, with a latch state
    /// folder in it.
    fn home() -> (tempfile::TempDir, Places) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir(home.join(".latch")).unwrap();
        let places = Places {
            state_dir: home.join(".latch"),
            home,
        };
        (dir, places)
    }

    fn staged(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("staged");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_destination_is_resolved_on_the_host_before_judging() {
        let (_dir, places) = home();
        let home = &places.home;
        fs::create_dir_all(home.join("docs")).unwrap();
        std::os::unix::fs::symlink(home.join("docs"), home.join("linked")).unwrap();
        let resolve = |remote: &str| destination(remote, Some("report.pdf"), &places);
        assert_eq!(resolve("~/docs/a.txt").unwrap(), home.join("docs/a.txt"));
        assert_eq!(resolve("docs/a.txt").unwrap(), home.join("docs/a.txt"));
        // A folder takes the file's name; a symlinked folder is resolved.
        assert_eq!(resolve("~/docs").unwrap(), home.join("docs/report.pdf"));
        assert_eq!(resolve("~/linked/a.txt").unwrap(), home.join("docs/a.txt"));
        let code = |remote: &str| resolve(remote).unwrap_err().code;
        assert_eq!(code("~/.latch/key"), "hidden_path");
        assert_eq!(code("~/linked/../.latch/key"), "hidden_path");
        assert_eq!(code("/etc/hosts"), "outside_home");
        assert_eq!(code("~/missing/a.txt"), "not_found");
        // A folder with no plain name for the file is not guessed at.
        assert_eq!(
            destination("~/docs", Some("../x"), &places)
                .unwrap_err()
                .code,
            "is_a_directory"
        );
    }

    #[test]
    fn a_folder_swapped_for_a_symlink_after_judging_gets_nothing() {
        let (dir, places) = home();
        let home = &places.home;
        fs::create_dir(home.join("docs")).unwrap();
        let path = destination("~/docs/key", None, &places).unwrap();
        // After judging: the folder becomes a link into the state folder.
        fs::rename(home.join("docs"), home.join("docs-moved")).unwrap();
        std::os::unix::fs::symlink(home.join(".latch"), home.join("docs")).unwrap();
        let staged = staged(dir.path(), b"not a key");
        let refused = place(&staged, &path, &places).unwrap_err();
        assert_eq!(refused.code, "destination_changed", "{refused:?}");
        assert_eq!(fs::read_dir(home.join(".latch")).unwrap().count(), 0);
        // A folder above it swapped is caught by the handle's own path.
        fs::create_dir_all(home.join("a/b")).unwrap();
        let path = destination("~/a/b/key", None, &places).unwrap();
        fs::create_dir(home.join(".latch/b")).unwrap();
        fs::rename(home.join("a"), home.join("a-moved")).unwrap();
        std::os::unix::fs::symlink(home.join(".latch"), home.join("a")).unwrap();
        let refused = place(&staged, &path, &places).unwrap_err();
        assert_eq!(refused.code, "destination_changed", "{refused:?}");
        assert_eq!(fs::read_dir(home.join(".latch/b")).unwrap().count(), 0);
    }

    #[test]
    fn a_put_replaces_the_file_whole_and_leaves_no_staging_file() {
        let (dir, places) = home();
        let home = &places.home;
        fs::create_dir(home.join("docs")).unwrap();
        fs::write(home.join("docs/a.txt"), "old").unwrap();
        let path = destination("~/docs/a.txt", None, &places).unwrap();
        place(&staged(dir.path(), b"new"), &path, &places).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let names: Vec<_> = fs::read_dir(home.join("docs")).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // A staged file that is gone fails, and the old file stays.
        let refused = place(&dir.path().join("gone"), &path, &places).unwrap_err();
        assert_eq!(refused.code, "io_error");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_dir(home.join("docs")).unwrap().count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_library_is_hidden_but_its_caches_are_not() {
        let (_dir, places) = home();
        let home = &places.home;
        fs::create_dir_all(home.join("Library/Caches")).unwrap();
        fs::create_dir_all(home.join("Library/Messages")).unwrap();
        let code = |remote: &str| {
            destination(remote, None, &places)
                .map(|_| "ok")
                .unwrap_or_else(|r| r.code)
        };
        assert_eq!(code("~/Library/Messages/chat.db"), "hidden_path");
        assert_eq!(code("~/Library/Caches/x"), "ok");
    }
}
