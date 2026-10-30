//! Browser profiles: each caller's one persistent profile per engine, in
//! the host's state directory, `<state-dir>/browser/agents/<principal>/<engine>/`.
//! The principal is the caller the gatekeeper judges: an agent by its
//! verified Plow uid, or `owner` for every one of the owner's own calls
//! (`gate::Caller::principal`). Chrome's and Firefox's data don't mix, so
//! an agent has one profile for each engine.
//!
//! A profile lasts across sessions, idle expiry and host restarts: its
//! cookies, site storage, cache and service workers are there for the
//! agent's next session; its tabs are not. Only one session at a time runs
//! on a profile (`host.rs`). A profile folder holds `data/`, the engine's
//! own data folder, and Camoufox's `fingerprint.json`. `exec` and file
//! workers are denied the whole state directory, profiles included.
//!
//! Folders an earlier latch kept, a session's own under
//! `<state-dir>/browser/sessions/` or a named one under
//! `<state-dir>/browser/profiles/`, are removed when a host starts.

use crate::state::OwnerOnly;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::failure::{Failure, code};

pub struct Profile {
    pub dir: PathBuf,
}

impl Profile {
    /// The engine's data folder (Chrome's `--user-data-dir`).
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }
}

/// `<state-dir>/browser/agents`.
pub fn root(state_dir: &Path) -> PathBuf {
    state_dir.join("browser/agents")
}

/// The profile folder of `principal` on `engine`.
pub fn of(state_dir: &Path, principal: &str, engine: &str) -> Result<PathBuf, Failure> {
    let plain = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if !plain(principal) || !plain(engine) {
        return Err(Failure::new(
            code::profile_failed,
            format!("no browser profile for {principal:?} on {engine:?}"),
        ));
    }
    Ok(root(state_dir).join(principal).join(engine))
}

/// Makes profile folder `dir`, private to this user, if it is new.
pub fn make(dir: PathBuf) -> Result<PathBuf, Failure> {
    fs::DirBuilder::new()
        .recursive(true)
        .owner_only()
        .create(&dir)
        .map_err(|err| Failure::new(code::profile_failed, format!("{}: {err}", dir.display())))?;
    Ok(dir)
}

/// Removes the profile folders an earlier latch kept.
pub fn reap(state_dir: &Path) {
    for retired in ["browser/sessions", "browser/profiles"] {
        let folder = state_dir.join(retired);
        if let Err(err) = fs::remove_dir_all(&folder)
            && err.kind() != io::ErrorKind::NotFound
        {
            crate::log::log(&format!(
                "browser: could not remove {}: {err}",
                folder.display()
            ));
        }
    }
}

/// The profile in folder `dir`, its engine's data folder made, with the
/// engine's password saving and autofill off at every launch (Camoufox
/// ships with them off). Chrome's other preferences, a site's settings
/// among them, are kept.
pub fn prepare(dir: &Path, engine: &str) -> Result<Profile, Failure> {
    let profile = Profile {
        dir: dir.to_path_buf(),
    };
    let made = (|| {
        fs::create_dir_all(profile.data())?;
        if engine == "cft" {
            fs::create_dir_all(profile.data().join("Default"))?;
            let file = profile.data().join("Default/Preferences");
            let mut preferences = fs::read(&file)
                .ok()
                .and_then(|kept| serde_json::from_slice::<Value>(&kept).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            for (key, value) in [
                ("credentials_enable_service", json!(false)),
                ("credentials_enable_autosignin", json!(false)),
            ] {
                preferences[key] = value;
            }
            preferences["profile"] = merged(
                &preferences["profile"],
                json!({"password_manager_enabled": false}),
            );
            preferences["autofill"] = merged(
                &preferences["autofill"],
                json!({"enabled": false, "profile_enabled": false, "credit_card_enabled": false}),
            );
            fs::write(&file, serde_json::to_vec(&preferences).expect("JSON"))?;
        }
        io::Result::Ok(())
    })();
    made.map_err(|err| {
        Failure::new(
            code::profile_failed,
            format!("{}: {err}", profile.dir.display()),
        )
    })?;
    Ok(profile)
}

/// `kept`'s keys, with `set`'s over them.
fn merged(kept: &Value, set: Value) -> Value {
    let mut out = kept.as_object().cloned().unwrap_or_default();
    out.extend(set.as_object().cloned().unwrap_or_default());
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn each_agent_keeps_one_private_profile_per_engine_and_old_folders_go() {
        let state = tempfile::tempdir().unwrap();
        let a = make(of(state.path(), "agent_a", "cft").unwrap()).unwrap();
        assert_eq!(
            a,
            make(of(state.path(), "agent_a", "cft").unwrap()).unwrap()
        );
        assert_ne!(a, of(state.path(), "agent_b", "cft").unwrap());
        assert_ne!(a, of(state.path(), "agent_a", "camoufox").unwrap());
        let mode = fs::metadata(&a).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "private to this user");
        for odd in ["", "..", "a/b"] {
            assert_eq!(
                of(state.path(), odd, "cft").unwrap_err().code,
                "profile_failed"
            );
        }

        // Chrome's own preferences stay; password saving and autofill are
        // set off again at every launch.
        let profile = prepare(&a, "cft").unwrap();
        let file = profile.data().join("Default/Preferences");
        fs::write(
            &file,
            r#"{"profile": {"content_settings": {"site": 1}, "password_manager_enabled": true}, "credentials_enable_service": true}"#,
        )
        .unwrap();
        prepare(&a, "cft").unwrap();
        let preferences: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(preferences["credentials_enable_service"], false);
        assert_eq!(preferences["profile"]["password_manager_enabled"], false);
        assert_eq!(preferences["profile"]["content_settings"]["site"], 1);
        assert_eq!(preferences["autofill"]["profile_enabled"], false);
        // A Camoufox profile gets no Chrome preferences.
        let fox = make(of(state.path(), "agent_a", "camoufox").unwrap()).unwrap();
        let fox = prepare(&fox, "camoufox").unwrap();
        assert!(!fox.data().join("Default").exists());

        let retired = [
            state.path().join("browser/sessions/xyz/data"),
            state.path().join("browser/profiles/default"),
        ];
        for folder in &retired {
            fs::create_dir_all(folder).unwrap();
        }
        reap(state.path());
        assert!(retired.iter().all(|folder| !folder.exists()));
        assert!(file.exists(), "an agent's profile stays");
    }
}
