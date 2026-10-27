//! Calls to other hosts from a machine whose own host is serving go
//! through that host. Both would use the state folder's one key, and a
//! relay delivers to the newest endpoint of a key, so a command that bound
//! its own endpoint beside the host's would take the host's callers from
//! it for as long as it ran (see `StateDir::use_key`).
//!
//! On the host's local socket, the command sends a `proxy` request naming
//! the host to call. This machine's host dials it from its own endpoint,
//! on a connection for this call alone (a host serves one connection's
//! streams one after another, so a follow would hold up any call behind
//! it), and answers as dialing would: `Ok({})`, or the same failure. The
//! socket then carries the call's one stream, each way, as frames
//! (`wire::write_frame`): a frame of the stream's bytes, or an empty frame
//! and then an `Ended` frame. The command's `Finished` is the stream's
//! half-close; its socket closing ends that call's connection, and only
//! that one. The request inside goes as it is, chat and all, and is not
//! judged here: the remote host judges it, as it would a direct call, and
//! sees the same caller, since the key is the same.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

use crate::failure::{Failure, code};
use crate::plow::Peer;
use crate::state::StateDir;
use crate::transport::{RecvStream, SendStream};
use crate::wire::{self, Request, Response};

/// The local socket request that asks the host to carry a call.
pub const OP: &str = "proxy";

/// The most bytes in one frame, and what each side buffers.
pub const CHUNK: usize = 64 * 1024;

/// How long a command waits past the host's own connect timeout for the
/// host to say whether it connected.
const ANSWER_GRACE: Duration = Duration::from_secs(5);

/// How one direction of a proxied stream ended.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// Its sender finished it: the stream's half-close.
    Finished,
    /// The remote host refused the caller, for this reason.
    Denied(String),
    /// The remote stream failed, or this machine's host stopped carrying it.
    Lost,
}

/// Sends `from`'s bytes as frames until `from` ends.
pub async fn send_bytes(
    from: &mut (impl AsyncRead + Unpin),
    to: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = from.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        wire::write_frame(to, &buffer[..read]).await?;
    }
}

pub async fn send_end(to: &mut (impl AsyncWrite + Unpin), ended: &Ended) -> io::Result<()> {
    let json = serde_json::to_vec(ended).expect("an end serializes");
    wire::write_frame(to, &[]).await?;
    wire::write_frame(to, &json).await
}

/// Writes the bytes of the frames `from` sends to `to`, until their end,
/// which it returns. `from` ending first is an error: its sender went away.
pub async fn recv_bytes(
    from: &mut (impl AsyncRead + Unpin),
    to: &mut (impl AsyncWrite + Unpin),
) -> io::Result<Ended> {
    use tokio::io::AsyncWriteExt;
    loop {
        let bytes = wire::read_frame(from).await?;
        if bytes.is_empty() {
            let json = wire::read_frame(from).await?;
            return serde_json::from_slice(&json).map_err(io::Error::other);
        }
        to.write_all(&bytes).await?;
    }
}

/// Asks this machine's host to connect to `host` for one call. Failure
/// here means nothing was sent: exit 3, or the host's own refusal.
pub async fn connect(state: &StateDir, host: &Peer) -> Result<Proxied, Failure> {
    let mut socket = match crate::transport::connect_local(&state.path).await {
        Ok(socket) => socket,
        Err(failure) if failure.code == "serve_required" => {
            if let Some(pid) = crate::lifecycle::other_version(&state.path) {
                return Err(crate::lifecycle::other_version_running(pid));
            }
            return Err(unavailable(host));
        }
        Err(failure) => return Err(failure),
    };
    let request = Request {
        op: OP.to_string(),
        args: serde_json::json!({ "host": host }),
        chat: None,
    };
    let json = serde_json::to_vec(&request).expect("a request serializes");
    let within = crate::dial::CONNECT_TIMEOUT + ANSWER_GRACE;
    let answered = async {
        wire::write_frame(&mut socket, &json).await?;
        wire::read_frame(&mut socket).await
    };
    let Ok(Ok(json)) = tokio::time::timeout(within, answered).await else {
        return Err(unavailable(host));
    };
    let response: Response = serde_json::from_slice(&json).map_err(|_| {
        let message = "this machine's host did not answer with a valid response".to_string();
        Failure::new(code::bad_response, message)
    })?;
    response.into_result()?;
    Ok(Proxied {
        socket: Mutex::new(Some(socket)),
        denied: Arc::default(),
        pump: Mutex::new(None),
    })
}

