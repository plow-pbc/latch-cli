//! The `status`, `audit` and `events` operations: what this host's owner
//! reads from another machine with `--host`, the same output as `latch
//! status --local`, `audit show [--follow]` and `events` give on the host
//! itself. From the owner's own devices only; an agent's key gets exit 4
//! `owner_only`.
//!
//! `audit` with `follow` and `events` stream: each batch of lines, the
//! opening first, goes to the caller as one `{"ok": {"lines": [...]}}`
//! frame, until the lines end, the caller leaves or this host stops; then
//! the final answer (see `wire.rs`).

use std::future::Future;
use std::os::fd::{AsRawFd, RawFd};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::Host;
use crate::cli::{AuditAction, EventsArgs};
use crate::failure::{Exit, Failure};
use crate::serve::Done;
use crate::state::StateDir;
use crate::transport::SendStream;
use crate::wire::{self, Response};

/// How many batches of lines may wait for a slow caller before the reader
/// waits too.
const QUEUED: usize = 64;

pub(super) async fn run(
    host: &Host,
    kind: &str,
    op: &str,
    args: &Value,
    send: &mut SendStream,
    cut_off: impl Future<Output = &'static str>,
) -> Done {
    let result = match owner_only(kind, op) {
        Ok(()) => read(host, op, args, send, cut_off).await,
        Err(failure) => Err(failure),
    };
    crate::vault::host::done(result, Map::new())
}

async fn read(
    host: &Host,
    op: &str,
    args: &Value,
    send: &mut SendStream,
    cut_off: impl Future<Output = &'static str>,
) -> Result<Value, Failure> {
    let dir = host.workers.state_dir.clone();
    let follow = args["follow"] == true;
    match op {
        "status" => {
            blocking(move || {
                let status = crate::status::local(&StateDir { path: dir })?;
                Ok(serde_json::to_value(status).expect("a status serializes"))
            })
            .await
        }
        "audit" if !follow => {
            blocking(move || {
                let show = AuditAction::Show {
                    follow: false,
                    host: None,
                };
                crate::audit::run(&StateDir { path: dir }, show)
            })
            .await
        }
        "audit" => {
            stream(host, send, cut_off, move |hangup, lines| {
                crate::audit::follow_lines(&dir, Some(hangup), |batch| {
                    lines.blocking_send(batch.to_vec()).is_ok()
                })
            })
            .await
        }
        _ => {
            let events = EventsArgs {
                follow,
                after: args["after"].as_str().map(str::to_string),
                lifecycle_only: args["lifecycle_only"] == true,
                host: None,
            };
            stream(host, send, cut_off, move |hangup, lines| {
                crate::events::stream(&dir, &events, Some(hangup), |batch| {
                    lines.blocking_send(batch.to_vec()).is_ok()
                })
            })
            .await
        }
    }
}

/// Only the owner's own devices may read this host's state from elsewhere.
fn owner_only(kind: &str, op: &str) -> Result<(), Failure> {
    if kind == "device" {
        return Ok(());
    }
    Err(Failure::new(
        Exit::Denied,
        "owner_only",
        format!("only the owner's own devices may read `{op}` from this host"),
    ))
}

/// Runs `read` off the runtime: it reads files and may ask the service
/// manager, which blocks.
async fn blocking(
    read: impl FnOnce() -> Result<Value, Failure> + Send + 'static,
) -> Result<Value, Failure> {
    tokio::task::spawn_blocking(read)
        .await
        .map_err(|err| Failure::failed("read_failed", err.to_string()))?
}

/// Sends the caller each batch of lines `lines` gives, as it gives it. It
/// runs on a thread of its own, waiting on the folder, until it ends or the
/// hang-up descriptor it is given closes: once the caller leaves or the
/// host stops, even while a caller that stopped reading holds up a send.
async fn stream(
    host: &Host,
    send: &mut SendStream,
    cut_off: impl Future<Output = &'static str>,
    lines: impl FnOnce(RawFd, mpsc::Sender<Vec<Value>>) -> Result<(), Failure> + Send + 'static,
) -> Result<Value, Failure> {
    let (hangup, hold) =
        std::io::pipe().map_err(|err| Failure::failed("watch_failed", err.to_string()))?;
    let (queue, mut queued) = mpsc::channel(QUEUED);
    let reader = tokio::task::spawn_blocking(move || lines(hangup.as_raw_fd(), queue));
    tokio::pin!(cut_off);
    let mut stopping = host.stopping.subscribe();
    let stopping = async move {
        let _ = stopping
            .wait_for(|stopping| *stopping != super::Stopping::No)
            .await;
    };
    tokio::pin!(stopping);
    let forward = async {
        // Until the reader is done, and every line it gave was sent.
        while let Some(lines) = queued.recv().await {
            let frame = Response::Ok(json!({"lines": lines}));
            let json = serde_json::to_vec(&frame).expect("lines serialize");
            if wire::write_frame(send, &json).await.is_err() {
                break;
            }
        }
    };
    let ended = tokio::select! {
        () = forward => Ok(()),
        _ = &mut cut_off => Ok(()),
        () = &mut stopping => Err(stopped(&host.name)),
    };
    // Closing the hang-up wakes a reader waiting on the folder; dropping
    // the queue frees one waiting to hand over a line.
    drop(hold);
    drop(queued);
    let read = reader
        .await
        .map_err(|err| Failure::failed("read_failed", err.to_string()))?;
    ended.and(read).map(|()| json!({}))
}

fn stopped(name: &str) -> Failure {
    Failure::new(
        Exit::Unreachable,
        "host_stopped",
        format!("host {name:?} stopped serving; run this again once it serves"),
    )
}
