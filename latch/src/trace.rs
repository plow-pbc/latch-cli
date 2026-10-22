//! Diagnostics for connection problems: `LATCH_TRACE` turns on the tracing
//! events of iroh and the other libraries latch is built on, filtered the
//! way `RUST_LOG` is (`LATCH_TRACE=iroh=debug,latch=info`). Off when unset.
//! The events go where the host's log goes (stderr), never to stdout,
//! which carries the JSON. The audit
//! log is not affected.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

use crate::failure::Failure;

pub fn init() -> Result<(), Failure> {
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
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        // A host operation's span says how long it took when it closes.
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .init();
    Ok(())
}
