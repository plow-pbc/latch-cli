//! The `status`, `audit`, `audit_shot` and `audit_judge` operations: what
//! this host's owner reads from another machine with `--host`, the same
//! output as `latch status --local [--follow]`, `latch audit`, `latch audit
//! shot` and `latch audit judge` give on the host itself. From the owner's
//! own devices only; an agent's key gets exit 4 `owner_only`.
//!
//! `audit` and `status` with `follow` stream: each line goes to the caller
//! as its own `{"ok": {"line": LINE}}` frame, until the lines end, the
//! caller leaves or this host stops; then the final answer (see `wire.rs`).
//! A line too large for a frame ends it with `record_too_large`, never a
//! line left out.

use std::future::Future;
use std::os::fd::{AsRawFd, RawFd};

use iroh::endpoint::VarInt;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::Host;
use crate::failure::{Code, Failure, code};
use crate::serve::Done;
use crate::state::StateDir;
use crate::transport::SendStream;
use crate::wire::{self, Response};

/// How many lines may wait for a slow caller before the reader waits too.
const QUEUED: usize = 64;

pub(super) async fn run(
    host: &Host,
    kind: &str,
    op: &str,
    args: &Value,
    send: &mut SendStream,
    cut_off: impl Future<Output = Code>,
) -> Done {
    let result = match crate::gate::owner::owner_only(kind, &format!("this host's {op}")) {
        Ok(()) if op == "audit_shot" => return shot(host, args, send).await,
        Ok(()) if op == "audit_judge" => return judge(host, args).await,
        Ok(()) => read(host, op, args, send, cut_off).await,
        Err(failure) => Err(failure),
    };
    crate::vault::host::done(result, Map::new())
}

/// `audit shot`: the browser screenshot an audit record names, as a file
/// is sent: its size, its bytes, then their sha256.
async fn shot(host: &Host, args: &Value, send: &mut SendStream) -> Done {
    let (record, shot) = (args["record"].clone(), args["shot"].clone());
    let audit = Map::from_iter([
        ("record".into(), record.clone()),
        ("shot".into(), shot.clone()),
    ]);
    let dir = host.workers.state_dir.clone();
    let read = tokio::task::spawn_blocking(move || {
        let (Some(record), Some(shot)) = (record.as_str(), shot.as_str()) else {
            let message = "audit shot takes a record's id and a shot".to_string();
            return Err(Failure::new(code::bad_request, message));
        };
        crate::browser::journal::audited(&dir, record, shot)
    })
    .await
    .map_err(|err| Failure::new(code::read_failed, err.to_string()))
    .flatten();
    let png = match read {
        Ok(png) => png,
        Err(failure) => return crate::vault::host::done(Err(failure), audit),
    };
    Done::send_bytes(send, json!({"size": png.len()}), &png, audit).await
}

/// `audit judge`: what the gatekeeper sent for one call and what it was
/// answered.
async fn judge(host: &Host, args: &Value) -> Done {
    let request = args["request"].clone();
    let audit = Map::from_iter([("request".into(), request.clone())]);
    let dir = host.workers.state_dir.clone();
    let read = blocking(move || {
        let Some(request) = request.as_str() else {
            let message = "audit judge takes a request id".to_string();
            return Err(Failure::new(code::bad_request, message));
        };
        crate::gate::requests::read(&dir, request)
    })
    .await;
    crate::vault::host::done(read, audit)
}

