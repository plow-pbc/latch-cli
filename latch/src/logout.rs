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
//! The folder is resolved once, and must be a latch state folder, not a
//! link to one. From the stop on, logout holds `serve.lock` as a host
//! would, so no host starts until it is done.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::failure::Failure;
use crate::lifecycle;
use crate::managed;
use crate::secrets::{Secret, Secrets};
use crate::state::StateDir;

/// How long logout waits for a `latch plugins` command to let go of the
/// folder; a host never does, and is refused at once.
const HOLD_WITHIN: Duration = Duration::from_secs(60);

pub async fn run(state: &StateDir, purge: bool) -> Result<Value, Failure> {
    let refused = |why: &str| {
        let message = format!("{} {why}; nothing was deleted", state.path.display());
        Failure::failed("state_format", message)
    };
    match fs::symlink_metadata(&state.path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(
                json!({"removed": [], "stopped": false, "purged": false, "state_dir": state.path}),
            );
        }
        Err(err) => return Err(io_failure(&state.path, err)),
        Ok(found) if found.file_type().is_symlink() => {
            return Err(refused("is a link; name the state folder itself"));
        }
        Ok(_) => {}
    }
    let path = state
        .path
        .canonicalize()
        .map_err(|err| io_failure(&state.path, err))?;
    if !path.join("latch.json").is_file() {
        return Err(refused(
            "has no latch.json, so it is not a latch state folder",
        ));
    }
    let stopped = managed::stop(&StateDir { path: path.clone() }).await?;
    let serving = |holder: &str| holder.starts_with("latch serve");
    let taken =
        crate::serve::hold_state(&path, lifecycle::LOGGING_OUT, HOLD_WITHIN, |h| !serving(h))?;
    let _lock = match taken {
        Ok(lock) => lock,
        Err(holder) if serving(&holder) => {
            let message = format!(
                "{holder} started on {} as it was stopped; `latch stop`, then log out again",
                path.display()
            );
            return Err(Failure::failed("host_running", message));
        }
        Err(holder) => {
            let message = format!("{holder} still holds this state folder; run this again");
            return Err(Failure::failed("state_busy", message));
        }
    };
    held_for_tests();
    let secrets = Secrets::open(&path)?;
    let mut removed = secrets.remove_each(if purge { &Secret::ALL } else { &Secret::LOGIN })?;
    if purge {
        fs::remove_dir_all(&path).map_err(|err| io_failure(&path, err))?;
    } else {
        let config = path.join("config.json");
        match fs::remove_file(&config) {
            Ok(()) => removed.push("config.json"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_failure(&config, err)),
        }
    }
    Ok(json!({
        "removed": removed,
        "from": if secrets.in_keychain() { "keychain" } else { "files" },
        "stopped": stopped["stopped"],
        "purged": purge,
        "state_dir": path,
    }))
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

fn io_failure(path: &Path, err: io::Error) -> Failure {
    Failure::failed("state_io", format!("{}: {err}", path.display()))
}
