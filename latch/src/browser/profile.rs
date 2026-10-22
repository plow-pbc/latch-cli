//! Session profiles: the engine's data for one session, in a folder of its
//! own in the host's state directory, `<state-dir>/browser/sessions/<name>/`,
//! its name random and never derived from the session's id. The host makes
//! one for each `open` and removes it when the session's runtime ends
//! (`runtime.rs`); a host starting removes any an earlier one left. A
//! profile folder holds `data/`, the engine's own data folder. `exec` and file
//! workers are denied the whole state directory, profiles included.
//!
//! Folders an earlier latch kept under `<state-dir>/browser/profiles/` are
//! never opened, nor removed.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use crate::failure::Failure;

pub struct Profile {
    pub dir: PathBuf,
}

impl Profile {
    /// The engine's data folder (Chrome's `--user-data-dir`).
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }
}

/// `<state-dir>/browser/sessions`.
pub fn root(state_dir: &Path) -> PathBuf {
    state_dir.join("browser/sessions")
}

/// A new, empty profile folder for one session, private to this user, and
/// removed when the answer is dropped.
pub fn make(state_dir: &Path) -> io::Result<TempDir> {
    let root = root(state_dir);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    tempfile::Builder::new()
        .prefix("")
        .rand_bytes(22)
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(root)
}

/// Removes the profiles an earlier host left: none of its sessions is open
/// in this one.
pub fn reap(state_dir: &Path) {
    let root = root(state_dir);
    if let Err(err) = fs::remove_dir_all(&root)
        && err.kind() != io::ErrorKind::NotFound
    {
        crate::log::log(&format!(
            "browser: could not remove {}: {err}",
            root.display()
        ));
    }
}

/// The profile in folder `dir`, its engine's data folder made, with the
/// engine's password saving and autofill off from the first launch
/// (Camoufox ships with them off).
pub fn prepare(dir: &Path, engine: &str) -> Result<Profile, Failure> {
    let profile = Profile {
        dir: dir.to_path_buf(),
    };
    let made = (|| {
        fs::create_dir_all(profile.data())?;
        if engine == "cft" {
            fs::create_dir_all(profile.data().join("Default"))?;
            let preferences = json!({
                "credentials_enable_service": false,
                "credentials_enable_autosignin": false,
                "profile": {"password_manager_enabled": false},
                "autofill": {"enabled": false, "profile_enabled": false, "credit_card_enabled": false},
            });
            fs::write(
                profile.data().join("Default/Preferences"),
                serde_json::to_vec(&preferences).expect("JSON"),
            )?;
        }
        io::Result::Ok(())
    })();
    made.map_err(|err| {
        Failure::failed(
            "profile_failed",
            format!("{}: {err}", profile.dir.display()),
        )
    })?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_session_gets_a_private_profile_gone_when_dropped_or_at_the_next_host() {
        let state = tempfile::tempdir().unwrap();
        let a = make(state.path()).unwrap();
        let b = make(state.path()).unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().file_name().unwrap().len() >= 22);
        let mode = fs::metadata(a.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "private to this user");

        let profile = prepare(a.path(), "cft").unwrap();
        let preferences: serde_json::Value =
            serde_json::from_slice(&fs::read(profile.data().join("Default/Preferences")).unwrap())
                .unwrap();
        assert_eq!(preferences["credentials_enable_service"], false);
        assert_eq!(preferences["autofill"]["profile_enabled"], false);
        // A Camoufox profile gets no Chrome preferences.
        let fox = prepare(b.path(), "camoufox").unwrap();
        assert!(!fox.data().join("Default").exists());

        let (a_path, b_path) = (a.path().to_path_buf(), b.path().to_path_buf());
        drop(a);
        assert!(!a_path.exists());
        // One a host that stopped without closing left: the next one removes it.
        let left = b.keep();
        let old = state.path().join("browser/profiles/default");
        fs::create_dir_all(&old).unwrap();
        reap(state.path());
        assert!(!left.exists() && !b_path.exists());
        assert!(old.exists(), "an earlier latch's profiles stay");
    }
}
