//! Diagnostics for connection problems: `LATCH_TRACE` turns on the tracing
//! events of iroh and the other libraries latch is built on, filtered the
//! way `RUST_LOG` is (`LATCH_TRACE=iroh=debug,latch=info`). Off when unset.
//! A command's events go to `trace.log` in its state folder, so its stderr
//! stays its one error line; a foreground host's go to its stderr, which is
//! its log. Never to stdout, which carries the JSON. The audit log is not
//! affected. The installers a command runs write to `trace.log` too. A
//! host also logs its relay link's changes from iroh's events, set or not
//! (`relay_link.rs`).

use std::fs::{File, OpenOptions};
use std::process::Stdio;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::prelude::*;

use crate::failure::{Failure, code};
use crate::state::{OwnerOnly, StateDir};

/// The file in the state folder a command's events are appended to.
pub const FILE: &str = "trace.log";

/// Sets up where tracing events go: `LATCH_TRACE`'s, if set, and for a
/// host (`host`, which logs to its stderr) the relay events it logs itself
/// (`relay_link`).
pub fn init(state: &StateDir, host: bool) -> Result<(), Failure> {
    let filter = match std::env::var_os("LATCH_TRACE") {
        None => None,
        Some(filter) => {
            let filter = filter.to_string_lossy();
            Some(EnvFilter::builder().parse(&*filter).map_err(|err| {
                Failure::new(
                    code::bad_trace,
                    format!("LATCH_TRACE {filter:?} is not a filter: {err}"),
                )
            })?)
        }
    };
    if filter.is_none() && !host {
        return Ok(());
    }
    let events = match filter {
        None => None,
        Some(filter) => {
            let writer = if host {
                BoxMakeWriter::new(std::io::stderr)
            } else {
                BoxMakeWriter::new(Mutex::new(file(state)?))
            };
            let events = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                // A host operation's span says how long it took when it closes.
                .with_span_events(FmtSpan::CLOSE)
                .with_writer(writer);
            Some(events.with_filter(filter))
        }
    };
    tracing_subscriber::registry()
        .with(events)
        .with(host.then(crate::relay_link::layer))
        .init();
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
        .owner_only()
        .open(&path)
        .map_err(|err| Failure::new(code::state_io, format!("{}: {err}", path.display())))
}

/// Where a program a command runs writes, stdout and stderr both: `log`,
/// `trace.log` as `file` opened it.
pub fn output(log: &File) -> Result<(Stdio, Stdio), Failure> {
    let copy = || {
        log.try_clone()
            .map(Stdio::from)
            .map_err(|err| Failure::new(code::state_io, format!("{FILE}: {err}")))
    };
    Ok((copy()?, copy()?))
}

/// Runs Windows' own `program` (curl, tar, icacls), from System32, with
/// `args`, its output into `trace.log`, as the installers latch runs
/// elsewhere write there. A failure names it and that file.
#[cfg(windows)]
pub fn system_tool(
    state: &StateDir,
    program: &str,
    args: &[&std::ffi::OsStr],
) -> Result<(), Failure> {
    let path = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_default())
        .join("System32")
        .join(format!("{program}.exe"));
    let (out, err) = output(&file(state)?)?;
    let status = std::process::Command::new(path)
        .args(args)
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|err| Failure::new(code::setup_failed, format!("{program}: {err}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Failure::new(
        code::setup_failed,
        format!(
            "{program} failed ({status}); its messages are in {}",
            state.path.join(FILE).display()
        ),
    ))
}
