//! `status` and `audit` with `--host`: another host's own output, read
//! over latch's connection as the `status` and `audit` operations (see
//! `serve/reads.rs`). The host answers its
//! owner's own devices only. A failed call never falls back to this
//! machine's own state; naming this machine reads it here.

use std::future::Future;
use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};

use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Failure, code};
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

/// `host`'s `status --local`, as it prints it there: the id of the host's
/// record of this read is left out, as a stream leaves it out.
pub async fn status(state: &StateDir, host: &Peer) -> Result<Value, Failure> {
    let request = request("status", serde_json::json!({}));
    let mut status = dial::call(state, host, &request, ANSWER_WITHIN).await?;
    if let Some(status) = status.as_object_mut() {
        status.remove("audit_id");
    }
    Ok(status)
}

/// How many lines may wait for a slow reader of our stdout.
const QUEUED: usize = 64;

/// Prints each line `host` streams, one JSON line each, as it comes,
/// until the host ends them or nobody reads our stdout. A followed read
/// (`follow`) also ends when we are asked to stop (Ctrl-C, SIGTERM, or,
/// when stdin is a pipe, its closing), from its first line on; stopping
/// closes the connection at once, so the host stops reading for us too. A
/// read that ends by itself never looks at stdin: it always prints whole.
/// A follow on an endpoint of its own ends when a host starts on this
/// state folder, which waits for the key it holds (`StateDir::own_key`).
pub async fn stream(
    state: &StateDir,
    host: &Peer,
    op: &str,
    args: Value,
    follow: bool,
) -> Result<(), Failure> {
    let request = request(op, args);
    let link = dial::connect(state, host).await?;
    let stop = follow.then_some(stop_requested);
    let printing = print_stream(&link.connection, host, &request, std::io::stdout(), stop);
    let printed = if follow && link.direct() {
        tokio::select! {
            printed = printing => printed,
            () = host_started(state) => Err(host_took_key(host)),
        }
    } else {
        printing.await
    };
    link.close();
    printed.map_err(dial::naming(host))
}

/// How often a direct follow looks for a host starting.
const HOST_CHECK_EVERY: Duration = Duration::from_secs(1);

/// Finishes once a host runs on this state folder.
async fn host_started(state: &StateDir) {
    while !crate::lifecycle::now(&state.path).running {
        tokio::time::sleep(HOST_CHECK_EVERY).await;
    }
}

fn host_took_key(host: &Peer) -> Failure {
    let message = format!(
        "a host started on this machine, and while it serves only it may use this state \
         folder's key, so this follow of host {:?} ended to let it start",
        host.name
    );
    let mut failure = Failure::new(code::host_unreachable, message);
    failure.next = Some("run this again: it then goes through that host".into());
    failure
}

/// Lines go through a printer thread, so a stop ends this even while a
/// reader that stopped reading holds up a write. Without `stop`, it ends
/// once every line is printed.
async fn print_stream<S: Future<Output = ()>>(
    connection: &Connection,
    host: &Peer,
    request: &Request,
    out: impl Write + Send + 'static,
    stop: Option<impl FnOnce() -> S>,
) -> Result<(), Failure> {
    let (printer, mut printed) = printer(out);
    let reading = async {
        let (mut send, mut recv) = dial::start(connection, request).await?;
        send.finish().map_err(|_| dial::lost(connection, true))?;
        // The opening comes at once; after it, a followed host may say
        // nothing for hours.
        let mut within = ANSWER_WITHIN;
        loop {
            let answer = tokio::select! {
                answer = dial::answer(connection, &mut recv, within) => answer,
                // The printer ended early: nobody reads our stdout.
                _ = &mut printed => return Ok(()),
            };
            let Some(line) = line(answer, host)? else {
                break;
            };
            if matches!(line["type"].as_str(), Some("caught_up" | "status")) {
                within = Duration::MAX;
            }
            if printer.send(line).await.is_err() {
                // Nobody reads our stdout.
                return Ok(());
            }
        }
        drop(printer);
        let _ = printed.await;
        Ok(())
    };
    match stop {
        Some(stop) => tokio::select! {
            result = reading => result,
            () = stop() => Ok(()),
        },
        None => reading.await,
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
    let message = format!("the connection to host {:?} dropped", host.name);
    let mut failure = Failure::new(code::host_unreachable, message);
    failure.next = Some("run this again".into());
    failure
}

/// SIGTERM, Ctrl-C, or (when it is a pipe) stdin closing: whoever started
/// this wants it to stop.
pub(crate) async fn stop_requested() {
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
        crate::watch::wait_for_stdin_to_close();
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

    /// A stop that is never asked for: a read that ends by itself.
    const NO_STOP: Option<fn() -> std::future::Ready<()>> = None;

    #[tokio::test]
    async fn a_read_that_ends_by_itself_prints_every_line() {
        let lines = [
            json!({"type": "hello", "schema": 1}),
            json!({"type": "snapshot", "cursor": "0".repeat(64), "has_older": false}),
            json!({"type": "record", "cursor": "a".repeat(64), "record": {"op": "exec"}}),
            json!({"type": "caught_up", "cursor": "a".repeat(64), "has_older": false}),
        ];
        let mut answers: Vec<Value> = lines.iter().map(|line| json!({"line": line})).collect();
        answers.push(json!({"audit_id": "b".repeat(64)}));
        let connection = host_answering(answers);
        let out = Printed::default();
        let request = request("audit", json!({}));
        print_stream(&connection, &host(), &request, out.clone(), NO_STOP)
            .await
            .unwrap();
        let printed = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let expected: String = lines.iter().map(|line| format!("{line}\n")).collect();
        assert_eq!(printed, expected);
    }

    #[test]
    fn a_stop_ends_a_follow_while_its_opening_is_stuck_in_a_write_to_a_stalled_reader() {
        // A pipe nobody reads: a write past its buffer never returns.
        let (_unread, stalled) = std::io::pipe().unwrap();
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let result = runtime.block_on(async {
                // The opening never reaches `caught_up`: its first record
                // is more than the pipe holds.
                let connection = host_answering(vec![
                    json!({"line": {"type": "hello", "schema": 1}}),
                    json!({"line": {"type": "record", "big": "x".repeat(1 << 20)}}),
                    json!({"line": {"type": "caught_up"}}),
                ]);
                let request = request("audit", json!({"follow": true}));
                let stop = || tokio::time::sleep(Duration::from_millis(300));
                print_stream(&connection, &host(), &request, stalled, Some(stop)).await
            });
            let _ = ended.send(result.is_ok());
            // The printer stays stuck in its write; this runtime is left behind.
            std::mem::forget(runtime);
        });
        let stopped = end.recv_timeout(Duration::from_secs(5));
        assert_eq!(stopped, Ok(true), "the stop did not end the stream");
    }

    /// A stdout whose reader went away.
    struct Gone;

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_follow_ends_once_nobody_reads_it_though_its_host_stays_quiet() {
        let connection = host_answering(vec![json!({"line": {"type": "hello", "schema": 1}})]);
        let request = request("status", json!({"follow": true}));
        let never = || std::future::pending::<()>();
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            print_stream(&connection, &host(), &request, Gone, Some(never)),
        )
        .await;
        assert!(matches!(ended, Ok(Ok(()))), "it waited on the host");
    }
}
