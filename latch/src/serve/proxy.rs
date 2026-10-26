//! The host's side of `proxy.rs`: a latch command on this machine calls
//! another host, and this host makes the call from its own endpoint. The
//! request inside is the command's, sent on unread: the other host judges
//! it, as it judges a direct call. A followed read ends as the host starts
//! stopping, as one of its own does; any other call gets the time an
//! operation gets to finish.

use std::sync::atomic::{AtomicBool, Ordering};

use iroh::endpoint::{Connection, VarInt};
use serde_json::json;
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::{Host, Stopping, answer, failed};
use crate::dial;
use crate::failure::code;
use crate::log::log;
use crate::plow::Peer;
use crate::proxy::{self, Ended};
use crate::wire::{self, Request, Response};

pub async fn serve(host: &Host, request: &Request, mut send: OwnedWriteHalf, recv: OwnedReadHalf) {
    let Ok(peer) = serde_json::from_value::<Peer>(request.args["host"].clone()) else {
        let message = "a proxy request names the host to call".to_string();
        return answer(&mut send, &failed(code::bad_request, message)).await;
    };
    let connection = match dial::connect_on(&host.endpoint, &peer, dial::CONNECT_TIMEOUT).await {
        Ok(connection) => connection,
        Err(failure) => return answer(&mut send, &Response::error(&failure)).await,
    };
    // Not `answer`, which ends the socket's writes.
    let connected = serde_json::to_vec(&Response::Ok(json!({}))).expect("a response serializes");
    if wire::write_frame(&mut send, &connected).await.is_err() {
        return;
    }

    let follow = AtomicBool::new(false);
    let (to_remote, from_caller) = tokio::io::duplex(proxy::CHUNK);
    let caller = caller(recv, to_remote);
    let forwarding = async {
        forward(&connection, &peer, from_caller, &mut send, &follow).await;
        // Done both ways: the caller closing its socket ends the call.
        std::future::pending::<()>().await
    };
    let mut stopping = host.stopping.subscribe();
    let stopped = stopping.wait_for(|stopping| match stopping {
        Stopping::No => false,
        Stopping::Draining => follow.load(Ordering::SeqCst),
        Stopping::CuttingOff => true,
    });
    let stopped = tokio::select! {
        () = caller => false,
        () = forwarding => false,
        _ = stopped => true,
    };
    if stopped {
        let _ = proxy::send_end(&mut send, &Ended::Lost).await;
    }
    connection.close(VarInt::from_u32(0), b"done");
}

/// The caller's bytes into `to_remote`, until it finishes its side and
/// then goes away, or goes away without finishing: either way, its call
/// is over.
async fn caller(mut recv: OwnedReadHalf, mut to_remote: DuplexStream) {
    let ended = proxy::recv_bytes(&mut recv, &mut to_remote).await;
    drop(to_remote);
    if matches!(ended, Ok(Ended::Finished)) {
        let _ = recv.read_u8().await;
    }
}

/// The call's one stream: the caller's request and bytes up, the remote
/// host's down, each direction's end passed on as it comes.
async fn forward(
    connection: &Connection,
    peer: &Peer,
    mut from_caller: DuplexStream,
    send: &mut OwnedWriteHalf,
    follow: &AtomicBool,
) {
    let Ok(frame) = wire::read_frame(&mut from_caller).await else {
        return;
    };
    if let Ok(request) = serde_json::from_slice::<Request>(&frame) {
        follow.store(request.args["follow"] == true, Ordering::SeqCst);
        log(&format!("proxy: {} to {:?}", request.op, peer.name));
    }
    let opened = async {
        let (mut up, down) = connection.open_bi().await.ok()?;
        wire::write_frame(&mut up, &frame).await.ok()?;
        Some((up, down))
    };
    let Some((mut up, mut down)) = opened.await else {
        let _ = proxy::send_end(send, &ended(connection)).await;
        return;
    };
    let upward = async {
        if tokio::io::copy(&mut from_caller, &mut up).await.is_ok() {
            let _ = up.finish();
        }
    };
    let downward = async {
        let ended = match proxy::send_bytes(&mut down, send).await {
            Ok(()) => Ended::Finished,
            Err(_) => ended(connection),
        };
        let _ = proxy::send_end(send, &ended).await;
    };
    tokio::join!(upward, downward);
}

/// Why the remote stream failed, as the caller's client would tell.
fn ended(connection: &Connection) -> Ended {
    match crate::transport::refusal(connection) {
        Some(reason) => Ended::Denied(reason),
        None => Ended::Lost,
    }
}
