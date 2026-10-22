//! What the host's local socket answers besides operations, for `latch
//! serve`, `stop` and `dashboard` on this machine: who the host is
//! (`host_hello`), stop (`host_stop`), and its board, for a viewer
//! (`host_watch`) or for the attached `latch serve` whose life the host
//! shares (`host_attach`). The first answer is the usual `Response`; a
//! watcher then gets the board as a frame of its own each time it changes.
//! Over iroh these are unknown operations.
//!
//! An attached host (`serve --attached`) stops when its holder's connection
//! ends, however that `latch serve` ended: Ctrl-C, its terminal closing, or
//! killed. Only a host started that way takes a holder, and only one, so a
//! host started detached is never tied to a terminal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Notify;

use super::{Host, Stopping, answer, failed};
use crate::lifecycle;
use crate::log::log;
use crate::wire::{self, Response};

pub const OPS: [&str; 4] = ["host_hello", "host_stop", "host_watch", "host_attach"];

/// How long an attached host waits for its holder once it serves: the
/// `latch serve` that started it attaches at once, or is gone.
const HOLD_WITHIN: Duration = Duration::from_secs(30);

pub struct Lifetime {
    attached: bool,
    held: AtomicBool,
    /// Ends the host's serving loop.
    pub stop: Notify,
}

impl Lifetime {
    pub fn new(attached: bool) -> Lifetime {
        Lifetime {
            attached,
            held: AtomicBool::new(false),
            stop: Notify::new(),
        }
    }

    fn stop(&self, why: &str) {
        log(&format!("stopping: {why}"));
        self.stop.notify_one();
    }
}

/// An attached host whose holder never comes stops, rather than run on
/// with no one to stop it.
pub fn await_holder(host: &Arc<Host>) {
    if !host.lifetime.attached {
        return;
    }
    let host = host.clone();
    tokio::spawn(async move {
        tokio::time::sleep(HOLD_WITHIN).await;
        if !host.lifetime.held.load(Ordering::SeqCst) {
            host.lifetime
                .stop("the `latch serve` that started this host never attached");
        }
    });
}

pub async fn serve(host: &Host, op: &str, mut send: OwnedWriteHalf, recv: OwnedReadHalf) {
    let hello = Response::Ok(hello(host));
    match op {
        "host_hello" => answer(&mut send, &hello).await,
        "host_stop" => {
            answer(&mut send, &hello).await;
            host.lifetime.stop("asked to by `latch stop`");
        }
        "host_watch" => watch(host, hello, send, recv).await,
        _ if !host.lifetime.attached => {
            let message = "this host was started detached, so no `latch serve` holds it; \
                           `latch dashboard` views it"
                .to_string();
            answer(&mut send, &failed("not_attached", message)).await;
        }
        _ if host.lifetime.held.swap(true, Ordering::SeqCst) => {
            let message = "another `latch serve` already holds this host".to_string();
            answer(&mut send, &failed("already_attached", message)).await;
        }
        _ => {
            watch(host, hello, send, recv).await;
            host.lifetime
                .stop("the `latch serve` that holds this host went away");
        }
    }
}

/// What a client checks before it relies on this host: that it is the
/// host now on the state folder (`run`), and which latch it runs.
fn hello(host: &Host) -> Value {
    json!({"pid": std::process::id(), "run": lifecycle::this_run(),
           "version": crate::cli::version(), "attached": host.lifetime.attached})
}

/// Answers `hello`, then sends the board as it changes until the watcher
/// goes away (its side of the socket ends, or it cannot be written to) or
/// the host is stopping.
async fn watch(host: &Host, hello: Response, mut send: OwnedWriteHalf, mut recv: OwnedReadHalf) {
    let hello = serde_json::to_vec(&hello).expect("a response serializes");
    if wire::write_frame(&mut send, &hello).await.is_err() {
        return;
    }
    let Some(dashboard) = &host.dashboard else {
        return;
    };
    let mut boards = dashboard.watch();
    let mut stopping = host.stopping.subscribe();
    let mut byte = [0];
    loop {
        let board = serde_json::to_vec(&*boards.borrow_and_update()).expect("a board serializes");
        if wire::write_frame(&mut send, &board).await.is_err() {
            return;
        }
        tokio::select! {
            changed = boards.changed() => if changed.is_err() { return },
            // A watcher sends nothing after its request: anything read,
            // the end included, means it is gone.
            _ = recv.read(&mut byte) => return,
            // A stopping host lets its watchers go first, rather than
            // wait for them as for an operation.
            _ = stopping.wait_for(|stopping| *stopping != Stopping::No) => return,
        }
    }
}
