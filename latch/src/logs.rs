//! `latch logs`: the host's diagnostics (`serve.log`), a `log` line per
//! record, and with `--follow` each one added. It sleeps until the kernel says the
//! folder changed (`watch.rs`), and on Windows, which says nothing of writes to a
//! file held open (a host holds its log for its whole run), looks again each second.

use std::io::{ErrorKind, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::failure::{Failure, code};
use crate::state::StateDir;
use crate::watch::{self, Watch, Woke};

/// `latch logs`: the host's log (`serve.log`), a `log` line per record
/// (each line the host wrote); with `follow`, then each one added.
pub fn run(state: &StateDir, follow: bool) -> Result<(), Failure> {
    let path = state.path.join("serve.log");
    let mut watch = start_watch(state, &path)?;
    let mut offset = 0;
    let mut out = std::io::stdout().lock();
    if !emit(&mut out, &crate::stream::hello()) {
        return Ok(());
    }
    loop {
        watch.arm().map_err(|err| watch_failed(state, err))?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(Failure::new(
                    code::log_io,
                    format!("{}: {err}", path.display()),
                ));
            }
        };
        // A log that was started over is printed from its start.
        if bytes.len() < offset {
            offset = 0;
        }
        // Whole records only: a line still being written waits for its end.
        let written = bytes[offset..]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |end| end + 1);
        for record in bytes[offset..offset + written].split(|&b| b == b'\n') {
            if record.is_empty() {
                continue;
            }
            let line = json!({"type": "log", "line": String::from_utf8_lossy(record)});
            if !emit(&mut out, &line) {
                return Ok(());
            }
        }
        offset += written;
        if !follow || !sleep(&mut watch, state)? {
            return Ok(());
        }
    }
}

fn start_watch(state: &StateDir, file: &Path) -> Result<Watch, Failure> {
    state.create()?;
    Watch::new(&state.path, &[file.to_path_buf()], watch::stdin_hangup())
        .map_err(|err| watch_failed(state, err))
}

/// Sleeps until the folder changes; false once whoever started us is gone.
fn sleep(watch: &mut Watch, state: &StateDir) -> Result<bool, Failure> {
    #[cfg(windows)]
    let woke = watch
        .wait_for(Some(std::time::Duration::from_secs(1)))
        .map_err(|err| watch_failed(state, err))?
        .unwrap_or(Woke::Changed);
    #[cfg(not(windows))]
    let woke = watch.wait().map_err(|err| watch_failed(state, err))?;
    match woke {
        Woke::HungUp => Ok(false),
        Woke::Changed => Ok(true),
    }
}

fn watch_failed(state: &StateDir, err: std::io::Error) -> Failure {
    Failure::new(
        code::watch_failed,
        format!("{}: {err}", state.path.display()),
    )
}

/// Writes one line; false once nobody reads our stdout.
fn emit(out: &mut impl Write, line: &Value) -> bool {
    writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
}
