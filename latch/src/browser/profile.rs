//! Profiles: latch-owned browser data folders in the host's state
//! directory, `<state-dir>/browser/profiles/<name>/`, each bound to one
//! engine for life. A profile folder holds `profile.json` (the binding),
//! `lock` (held by the runtime using it), `data/`, the engine's own data
//! folder, and `agent-code.json`, the sites agent code ran on. `exec` and
//! file workers are denied the whole state directory, profiles included.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::failure::Failure;

/// How long `lock` waits for a lock that may be on its way out: a lock
/// dropped a moment ago is still held by any process another thread was
/// starting at the time, through its copy of the descriptor, until it execs.
const LOCK_WAIT: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
struct Binding {
    engine: String,
}

pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
}

impl Profile {
    /// The engine's data folder (Chrome's `--user-data-dir`).
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("lock")
    }
}

/// The sites agent code (`eval`) has run on in a profile, in any session,
/// kept as `agent-code.json`: service workers and site storage outlive
/// sessions, so what a script left on a site may still be there when a
/// value is filled there.
const AGENT_CODE: &str = "agent-code.json";

pub fn agent_code_sites(state_dir: &Path, name: &str) -> BTreeSet<String> {
    fs::read(root(state_dir).join(name).join(AGENT_CODE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Replaces the profile's agent-code sites with `sites`, atomically.
pub fn keep_agent_code_sites(
    state_dir: &Path,
    name: &str,
    sites: &BTreeSet<String>,
) -> io::Result<()> {
    let path = root(state_dir).join(name).join(AGENT_CODE);
    let temp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec(sites).expect("JSON"))?;
    fs::rename(&temp, &path)
}

/// `<state-dir>/browser/profiles`.
pub fn root(state_dir: &Path) -> PathBuf {
    state_dir.join("browser/profiles")
}

/// The profile `name` for `engine`, created on first use. A profile made
/// for another engine is refused: an engine never reads another's data.
pub fn open(state_dir: &Path, name: &str, engine: &str) -> Result<Profile, Failure> {
    if !valid_name(name) {
        return Err(Failure::usage(
            "bad_profile",
            format!("{name:?} is not a profile name: use 1-32 of a-z, 0-9 and -"),
        ));
    }
    let profile = Profile {
        name: name.to_string(),
        dir: root(state_dir).join(name),
    };
    let binding = profile.dir.join("profile.json");
    let failed = |err: io::Error| {
        Failure::failed(
            "profile_failed",
            format!("{}: {err}", profile.dir.display()),
        )
    };
    match fs::read(&binding) {
        Ok(bytes) => {
            let bound: Binding = serde_json::from_slice(&bytes).map_err(|err| {
                Failure::failed("profile_failed", format!("{}: {err}", binding.display()))
            })?;
            if bound.engine != engine {
                return Err(Failure::usage(
                    "profile_engine_mismatch",
                    format!("profile {name} belongs to {}, not {engine}", bound.engine),
                ));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            create(&profile, engine).map_err(failed)?;
        }
        Err(err) => return Err(failed(err)),
    }
    Ok(profile)
}

/// The engine profile `name` is bound to; None if there is no such profile.
pub fn engine_of(state_dir: &Path, name: &str) -> Result<Option<String>, Failure> {
    let binding = root(state_dir).join(name).join("profile.json");
    match fs::read(&binding) {
        Ok(bytes) => serde_json::from_slice::<Binding>(&bytes)
            .map(|bound| Some(bound.engine))
            .map_err(|err| {
                Failure::failed("profile_failed", format!("{}: {err}", binding.display()))
            }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Failure::failed(
            "profile_failed",
            format!("{}: {err}", binding.display()),
        )),
    }
}

/// A new profile folder, private to this user, with the engine's password
/// saving and autofill off from the first launch (Camoufox ships with
/// them off). The binding is written last, so a half-made profile is made
/// again rather than used.
fn create(profile: &Profile, engine: &str) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(profile.data())?;
    if super::engine::cdp_pin(engine).is_some() {
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
    let binding = serde_json::to_vec(&Binding {
        engine: engine.to_string(),
    })
    .expect("JSON");
    fs::write(profile.dir.join("profile.json"), binding)
}

pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Holds a profile for one session: an exclusive lock on its `lock` file,
/// released when this is dropped or the process ends.
pub struct Lock(#[allow(dead_code)] File);

pub fn lock(profile: &Profile) -> Result<Lock, Failure> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(profile.lock_path())
        .map_err(|err| Failure::failed("profile_failed", err.to_string()))?;
    let deadline = Instant::now() + LOCK_WAIT;
    // SAFETY: flock only reads the descriptor, which `file` keeps open.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        if Instant::now() >= deadline {
            return Err(Failure::failed(
                "profile_busy",
                format!("profile {} is in use by another session", profile.name),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(Lock(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_is_made_once_bound_to_its_engine_and_locked_by_one_session() {
        let state = tempfile::tempdir().unwrap();
        let profile = open(state.path(), "default", "cft").unwrap();
        let preferences: serde_json::Value =
            serde_json::from_slice(&fs::read(profile.data().join("Default/Preferences")).unwrap())
                .unwrap();
        assert_eq!(preferences["credentials_enable_service"], false);
        assert_eq!(preferences["autofill"]["profile_enabled"], false);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&profile.dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "private to this user");

        assert_eq!(
            open(state.path(), "default", "camoufox")
                .err()
                .unwrap()
                .code,
            "profile_engine_mismatch"
        );
        assert_eq!(
            open(state.path(), "../x", "cft").err().unwrap().code,
            "bad_profile"
        );

        // A Camoufox profile gets no Chrome preferences; its binding is read back.
        let fox = open(state.path(), "fox", "camoufox").unwrap();
        assert!(!fox.data().join("Default").exists());
        assert_eq!(
            engine_of(state.path(), "fox").unwrap().as_deref(),
            Some("camoufox")
        );
        assert_eq!(engine_of(state.path(), "none").unwrap(), None);

        let held = lock(&profile).unwrap();
        assert_eq!(lock(&profile).err().unwrap().code, "profile_busy");
        drop(held);
        lock(&profile).unwrap();
    }

    #[test]
    fn a_dropped_lock_is_free_while_other_threads_start_processes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let state = tempfile::tempdir().unwrap();
        let profile = open(state.path(), "default", "cft").unwrap();
        // A process starting meanwhile holds a copy of every descriptor,
        // the lock's included, until it execs.
        let done = AtomicBool::new(false);
        let busy = std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    while !done.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("/bin/sh")
                            .args(["-c", ":"])
                            .status();
                    }
                });
            }
            let busy = (0..2000).filter(|_| lock(&profile).is_err()).count();
            done.store(true, Ordering::Relaxed);
            busy
        });
        assert_eq!(busy, 0, "a dropped lock was still held");
    }
}
