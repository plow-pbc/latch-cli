//! The host's own log: one time-stamped line per event, never tokens or
//! contents. It goes to stderr, where launchd and other service managers
//! collect it, except while the `serve` dashboard has the terminal: then
//! it goes to a file in the state directory (see `to_file`).

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The log file, once `to_file` has been called.
static FILE: OnceLock<Mutex<File>> = OnceLock::new();

pub fn log(message: &str) {
    let line = format!("{} {message}", jiff::Timestamp::now());
    match FILE.get() {
        Some(file) => {
            let _ = writeln!(file.lock().expect("log file lock"), "{line}");
        }
        None => eprintln!("{line}"),
    }
}

/// Sends every later log line to `path` (appending), not to stderr.
pub fn to_file(path: &Path) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let _ = FILE.set(Mutex::new(file));
    Ok(())
}