async fn read(
    host: &Host,
    op: &str,
    args: &Value,
    send: &mut SendStream,
    cut_off: impl Future<Output = Code>,
) -> Result<Value, Failure> {
    let dir = host.workers.state_dir.clone();
    match op {
        "status" if args["follow"] != true => {
            blocking(move || {
                let status = crate::status::local(&StateDir { path: dir })?;
                Ok(serde_json::to_value(status).expect("a status serializes"))
            })
            .await
        }
        "status" => {
            stream(host, send, cut_off, move |hangup, lines| {
                crate::status::follow(&dir, Some(hangup), |line| {
                    lines.blocking_send(line.clone()).is_ok()
                })
            })
            .await
        }
        _ => {
            let range: crate::audit::Range = serde_json::from_value(args.clone())
                .map_err(|err| Failure::new(code::usage, format!("not an audit range: {err}")))?;
            let whose = format!("{}'s", host.name);
            stream(host, send, cut_off, move |hangup, lines| {
                crate::audit::read(&dir, &whose, &range, Some(hangup), |line| {
                    lines.blocking_send(line.clone()).is_ok()
                })
            })
            .await
        }
    }
}

/// Runs `read` off the runtime: it reads files and may ask the service
/// manager, which blocks.
async fn blocking(
    read: impl FnOnce() -> Result<Value, Failure> + Send + 'static,
) -> Result<Value, Failure> {
    tokio::task::spawn_blocking(read)
        .await
        .map_err(|err| Failure::new(code::read_failed, err.to_string()))?
}

/// Sends the caller each line `lines` gives, as it gives it. It
/// runs on a thread of its own, waiting on the folder, until it ends or the
/// hang-up descriptor it is given closes: once the caller leaves or the
/// host stops, even while a caller that stopped reading holds up a send.
async fn stream(
    host: &Host,
    send: &mut SendStream,
    cut_off: impl Future<Output = Code>,
    lines: impl FnOnce(RawFd, mpsc::Sender<Value>) -> Result<(), Failure> + Send + 'static,
) -> Result<Value, Failure> {
    let (hangup, hold) =
        std::io::pipe().map_err(|err| Failure::new(code::watch_failed, err.to_string()))?;
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
    let ended = loop {
        // A stop ends it before the next line, however many are waiting.
        let line = tokio::select! {
            biased;
            _ = &mut cut_off => break Ok(()),
            () = &mut stopping => break Err(stopped(&host.name)),
            line = queued.recv() => line,
        };
        // The reader is done, and every line it gave was sent.
        let Some(line) = line else {
            break Ok(());
        };
        let frame = Response::Ok(json!({"line": line}));
        let json = serde_json::to_vec(&frame).expect("a line serializes");
        if json.len() > wire::MAX_FRAME {
            break Err(too_large(&line, json.len()));
        }
        // A frame cut off midway can't be followed by an answer: the
        // stream is reset instead, so the caller never reads half of one.
        // A frame the caller takes at once is sent whole, stop or not.
        let cut = tokio::select! {
            biased;
            sent = wire::write_frame(send, &json) => match sent {
                Ok(()) => continue,
                // The caller is gone.
                Err(_) => break Ok(()),
            },
            _ = &mut cut_off => Ok(()),
            () = &mut stopping => Err(stopped(&host.name)),
        };
        let _ = send.reset(VarInt::from_u32(0));
        break cut;
    };
    // Closing the hang-up wakes a reader waiting on the folder; dropping
    // the queue frees one waiting to hand over a line.
    drop(hold);
    drop(queued);
    let read = reader
        .await
        .map_err(|err| Failure::new(code::read_failed, err.to_string()))?;
    ended.and(read).map(|()| json!({}))
}

/// A record whose line is larger than the wire carries in one frame.
fn too_large(line: &Value, size: usize) -> Failure {
    let id = &line["record"]["id"];
    let mut failure = Failure::new(
        code::record_too_large,
        format!(
            "the record {id} is {size} bytes, more than the {} one frame carries",
            wire::MAX_FRAME
        ),
    );
    failure.next = Some("read it on that host".into());
    failure.details = json!({"id": id, "size": size, "limit": wire::MAX_FRAME});
    failure
}

fn stopped(name: &str) -> Failure {
    let mut failure = Failure::new(code::host_stopped, format!("host {name:?} stopped serving"));
    failure.next = Some("run this again once it serves".into());
    failure
}