/// The host holding this folder's key does not take calls now. Nothing
/// dials around it: that would take its callers from it.
fn unavailable(host: &Peer) -> Failure {
    let message = format!(
        "could not connect to host {:?}: this machine's own host holds this state folder's \
         key, so calls to other hosts go through it, and it is not taking calls now \
         (starting, stopping or not answering); nothing was sent. Run this again once \
         `latch status --local` shows it serving",
        host.name
    );
    Failure::new(code::host_unreachable, message)
}

/// A connection to another host through this machine's host: one stream,
/// which a task carries between the socket and the command's two ends.
pub struct Proxied {
    socket: Mutex<Option<UnixStream>>,
    /// Why the remote host refused us, once it has.
    denied: Arc<Mutex<Option<String>>>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl Proxied {
    pub fn open_bi(&self) -> io::Result<(SendStream, RecvStream)> {
        let socket = self
            .socket
            .lock()
            .expect("proxied socket lock")
            .take()
            .ok_or_else(|| io::Error::other("proxied connection already used"))?;
        let (ours, outgoing) = tokio::io::duplex(CHUNK);
        let (incoming, theirs) = tokio::io::duplex(CHUNK);
        let pump = tokio::spawn(pump(socket, outgoing, theirs, self.denied.clone()));
        *self.pump.lock().expect("proxied pump lock") = Some(pump);
        Ok((
            SendStream::Proxied(Some(ours)),
            RecvStream::Proxied(incoming),
        ))
    }

    pub fn denied(&self) -> Option<String> {
        self.denied.lock().expect("proxied denial lock").clone()
    }

    /// Closes the socket, which ends this call's connection.
    pub fn close(&self) {
        self.socket.lock().expect("proxied socket lock").take();
        if let Some(pump) = self.pump.lock().expect("proxied pump lock").take() {
            pump.abort();
        }
    }
}

impl Drop for Proxied {
    fn drop(&mut self) {
        self.close();
    }
}

/// Carries the command's bytes (`outgoing`) to the socket and the
/// socket's to the command (`incoming`), until both directions end.
async fn pump(
    socket: UnixStream,
    mut outgoing: DuplexStream,
    mut incoming: DuplexStream,
    denied: Arc<Mutex<Option<String>>>,
) {
    let (mut read, mut write) = socket.into_split();
    let sending = async move {
        // Dropping the send half, as finishing it does, is its end.
        if send_bytes(&mut outgoing, &mut write).await.is_ok() {
            let _ = send_end(&mut write, &Ended::Finished).await;
        }
        write
    };
    let receiving = async move {
        if let Ok(Ended::Denied(reason)) = recv_bytes(&mut read, &mut incoming).await {
            *denied.lock().expect("proxied denial lock") = Some(reason);
        }
        // The command reads what is left, then the end.
        drop(incoming);
        read
    };
    let _ = tokio::join!(sending, receiving);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_carry_bytes_then_their_end_and_a_sender_gone_is_no_end() {
        let mut sent = Vec::new();
        send_bytes(&mut &b"hello"[..], &mut sent).await.unwrap();
        send_end(&mut sent, &Ended::Denied("unknown key".into()))
            .await
            .unwrap();
        let mut got = Vec::new();
        let ended = recv_bytes(&mut sent.as_slice(), &mut got).await.unwrap();
        assert_eq!(
            (got.as_slice(), ended),
            (&b"hello"[..], Ended::Denied("unknown key".into()))
        );

        let mut cut = Vec::new();
        send_bytes(&mut &b"hello"[..], &mut cut).await.unwrap();
        assert!(
            recv_bytes(&mut cut.as_slice(), &mut Vec::new())
                .await
                .is_err()
        );
    }
}
