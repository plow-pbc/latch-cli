//! The client side of an operation: dial the host over iroh, send the
//! request, read the answer, and turn every way that can go wrong into the
//! right exit code (file transfers, in `transfer.rs`, use the same pieces):
//!
//! - could not connect, so nothing was sent: exit 3 (retry later);
//! - the host refused us: exit 4 (don't retry);
//! - the connection dropped after the request was sent: exit 5 (the
//!   operation may or may not have happened; check before resending).

use std::sync::Mutex;
use std::time::Duration;

use crate::transport::{Connection, RecvStream, SendStream};
use iroh::endpoint::{
    ConnectError, ConnectingError, ConnectionError, TransportErrorCode, VarInt, presets,
};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl};
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::failure::{Exit, Failure};
use crate::plow::Peer;
use crate::state::StateDir;
use crate::wire::{self, Request, Response};

/// How long to try to reach a host before giving up with exit 3.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for a host's answer to an operation without a time
/// limit of its own. For a file transfer, it is the wait before the bytes
/// start and after they end; the bytes themselves may take as long as they
/// keep flowing (a stalled connection ends on QUIC's idle timeout).
pub const ANSWER_WITHIN: Duration = Duration::from_secs(120);

/// Added to an operation's own time limit (such as `exec --timeout`) for the
/// host to stop it, report, and the answer to travel back.
pub const ANSWER_MARGIN: Duration = Duration::from_secs(30);

/// An open connection to a host, and the local endpoint it runs on.
pub struct Link {
    endpoint: Option<Endpoint>,
    pub connection: Connection,
}

/// Dials this state folder over its private socket, or another host over iroh.
/// Failure here means nothing was sent: exit 3.
pub async fn connect(state: &StateDir, host: &Peer) -> Result<Link, Failure> {
    let key = state.load_key()?;
    if host.key == key.public().to_string() {
        let socket = crate::transport::connect_local(&state.path).await?;
        return Ok(Link {
            endpoint: None,
            connection: Connection::Local(std::sync::Mutex::new(Some(socket))),
        });
    }
    let (id, relay) = host_address(host)?;
    // Our own key is our identity: the host learns who we are from the
    // connection's handshake, not from anything we send. Our only relay is
    // the host's own, so a client contacts no relay the host did not choose.
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::custom([relay.clone()]))
        .bind()
        .await
        .map_err(|err| Failure::failed("iroh", format!("could not start iroh: {err}")))?;
    let address = EndpointAddr::new(id).with_relay_url(relay);
    match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(address, wire::ALPN)).await {
        Ok(Ok(connection)) => Ok(Link {
            endpoint: Some(endpoint),
            connection: Connection::Remote(connection),
        }),
        Ok(Err(err)) if speaks_another_version(&err) => {
            endpoint.close().await;
            // Builds of one protocol version work together; only that differs.
            let message = format!(
                "host {:?} does not speak {}, the protocol of this latch {}, so one of \
                 the two runs an older latch; nothing was sent. Compare `latch --version` on \
                 both machines and update the older one",
                host.name,
                String::from_utf8_lossy(wire::ALPN),
                crate::cli::version(),
            );
            Err(Failure::failed("protocol_mismatch", message))
        }
        _ => {
            endpoint.close().await;
            let message = format!(
                "could not connect to host {:?}; nothing was sent",
                host.name
            );
            Err(Failure::new(Exit::Unreachable, "host_unreachable", message))
        }
    }
}

/// The host refused the handshake because it speaks no protocol version we
/// offered (TLS alert 120, "no application protocol").
fn speaks_another_version(err: &ConnectError) -> bool {
    let closed = match err {
        ConnectError::Connection { source, .. } => source,
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        _ => return false,
    };
    matches!(closed, ConnectionError::ConnectionClosed(close)
        if close.error_code == TransportErrorCode::crypto(120))
}

/// How long a client waits, once it is done, for its close to go out.
/// iroh would otherwise wait for the connection to drain: three times its
/// slowest path's probe timeout, about a second from a Mac to a Pi on the
/// same network. The host's answer has already arrived by then (and the
/// host wrote its audit record before sending it), so this only decides
/// how soon the host hears the client left: at once, or, if the close is
/// lost, when the connection times out.
const CLOSE_WITHIN: Duration = Duration::from_millis(100);

/// Closes still going out, which the command's output does not wait for.
static CLOSING: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

impl Link {
    /// Closes the connection. The close goes out while the command prints
    /// its answer; `closed` gives it up to `CLOSE_WITHIN` before exit.
    pub fn close(self) {
        if let Some(endpoint) = self.endpoint {
            self.connection.close(VarInt::from_u32(0), b"done");
            let closing = tokio::spawn(async move {
                let _ = tokio::time::timeout(CLOSE_WITHIN, endpoint.close()).await;
            });
            CLOSING.lock().expect("closing lock").push(closing);
        }
    }
}

/// Waits for every close `Link::close` started, before this process exits.
pub async fn closed() {
    let closing = std::mem::take(&mut *CLOSING.lock().expect("closing lock"));
    for close in closing {
        let _ = close.await;
    }
}

