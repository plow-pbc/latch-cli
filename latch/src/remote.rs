//! `status`, `audit show` and `events` with `--host`: another host's own
//! output, read over latch's connection as the `status`, `audit` and
//! `events` operations (see `serve/reads.rs`). The host answers its
//! owner's own devices only. A failed call never falls back to this
//! machine's own state; naming this machine reads it here.

use std::io::{Read, Write};
use std::time::Duration;

use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};

use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::Peer;
use crate::state::StateDir;
use crate::wire::Request;

/// The host `asked` names, unless that is this machine, or nothing.
pub async fn other_host(state: &StateDir, asked: Option<String>) -> Result<Option<Peer>, Failure> {
    if hosts::local_target(state, asked.as_deref())? {
        return Ok(None);
    }
    hosts::resolve(state, asked).await.map(Some)
}

fn request(op: &str, args: Value) -> Request {
    Request {
        op: op.to_string(),
        args,
        chat: None,
    }
}

/// One answer from `host`.
pub async fn call(state: &StateDir, host: &Peer, op: &str, args: Value) -> Result<Value, Failure> {
    dial::call(state, host, &request(op, args), ANSWER_WITHIN).await
}

/// Prints each line `host` streams, one JSON line each, as it comes: until
/// the host ends them, nobody reads our stdout, or we are asked to stop
/// (Ctrl-C, SIGTERM, or, when stdin is a pipe, its closing). Stopping
/// closes the connection at once, so the host stops reading for us too.
pub async fn stream(state: &StateDir, host: &Peer, op: &str, args: Value) -> Result<(), Failure> {
    let request = request(op, args);
    let link = dial::connect(state, host).await?;
    let printed = tokio::select! {
        printed = print_lines(&link, host, &request) => printed,
        () = stop_requested() => Ok(()),
    };
    link.close();
    printed
}

async fn print_lines(link: &dial::Link, host: &Peer, request: &Request) -> Result<(), Failure> {
    let connection = &link.connection;
    let (mut send, mut recv) = dial::start(connection, request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let mut out = std::io::stdout();
    // The first line comes at once; after it, a followed host may say
    // nothing for hours.
    let mut within = ANSWER_WITHIN;
    loop {
        let answer = match dial::answer(connection, &mut recv, within).await {
            Err(failure) if failure.code == "outcome_unknown" => Err(dropped(host)),
            answer => answer,
        };
        let Some(line) = answer?.get("line").cloned() else {
            return Ok(());
        };
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return Ok(());
        }
        within = Duration::MAX;
    }
}

/// A read lost midway changed nothing on the host: it can simply run again.
fn dropped(host: &Peer) -> Failure {
    let message = format!(
        "the connection to host {:?} dropped; run this again",
        host.name
    );
    Failure::new(Exit::Unreachable, "host_unreachable", message)
}

async fn stop_requested() {
    let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM can be handled");
    tokio::select! {
        () = stdin_closed() => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

/// Finishes once stdin, a pipe, is closed; never for a terminal or
/// `/dev/null`. Read on a thread of its own, which would not keep the
/// process from exiting as a runtime's blocking read would.
async fn stdin_closed() {
    if !crate::watch::stdin_is_a_pipe() {
        return std::future::pending().await;
    }
    let (closed, close) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 512];
        let mut stdin = std::io::stdin();
        while matches!(stdin.read(&mut buffer), Ok(read) if read > 0) {}
        let _ = closed.send(());
    });
    let _ = close.await;
}
