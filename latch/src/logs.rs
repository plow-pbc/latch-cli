//! `latch logs`: the host's diagnostics (`serve.log`), and with `--follow`
//! what is added to them, as it is. It never polls: it sleeps until the
//! kernel says the folder changed (`watch.rs`).

use std::io::{ErrorKind, Write};
use std::path::Path;

use crate::failure::Failure;
use crate::state::StateDir;
use crate::watch::{self, Watch, Woke};

/// `latch logs`: the host's log (`serve.log`); with `follow`, then what is
/// added to it, as it is.
pub fn run(state: &StateDir, follow: bool) -> Result<(), Failure> {
    let path = state.path.join("serve.log");
    let mut watch = start_watch(state, &path)?;
    let mut offset = 0;
    let mut out = std::io::stdout().lock();
    loop {
        watch.arm().map_err(|err| watch_failed(state, err))?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(Failure::failed(
                    "log_io",
                    format!("{}: {err}", path.display()),
                ));
            }
        };
        // A log that was started over is printed from its start.
        if bytes.len() < offset {
            offset = 0;
        }
        if out
            .write_all(&bytes[offset..])
            .and_then(|()| out.flush())
            .is_err()
        {
            return Ok(());
        }
        offset = bytes.len();
        if !follow || !sleep(&mut watch, state)? {
            return Ok(());
        }
    }
}

fn start_watch(state: &StateDir, file: &Path) -> Result<Watch, Failure> {
    state.create()?;
    let stdin = watch::stdin_is_a_pipe().then_some(libc::STDIN_FILENO);
    Watch::new(&state.path, &[file.to_path_buf()], stdin).map_err(|err| watch_failed(state, err))
}

/// Sleeps until the folder changes; false once whoever started us is gone.
fn sleep(watch: &mut Watch, state: &StateDir) -> Result<bool, Failure> {
    match watch.wait().map_err(|err| watch_failed(state, err))? {
        Woke::HungUp => Ok(false),
        Woke::Changed => Ok(true),
    }
}

fn watch_failed(state: &StateDir, err: std::io::Error) -> Failure {
    Failure::failed("watch_failed", format!("{}: {err}", state.path.display()))
}