/// Sends `request` to `host` and returns the host's result, for operations
/// that are one request and one answer. `answer_within` is how long to wait
/// for the answer once the request is sent; past it the outcome is unknown
/// (exit 5). It must cover the operation's own running time, such as an
/// `exec` timeout.
pub async fn call(
    state: &StateDir,
    host: &Peer,
    request: &Request,
    answer_within: Duration,
) -> Result<Value, Failure> {
    let link = connect(state, host).await?;
    let result = one_exchange(&link.connection, request, answer_within).await;
    link.close();
    result
}

async fn one_exchange(
    connection: &Connection,
    request: &Request,
    answer_within: Duration,
) -> Result<Value, Failure> {
    let (mut send, mut recv) = start(connection, request).await?;
    // A local caller keeps its write half alive so EOF means it disconnected.
    if matches!(connection, Connection::Remote(_)) {
        send.finish().map_err(|_| lost(connection, true))?;
    }
    let result = answer(connection, &mut recv, answer_within).await;
    drop(send);
    result
}

/// Opens a stream for one operation and sends its request. The send side
/// stays open, for operations that send file bytes next.
pub async fn start(
    connection: &Connection,
    request: &Request,
) -> Result<(SendStream, RecvStream), Failure> {
    let (mut send, recv) = connection
        .open_bi()
        .await
        .map_err(|_| lost(connection, false))?;
    let json = serde_json::to_vec(request).expect("a request serializes");
    wire::write_frame(&mut send, &json)
        .await
        .map_err(|_| lost(connection, true))?;
    Ok((send, recv))
}

/// Reads the host's next answer frame: its result, or its error as a
/// failure with the matching exit code.
pub async fn answer(
    connection: &Connection,
    recv: &mut RecvStream,
    within: Duration,
) -> Result<Value, Failure> {
    let json = match tokio::time::timeout(within, wire::read_frame(recv)).await {
        Ok(Ok(json)) => json,
        Ok(Err(_)) => return Err(lost(connection, true)),
        Err(_) => {
            let message = format!(
                "no answer within {}s; check before resending",
                within.as_secs()
            );
            return Err(Failure::new(Exit::Unknown, "no_answer", message));
        }
    };
    let response: Response = serde_json::from_slice(&json).map_err(|_| {
        let message = "the host's answer was not a valid response".to_string();
        Failure::new(Exit::Unknown, "bad_response", message)
    })?;
    response.into_result()
}

/// Where to dial: the host's key and relay, both from the Plow directory.
fn host_address(host: &Peer) -> Result<(EndpointId, RelayUrl), Failure> {
    let bad = || {
        Failure::failed(
            "bad_directory",
            format!("Plow's entry for {:?} is not dialable", host.name),
        )
    };
    let id: EndpointId = host.key.parse().map_err(|_| bad())?;
    let relay: RelayUrl = host
        .relay_url
        .as_deref()
        .ok_or_else(bad)?
        .parse()
        .map_err(|_| bad())?;
    Ok((id, relay))
}

/// The connection failed us. Either the host closed it because it refused
/// us, or it dropped: before the request went out (exit 3) or after (exit 5).
pub fn lost(connection: &Connection, sent: bool) -> Failure {
    if let Some(ConnectionError::ApplicationClosed(close)) = connection.close_reason()
        && close.error_code == VarInt::from_u32(wire::CLOSE_DENIED)
    {
        let message = String::from_utf8_lossy(&close.reason).to_string();
        return Failure::new(Exit::Denied, "caller_denied", message);
    }
    if sent {
        let message = "the connection dropped after the request was sent; check before resending";
        Failure::new(Exit::Unknown, "outcome_unknown", message.to_string())
    } else {
        let message = "the connection dropped before the request was sent";
        Failure::new(Exit::Unreachable, "host_unreachable", message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn closing_is_quick_and_the_host_hears_it() {
        // A host and a client on this machine, with no relay.
        let bind = |alpns: Vec<Vec<u8>>| {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(alpns)
                .bind()
        };
        let host = bind(vec![wire::ALPN.to_vec()]).await.unwrap();
        let client = bind(vec![]).await.unwrap();
        // The host must be accepting for the client's handshake to finish.
        let accepting = host.accept();
        let (accepted, connection) =
            tokio::join!(async { accepting.await.unwrap().await.unwrap() }, async {
                client.connect(host.addr(), wire::ALPN).await.unwrap()
            },);
        let link = Link {
            endpoint: Some(client),
            connection: connection.into(),
        };

        let started = Instant::now();
        link.close();
        closed().await;
        let took = started.elapsed();

        // The host learns the client closed with "done", rather than timing
        // the connection out later.
        let ended = tokio::time::timeout(Duration::from_secs(2), accepted.closed()).await;
        match ended {
            Ok(ConnectionError::ApplicationClosed(close)) => {
                assert_eq!(close.error_code, VarInt::from_u32(0));
                assert_eq!(&close.reason[..], b"done");
            }
            other => panic!("the host saw {other:?}"),
        }
        assert!(took < Duration::from_millis(300), "close took {took:?}");
        host.close().await;
    }
}
