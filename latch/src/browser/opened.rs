//! The sessions opened on this state folder, by the hashes of their ids:
//! how a session a host restart ended is told from one that never was.
//!
//! `browser/opened/<hash>` holds the key of the caller that opened the
//! session. It is written as the session opens and removed when the
//! session ends while the host runs (`close`, idle expiry, its browser
//! gone, a failed launch). A host that stops leaves it, cleanly or not
//! (killed, a crash, a reboot), so once a host starts, a file with no live
//! session behind it names a session a restart ended. Each is kept 7 days,
//! as a request's journal is; then its id is no session at all.

use crate::state::OwnerOnly;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::journal::KEEP;
use crate::failure::{Failure, code};

fn folder(state_dir: &Path) -> PathBuf {
    state_dir.join("browser/opened")
}

/// Notes that the caller `owner` opened the session whose id hashes to `hash`.
pub fn record(state_dir: &Path, hash: &str, owner: &str) -> Result<(), Failure> {
    let folder = folder(state_dir);
    let written = fs::DirBuilder::new()
        .recursive(true)
        .owner_only()
        .create(&folder)
        .and_then(|()| {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .owner_only()
                .open(folder.join(hash))
        })
        .and_then(|mut file| file.write_all(owner.as_bytes()));
    written.map_err(|err| {
        Failure::new(
            code::io_error,
            format!("the session could not be recorded in the state folder: {err}"),
        )
    })
}

/// The session `hash` ended while the host ran: it is no restart's.
pub fn forget(state_dir: &Path, hash: &str) {
    let _ = fs::remove_file(folder(state_dir).join(hash));
}

/// The key of the caller that opened session `hash`, if it was opened in
/// the last 7 days and has not ended while a host ran. Asked only of an
/// id with no live session: such a session was ended by a restart.
pub fn ended(state_dir: &Path, hash: &str) -> Option<String> {
    let path = folder(state_dir).join(hash);
    let opened = fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
    if opened.elapsed().unwrap_or_default() > KEEP {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Removes the records of sessions opened more than 7 days ago.
pub fn prune(state_dir: &Path) {
    let Some(before) = SystemTime::now().checked_sub(KEEP) else {
        return;
    };
    let Ok(records) = fs::read_dir(folder(state_dir)) else {
        return;
    };
    for record in records.flatten() {
        let old = record
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at < before);
        if old {
            let _ = fs::remove_file(record.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_session_is_ended_until_forgotten_or_a_week_old() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        assert_eq!(ended(state, "abc"), None, "never opened");
        record(state, "abc", "caller-key").unwrap();
        assert_eq!(ended(state, "abc").as_deref(), Some("caller-key"));
        forget(state, "abc");
        assert_eq!(ended(state, "abc"), None, "closed while a host ran");

        record(state, "old", "caller-key").unwrap();
        record(state, "new", "caller-key").unwrap();
        let eight_days = std::time::Duration::from_secs(8 * 24 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(folder(state).join("old"))
            .unwrap()
            .set_modified(SystemTime::now() - eight_days)
            .unwrap();
        assert_eq!(ended(state, "old"), None, "older than 7 days");
        prune(state);
        assert!(!folder(state).join("old").exists());
        assert_eq!(ended(state, "new").as_deref(), Some("caller-key"));
    }
}
