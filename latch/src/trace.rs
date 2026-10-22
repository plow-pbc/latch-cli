//! Diagnostics for connection problems: `LATCH_TRACE` turns on the tracing
//! events of iroh and the other libraries latch is built on, filtered the
//! way `RUST_LOG` is (`LATCH_TRACE=iroh=debug,latch=info`). Off when unset.
//! A command's events go to `trace.log` in its state folder, so its stderr
//! stays its one error line; a foreground host's go to its stderr, which is
//! its log. Never to stdout, which carries the JSON. The audit log is not
//! affected. The installers a command runs write to `trace.log` too.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

use crate::failure::Failure;
use crate::state::StateDir;

/// The file in the state folder a command's events are appended to.
pub const FILE: &str = "trace.log";

pub fn init(state: &StateDir, to_stderr: bool) -> Result<(), Failure> {
    let Some(filter) = std::env::var_os("LATCH_TRACE") else {
        return Ok(());
    };
    let filter = filter.to_string_lossy();
    let filter = EnvFilter::builder().parse(&*filter).map_err(|err| {
        Failure::usage(
            "bad_trace",
            format!("LATCH_TRACE {filter:?} is not a filter: {err}"),
        )
    })?;
    let events = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        // A host operation's span says how long it took when it closes.
        .with_span_events(FmtSpan::CLOSE);
    if to_stderr {
        events.with_writer(std::io::stderr).init();
        return Ok(());
    }
    events.with_writer(Mutex::new(file(state)?)).init();
    Ok(())
}

/// `trace.log` in `state`, opened to append, made (with the folder) if
/// missing.
pub fn file(state: &StateDir) -> Result<File, Failure> {
    state.create()?;
    let path = state.path.join(FILE);
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| Failure::failed("state_io", format!("{}: {err}", path.display())))
}

/// Where a program a command runs writes, stdout and stderr both: `log`,
/// `trace.log` as `file` opened it.
pub fn output(log: &File) -> Result<(Stdio, Stdio), Failure> {
    let copy = || {
        log.try_clone()
            .map(Stdio::from)
            .map_err(|err| Failure::failed("state_io", format!("{FILE}: {err}")))
    };
    Ok((copy()?, copy()?))
}
