//! `latch logout`: once any host on this state folder has stopped, its
//! identity and login are deleted (its key and Plow token), from the
//! Keychain on a signed macOS latch and as files on any other, and with
//! them `config.json`, the record that this folder is logged in. The vault
//! and audit keys stay, so after `latch login` the vault and audit log
//! read as before. `--purge` deletes those two keys as well, and then the
//! state folder itself: the flag is the consent. This machine stays in
//! Plow's directory: `latch devices remove` from another of the owner's
//! machines takes it out.
//!
//! The folder is resolved once, and held open: a path that names another
//! folder by the time the host has stopped (moved, or replaced by a link)
//! is refused, and nothing is deleted through it. From the stop on, logout
//! holds `serve.lock` as a host would, so no host starts until it is done.

use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::failure::{Exit, Failure};
use crate::lifecycle;
use crate::managed;
use crate::secrets::{Secret, Secrets};
use crate::state::StateDir;

/// How long logout waits for a `latch plugins` command to let go of the
/// folder; a host never does, and is refused at once.
const HOLD_WITHIN: Duration = Duration::from_secs(60);

pub async fn run(state: &StateDir, purge: bool) -> Result<Value, Failure> {
    let Some(folder) = Folder::pin(&state.path)? else {
        return Ok(
            json!({"removed": [], "stopped": false, "purged": false, "state_dir": state.path}),
        );
    };
    // Only a latch state folder is ever deleted whole.
    if purge && folder.stat("latch.json").is_err() {
        let message = format!(
            "{} has no latch.json, so it is not a latch state folder; nothing was deleted",
            folder.path.display()
        );
        return Err(Failure::failed("state_format", message));
    }
    let stopped = managed::stop(&StateDir {
        path: folder.path.clone(),
    })
    .await?;
    let serving = |holder: &str| holder.starts_with("latch serve");
    let held = crate::serve::hold_state(&folder.path, lifecycle::LOGGING_OUT, HOLD_WITHIN, |h| {
        !serving(h)
    })?;
    let lock = match held {
        Ok(lock) => lock,
        Err(holder) if serving(&holder) => {
            let message = format!(
                "{holder} started on {} as it was stopped; `latch stop`, then log out again",
                folder.path.display()
            );
            return Err(Failure::failed("host_running", message));
        }
        Err(holder) => {
            let message = format!("{holder} still holds this state folder; run this again");
            return Err(Failure::failed("state_busy", message));
        }
    };
    // The lock taken, and the folder about to change, are the ones pinned.
    folder.unchanged(&state.path)?;
    if !same(&lock.metadata(), &folder.stat("serve.lock")) {
        return Err(folder.changed());
    }
    held_for_tests();
    let secrets = Secrets::open_held(&folder.path, folder.dir.try_clone().map_err(io_failure)?)?;
    let mut removed = secrets.remove_each(if purge { &Secret::ALL } else { &Secret::LOGIN })?;
    if purge {
        folder.delete()?;
    } else {
        match crate::secrets::unlink_at(&folder.dir, "config.json") {
            Ok(()) => removed.push("config.json"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_failure(err)),
        }
    }
    drop(lock);
    Ok(json!({
        "removed": removed,
        "from": if secrets.in_keychain() { "keychain" } else { "files" },
        "stopped": stopped["stopped"],
        "purged": purge,
        "state_dir": folder.path,
    }))
}

/// The state folder as first resolved: its real path, the folder and its
/// parent held open, and its identity on disk.
struct Folder {
    path: PathBuf,
    dir: File,
    parent: File,
    name: CString,
    id: (u64, u64),
}

impl Folder {
    /// The folder `given` names now, or `None` if there is none.
    fn pin(given: &Path) -> Result<Option<Folder>, Failure> {
        let path = match given.canonicalize() {
            Ok(path) => path,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io_failure(err)),
        };
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            let message = format!("{} cannot be a state folder", path.display());
            return Err(Failure::usage("bad_state_dir", message));
        };
        let dir = File::open(&path).map_err(io_failure)?;
        let id = identity(&dir.metadata().map_err(io_failure)?);
        Ok(Some(Folder {
            parent: File::open(parent).map_err(io_failure)?,
            name: CString::new(name.as_bytes()).map_err(|_| io_failure(io::Error::other("NUL")))?,
            dir,
            path,
            id,
        }))
    }

    /// Refuses unless `given` still names the folder pinned, and its name
    /// in its parent is still it.
    fn unchanged(&self, given: &Path) -> Result<(), Failure> {
        let now = given.canonicalize().ok();
        let named = stat_at(&self.parent, &self.name).ok();
        if now.as_ref() != Some(&self.path)
            || named.map(|named| identity_of(&named)) != Some(self.id)
        {
            return Err(self.changed());
        }
        Ok(())
    }

    fn changed(&self) -> Failure {
        let message = format!(
            "{} is no longer the state folder it was when logout began (it was moved or \
             replaced); nothing was deleted",
            self.path.display()
        );
        Failure::new(Exit::Failed, "state_changed", message)
    }

    /// The file `name` in the folder itself, not following a link.
    fn stat(&self, name: &str) -> io::Result<libc::stat> {
        stat_at(&self.dir, &CString::new(name).map_err(io::Error::other)?)
    }

    /// Deletes the folder: first moved, by its name in its parent, to a
    /// name of logout's own, and deleted there only if what moved is it.
    fn delete(&self) -> Result<(), Failure> {
        let aside = format!(
            ".latch-logout-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        );
        let aside_c = CString::new(aside.as_str()).expect("no NUL");
        rename_at(&self.parent, &self.name, &aside_c).map_err(io_failure)?;
        let moved = stat_at(&self.parent, &aside_c).map_err(io_failure)?;
        if identity_of(&moved) != self.id {
            // Not the folder pinned: put it back, delete nothing.
            rename_at(&self.parent, &aside_c, &self.name).map_err(io_failure)?;
            return Err(self.changed());
        }
        let aside = self.path.with_file_name(aside);
        fs::remove_dir_all(&aside)
            .map_err(|err| Failure::failed("state_io", format!("{}: {err}", aside.display())))
    }
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

#[allow(clippy::unnecessary_cast)]
fn identity_of(stat: &libc::stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino as u64)
}

/// Whether the lock file held and the folder's `serve.lock` are one file.
fn same(held: &io::Result<fs::Metadata>, named: &io::Result<libc::stat>) -> bool {
    match (held, named) {
        (Ok(held), Ok(named)) => identity(held) == identity_of(named),
        _ => false,
    }
}

/// `name` in the open folder `dir`, not following a link.
fn stat_at(dir: &File, name: &CString) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: an open folder, a NUL-terminated name, and room for the result.
    let code = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if code != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat filled it in.
    Ok(unsafe { stat.assume_init() })
}

/// Renames `from` to `to`, both in the open folder `dir`.
fn rename_at(dir: &File, from: &CString, to: &CString) -> io::Result<()> {
    let fd = dir.as_raw_fd();
    // SAFETY: an open folder and two NUL-terminated names.
    if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// With `LATCH_TEST_LOGOUT_HELD` naming a file, a test build writes it
/// once logout holds the folder, and waits until the test deletes it.
fn held_for_tests() {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(file) = std::env::var_os("LATCH_TEST_LOGOUT_HELD") else {
        return;
    };
    let _ = fs::write(&file, "held");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while Path::new(&file).exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn io_failure(err: io::Error) -> Failure {
    Failure::failed("state_io", err.to_string())
}
