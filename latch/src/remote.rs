//! `status` and `audit` with `--host`: another host's own output, read
//! over latch's connection as the `status` and `audit` operations (see
//! `serve/reads.rs`). The host answers its
//! owner's own devices only. A failed call never falls back to this
//! machine's own state; naming this machine reads it here.

use std::future::Future;
use std::io::{Read, Write};
use std::time::Duration;

use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};

use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::Peer;
use crate::state::StateDir;
use crate::transport::Connection;
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

/// How many lines may wait for a slow reader of our stdout.
const QUEUED: usize = 64;

/// Prints each line `host` streams, one JSON line each, as it comes: until
/// the host ends them, nobody reads our stdout, or we are asked to stop
/// (Ctrl-C, SIGTERM, or, when stdin is a pipe, its closing). Stopping
/// closes the connection at once, so the host stops reading for us too.
pub async fn stream(state: &StateDir, host: &Peer, op: &str, args: Value) -> Result<(), Failure> {
    let request = request(op, args);
    let link = dial::connect(state, host).await?;
    let printed = print_stream(
        &link.connection,
        host,
        &request,
        std::io::stdout(),
        stop_requested,
    )
    .await;
    link.close();
    printed
}

/// The opening (an audit read through `caught_up`, a status through its
/// first `status`) is printed whole before anything can stop it, as the
/// host's own command prints it; only then is `stop` asked for. After it,
/// lines go through a printer thread, so a stop ends this even while a
/// reader that stopped reading holds up a write.
async fn print_stream<S: Future<Output = ()>>(
    connection: &Connection,
    host: &Peer,
    request: &Request,
    mut out: impl Write + Send + 'static,
    stop: impl FnOnce() -> S,
) -> Result<(), Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    loop {
        let answer = dial::answer(connection, &mut recv, ANSWER_WITHIN).await;
        let Some(line) = line(answer, host)? else {
            return Ok(());
        };
        if !print(&mut out, &line) {
            return Ok(());
        }
        if matches!(line["type"].as_str(), Some("caught_up" | "status")) {
            break;
        }
    }
    let (printer, printed) = printer(out);
    let following = async {
        // A followed host may say nothing for hours.
        while let Some(line) = line(
            dial::answer(connection, &mut recv, Duration::MAX).await,
            host,
        )? {
            if printer.send(line).await.is_err() {
                // Nobody reads our stdout.
                return Ok(());
            }
        }
        drop(printer);
        let _ = printed.await;
        Ok(())
    };
    tokio::select! {
        result = following => result,
        () = stop() => Ok(()),
    }
}

/// The line in a host's answer, or `None` once it ends them.
fn line(answer: Result<Value, Failure>, host: &Peer) -> Result<Option<Value>, Failure> {
    let mut answer = match answer {
        Err(failure) if failure.code == "outcome_unknown" => return Err(dropped(host)),
        answer => answer?,
    };
    Ok(answer.get_mut("line").map(Value::take))
}

/// Writes `line`; false once nobody reads it.
fn print(out: &mut impl Write, line: &Value) -> bool {
    writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
}

/// A thread that prints each line it is sent, until the queue closes or a
/// write fails; `printed` finishes when it is done. A thread of its own: a
/// write it is stuck in never keeps the process from exiting.
fn printer(mut out: impl Write + Send + 'static) -> (mpsc::Sender<Value>, oneshot::Receiver<()>) {
    let (queue, mut queued) = mpsc::channel::<Value>(QUEUED);
    let (done, printed) = oneshot::channel();
    std::thread::spawn(move || {
        while let Some(line) = queued.blocking_recv() {
            if !print(&mut out, &line) {
                break;
            }
        }
        let _ = done.send(());
    });
    (queue, printed)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::wire::{self, Response};

    fn host() -> Peer {
        Peer {
            key: "c".repeat(64),
            kind: "device".into(),
            name: "studio".into(),
            relay_url: None,
            last_seen_at: None,
            online: Some(true),
            this_machine: false,
        }
    }

    /// A host on the other end of a socket that answers with `answers`,
    /// then keeps the stream open.
    fn host_answering(answers: Vec<Value>) -> Connection {
        let (mine, theirs) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(async move {
            let (mut recv, mut send) = theirs.into_split();
            wire::read_frame(&mut recv).await.unwrap();
            for answer in answers {
                let frame = serde_json::to_vec(&Response::Ok(answer)).unwrap();
                wire::write_frame(&mut send, &frame).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        Connection::Local(Mutex::new(Some(mine)))
    }

    #[derive(Clone, Default)]
    struct Printed(Arc<Mutex<Vec<u8>>>);

    impl Write for Printed {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_opening_is_printed_whole_even_when_asked_to_stop_at_once() {
        let opening = [
            json!({"type": "hello", "schema": 1}),
            json!({"type": "snapshot", "cursor": "0".repeat(64), "has_older": false}),
            json!({"type": "record", "cursor": "a".repeat(64), "record": {"op": "exec"}}),
            json!({"type": "caught_up", "cursor": "a".repeat(64), "has_older": false}),
        ];
        let mut answers: Vec<Value> = opening.iter().map(|line| json!({"line": line})).collect();
        answers.push(json!({"line": {"type": "record", "cursor": "b".repeat(64)}}));
        let connection = host_answering(answers);
        let out = Printed::default();
        let request = request("audit", json!({"follow": true}));
        print_stream(&connection, &host(), &request, out.clone(), || async {})
            .await
            .unwrap();
        let printed = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let expected: String = opening.iter().map(|line| format!("{line}\n")).collect();
        assert!(printed.starts_with(&expected), "{printed}");
    }

    #[test]
    fn a_stop_ends_the_stream_while_a_write_to_a_stalled_reader_is_stuck() {
        // A pipe nobody reads: a write past its buffer never returns.
        let (_unread, stalled) = std::io::pipe().unwrap();
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let result = runtime.block_on(async {
                let connection = host_answering(vec![
                    json!({"line": {"type": "caught_up"}}),
                    json!({"line": {"type": "record", "big": "x".repeat(1 << 20)}}),
                ]);
                let request = request("audit", json!({"follow": true}));
                let stop = || tokio::time::sleep(Duration::from_millis(300));
                print_stream(&connection, &host(), &request, stalled, stop).await
            });
            let _ = ended.send(result.is_ok());
            // The printer stays stuck in its write; this runtime is left behind.
            std::mem::forget(runtime);
        });
        let stopped = end.recv_timeout(Duration::from_secs(5));
        assert_eq!(stopped, Ok(true), "the stop did not end the stream");
    }
}
