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

use crate::failure::{Failure, code};
use crate::plow::Peer;
use crate::state::{HeldKey, KeyUse, StateDir};
use crate::wire::{self, Request, Response};

/// An iroh endpoint as latch makes them, client or host. In tests
/// (`LATCH_TEST_LOOPBACK`, which the test harness sets, and this crate's own
/// tests) it binds 127.0.0.1 only and never probes the router for port
/// mappings, so a test touches nothing on the local network, and macOS
/// never asks to let latch find devices there.
pub fn endpoint() -> iroh::endpoint::Builder {
    let builder = Endpoint::builder(presets::Minimal);
    if !(cfg!(test) || std::env::var_os("LATCH_TEST_LOOPBACK").is_some()) {
        return builder;
    }
    builder
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")
        .expect("127.0.0.1:0 is a socket address")
        .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
}

/// How long to try to reach a host before giving up with exit 3.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for a host's answer to an operation without a time
/// limit of its own. For a file transfer, it is the wait before the bytes
/// start and after they end; the bytes themselves may take as long as they
/// keep flowing (a stalled connection ends on QUIC's idle timeout).
pub const ANSWER_WITHIN: Duration = Duration::from_secs(120);

/// Added to an operation's own time limit (such as `exec --timeout`) for the
/// host to stop it, report, and the answer to travel back.
pub const ANSWER_MARGIN: Duration = Duration::from_secs(30);

/// An open connection to a host, and the local endpoint it runs on, with
/// the key that endpoint holds.
pub struct Link {
    endpoint: Option<(Endpoint, Box<HeldKey>)>,
    pub connection: Connection,
}

/// Dials this state folder over its private socket, or another host over
/// iroh: through this machine's host when one serves here, else from an
/// endpoint of our own. Failure here means nothing was sent: exit 3.
pub async fn connect(state: &StateDir, host: &Peer) -> Result<Link, Failure> {
    connect_within(state, host, CONNECT_TIMEOUT).await
}

async fn connect_within(state: &StateDir, host: &Peer, within: Duration) -> Result<Link, Failure> {
    // Plow already knows the host is offline: say so now, rather than spend
    // the connect timeout finding out.
    if !host.this_machine && host.online == Some(false) {
        return Err(offline(host));
    }
    // This machine's own host is reached over its socket, with no secret:
    // the running host holds them.
    if host.this_machine {
        let socket = crate::transport::connect_local(&state.path).await?;
        return Ok(Link {
            endpoint: None,
            connection: Connection::Local(std::sync::Mutex::new(Some(socket))),
        });
    }
    let key = match state.use_key()? {
        KeyUse::Direct(key) => key,
        KeyUse::Host => {
            let proxied = crate::proxy::connect(state, host).await?;
            return Ok(Link {
                endpoint: None,
                connection: Connection::Proxied(proxied),
            });
        }
    };
    let (_, relay) = host_address(host)?;
    // Our own key is our identity: the host learns who we are from the
    // connection's handshake, not from anything we send. Our only relay is
    // the host's own, so a client contacts no relay the host did not choose.
    let endpoint = key
        .endpoint()
        .relay_mode(RelayMode::custom([relay]))
        .bind()
        .await
        .map_err(|err| Failure::new(code::iroh, format!("could not start iroh: {err}")))?;
    match connect_on(&endpoint, host, within).await {
        Ok(connection) => Ok(Link {
            endpoint: Some((endpoint, key)),
            connection: Connection::Remote(connection),
        }),
        Err(failure) => {
            // Nothing was sent, so there is nothing to drain: iroh would
            // otherwise keep the dial attempt going for seconds past `within`.
            let _ = tokio::time::timeout(CLOSE_WITHIN, endpoint.close()).await;
            Err(failure)
        }
    }
}

/// Connects `endpoint` to `host` within `within`, for a command's own
/// endpoint or for the host that carries its call (`proxy.rs`).
pub async fn connect_on(
    endpoint: &Endpoint,
    host: &Peer,
    within: Duration,
) -> Result<iroh::endpoint::Connection, Failure> {
    let (id, relay) = host_address(host)?;
    let address = EndpointAddr::new(id).with_relay_url(relay);
    match tokio::time::timeout(within, endpoint.connect(address, wire::ALPN)).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(err)) => Err(not_connected(host, &err)),
        Err(_) => Err(not_connected_within(host, within)),
    }
}

/// Plow lists `host` offline: it is not dialed.
fn offline(host: &Peer) -> Failure {
    let seen = match &host.last_seen_at {
        Some(at) => format!(", last seen {at}"),
        None => String::new(),
    };
    let message = format!(
        "host {:?} is not online{seen}; nothing was sent. Start `latch serve` there",
        host.name
    );
    Failure::new(code::host_offline, message)
}

/// Why dialing `host` failed: it speaks another protocol, or it could not
/// be reached.
fn not_connected(host: &Peer, err: &ConnectError) -> Failure {
    if speaks_another_version(err) {
        // Builds of one protocol version work together; only that differs.
        let message = format!(
            "host {:?} does not speak {}, the protocol of this latch {}, so one of \
             the two runs an older latch; nothing was sent. Compare `latch --version` on \
             both machines and update the older one",
            host.name,
            String::from_utf8_lossy(wire::ALPN),
            crate::cli::version(),
        );
        return Failure::new(code::protocol_mismatch, message);
    }
    tracing::info!(host = %host.name, error = ?err, "could not connect");
    let message = format!(
        "could not connect to host {:?}: {}; nothing was sent",
        host.name,
        reasons(err)
    );
    Failure::new(code::host_unreachable, message)
}

fn not_connected_within(host: &Peer, within: Duration) -> Failure {
    tracing::info!(host = %host.name, "no connection within {}s", within.as_secs_f32());
    let message = format!(
        "could not connect to host {:?} within {}s; nothing was sent",
        host.name,
        within.as_secs_f32()
    );
    Failure::new(code::host_unreachable, message)
}

/// Sends `request` to each of `hosts` at once, through one endpoint, and
/// gives `each` a host's answer (by its place in `hosts`) as it comes. All
/// of it, the endpoint's start included, is within `within`: a host with no
/// answer by then is `host_unreachable`, and none is dialed past it. A host
/// Plow lists offline is not dialed. Returns once every host has its answer
/// and the endpoint is closed.
pub async fn ask_each(
    state: &StateDir,
    hosts: &[Peer],
    request: &Request,
    within: Duration,
    mut each: impl FnMut(usize, Result<Value, Failure>),
) {
    let deadline = tokio::time::Instant::now() + within;
    let late = |host: &Peer| {
        let message = format!(
            "host {:?} did not answer within {}s",
            host.name,
            within.as_secs_f32()
        );
        Failure::new(code::host_unreachable, message)
    };
    let mut dialing = Vec::new();
    for (index, host) in hosts.iter().enumerate() {
        if host.online == Some(false) {
            each(index, Err(offline(host)));
            continue;
        }
        match host_address(host) {
            Ok(address) => dialing.push((index, address)),
            Err(failure) => each(index, Err(failure)),
        }
    }
    if dialing.is_empty() {
        return;
    }
    // Our relays are the dialed hosts' own, as for one host.
    let mut relays: Vec<RelayUrl> = dialing
        .iter()
        .map(|(_, (_, relay))| relay.clone())
        .collect();
    relays.sort();
    relays.dedup();
    let bound = match state.use_key() {
        Ok(KeyUse::Direct(key)) => {
            let binding = key.endpoint().relay_mode(RelayMode::custom(relays)).bind();
            match tokio::time::timeout_at(deadline, binding).await {
                Ok(Ok(endpoint)) => Ok(Some((endpoint, key))),
                Ok(Err(err)) => Err(Failure::new(
                    code::iroh,
                    format!("could not start iroh: {err}"),
                )),
                Err(_) => Err(Failure::new(
                    code::host_unreachable,
                    format!("could not start iroh within {}s", within.as_secs_f32()),
                )),
            }
        }
        // Each host is asked through this machine's host instead.
        Ok(KeyUse::Host) => Ok(None),
        Err(failure) => Err(failure),
    };
    let endpoint = match bound {
        Ok(endpoint) => endpoint,
        Err(failure) => {
            for (index, _) in dialing {
                each(
                    index,
                    Err(Failure::relayed(
                        failure.exit,
                        &failure.code,
                        failure.message.clone(),
                    )),
                );
            }
            return;
        }
    };
    let mut asking = tokio::task::JoinSet::new();
    for &(index, (id, ref relay)) in &dialing {
        let (request, host) = (request.clone(), hosts[index].clone());
        let endpoint = endpoint.as_ref().map(|(endpoint, _)| endpoint.clone());
        let state = StateDir {
            path: state.path.clone(),
        };
        let address = EndpointAddr::new(id).with_relay_url(relay.clone());
        asking.spawn(async move {
            let connected = match endpoint {
                Some(endpoint) => endpoint
                    .connect(address, wire::ALPN)
                    .await
                    .map(Connection::Remote)
                    .map_err(|err| not_connected(&host, &err)),
                None => crate::proxy::connect(&state, &host)
                    .await
                    .map(Connection::Proxied),
            };
            let connection = match connected {
                Ok(connection) => connection,
                Err(failure) => return (index, Err(failure)),
            };
            let answer = one_exchange(&connection, &request, within).await;
            connection.close(VarInt::from_u32(0), b"done");
            (index, answer)
        });
    }
    let mut unanswered: Vec<usize> = dialing.iter().map(|(index, _)| *index).collect();
    while let Ok(Some(joined)) = tokio::time::timeout_at(deadline, asking.join_next()).await {
        if let Ok((index, answer)) = joined {
            unanswered.retain(|&waiting| waiting != index);
            each(index, answer);
        }
    }
    // Past the deadline nothing goes on asking.
    asking.abort_all();
    for index in unanswered {
        each(index, Err(late(&hosts[index])));
    }
    if let Some((endpoint, _key)) = endpoint {
        let _ = tokio::time::timeout(CLOSE_WITHIN, endpoint.close()).await;
    }
}

/// An error and each error under it, outermost first: iroh's outer errors
/// alone ("Unable to connect to remote") don't say what went wrong.
fn reasons(err: &dyn std::error::Error) -> String {
    let mut reasons = vec![err.to_string()];
    let mut source = err.source();
    while let Some(err) = source {
        let reason = err.to_string();
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
        source = err.source();
    }
    reasons.join(": ")
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
        self.connection.close(VarInt::from_u32(0), b"done");
        if let Some((endpoint, key)) = self.endpoint {
            let closing = tokio::spawn(async move {
                let _ = tokio::time::timeout(CLOSE_WITHIN, endpoint.close()).await;
                // A host may take the key once our endpoint is gone.
                drop(key);
            });
            CLOSING.lock().expect("closing lock").push(closing);
        }
    }

    /// Whether this link has an endpoint of its own, holding the key.
    pub fn direct(&self) -> bool {
        self.endpoint.is_some()
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
    if !matches!(connection, Connection::Local(_)) {
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
            return Err(Failure::new(code::no_answer, message));
        }
    };
    let response: Response = serde_json::from_slice(&json).map_err(|_| {
        let message = "the host's answer was not a valid response".to_string();
        Failure::new(code::bad_response, message)
    })?;
    response.into_result()
}

/// Where to dial: the host's key and relay, both from the Plow directory.
fn host_address(host: &Peer) -> Result<(EndpointId, RelayUrl), Failure> {
    let bad = || {
        Failure::new(
            code::bad_directory,
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
    if let Some(message) = connection.denied() {
        return Failure::new(code::caller_denied, message);
    }
    if sent {
        let message = "the connection dropped after the request was sent; check before resending";
        Failure::new(code::outcome_unknown, message.to_string())
    } else {
        let message = "the connection dropped before the request was sent";
        Failure::new(code::host_unreachable, message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_test_endpoint_is_on_loopback_alone() {
        let endpoint = endpoint()
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let bound = endpoint.bound_sockets();
        assert!(!bound.is_empty());
        assert!(
            bound.iter().all(|socket| socket.ip().is_loopback()),
            "{bound:?}"
        );
        let addr = endpoint.addr();
        let advertised: Vec<_> = addr.ip_addrs().collect();
        assert!(
            advertised.iter().all(|socket| socket.ip().is_loopback()),
            "{advertised:?}"
        );
        endpoint.close().await;
    }
    use std::time::Instant;

    #[tokio::test]
    async fn a_host_plow_lists_offline_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir {
            path: dir.path().to_path_buf(),
        };
        let host = Peer {
            key: "c".repeat(64),
            kind: "device".into(),
            name: "mac".into(),
            relay_url: Some("https://relay.example/".into()),
            last_seen_at: Some("2026-10-22T01:00:00Z".into()),
            online: Some(false),
            this_machine: false,
        };
        let started = Instant::now();
        let failure = connect(&state, &host).await.err().unwrap();
        assert_eq!(failure.code, "host_offline");
        assert!(
            failure
                .message
                .contains("is not online, last seen 2026-10-22T01:00:00Z"),
            "{}",
            failure.message
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A state folder with a key, and a host at `key` behind `relay`.
    fn dialing(key: Option<String>, relay: &str) -> (tempfile::TempDir, StateDir, Peer) {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir {
            path: dir.path().to_path_buf(),
        };
        let (own, _) = state.load_or_create_key().unwrap();
        let host = Peer {
            key: key.unwrap_or_else(|| own.to_string()),
            kind: "device".into(),
            name: "mac".into(),
            relay_url: Some(relay.into()),
            last_seen_at: None,
            online: Some(true),
            this_machine: false,
        };
        (dir, state, host)
    }

    /// The `latch` events logged while `run` runs, as `LATCH_TRACE` would write them.
    async fn traced<T>(run: impl Future<Output = T>) -> (T, String) {
        #[derive(Clone, Default)]
        struct Events(std::sync::Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Events {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let events = Events::default();
        let writer = events.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("latch=info")
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let result = run.await;
        let logged = String::from_utf8(events.0.lock().unwrap().clone()).unwrap();
        (result, logged)
    }

    #[tokio::test]
    async fn a_failed_connection_says_why() {
        // Dialing our own key fails at once, with iroh's reason.
        let (_dir, state, host) = dialing(None, "http://127.0.0.1:9/");
        let (dialed, logged) = traced(connect(&state, &host)).await;
        let failure = dialed.err().unwrap();
        assert_eq!(failure.code, "host_unreachable");
        assert_eq!(
            failure.message,
            "could not connect to host \"mac\": Connecting to ourself is not supported; \
             nothing was sent"
        );
        assert!(logged.contains("could not connect"), "{logged}");
        assert!(logged.contains("error=Connecting to ourself"), "{logged}");
    }

    // On a paused clock, time passes only for the waits themselves, so a
    // busy machine that is slow to bind or schedule doesn't count.
    #[tokio::test(start_paused = true)]
    async fn a_connection_that_never_comes_says_it_timed_out() {
        // Nothing answers at the host's relay, so the dial runs out of time.
        let other = iroh::SecretKey::generate().public().to_string();
        let (_dir, state, host) = dialing(Some(other), "http://127.0.0.1:9/");
        let within = Duration::from_millis(300);
        let started = tokio::time::Instant::now();
        let (dialed, logged) = traced(connect_within(&state, &host, within)).await;
        // The time the message gives is the time it took.
        let took = started.elapsed();
        assert!(took < within + Duration::from_secs(1), "took {took:?}");
        let failure = dialed.err().unwrap();
        assert_eq!(failure.code, "host_unreachable");
        assert_eq!(
            failure.message,
            "could not connect to host \"mac\" within 0.3s; nothing was sent"
        );
        assert!(logged.contains("no connection within 0.3s"), "{logged}");
    }

    #[tokio::test]
    async fn a_new_client_cannot_connect_to_an_old_protocol_host() {
        let host = endpoint()
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![b"plow/latch/1".to_vec()])
            .bind()
            .await
            .unwrap();
        let client = endpoint()
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let (accepted, connection) = tokio::join!(
            async { host.accept().await.unwrap().await },
            client.connect(host.addr(), wire::ALPN),
        );
        assert!(accepted.is_err());
        assert!(connection.is_err());
        client.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn closing_is_quick_and_the_host_hears_it() {
        // A host and a client on this machine, with no relay.
        let bind = |alpns: Vec<Vec<u8>>| {
            endpoint()
                .relay_mode(RelayMode::Disabled)
                .alpns(alpns)
                .bind()
        };
        let host = bind(vec![wire::ALPN.to_vec()]).await.unwrap();
        let (_dir, state, _) = dialing(None, "http://127.0.0.1:9/");
        let Ok(KeyUse::Direct(key)) = state.use_key() else {
            panic!("no host serves here")
        };
        let client = key
            .endpoint()
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        // The host must be accepting for the client's handshake to finish.
        let accepting = host.accept();
        let (accepted, connection) =
            tokio::join!(async { accepting.await.unwrap().await.unwrap() }, async {
                client.connect(host.addr(), wire::ALPN).await.unwrap()
            },);
        let link = Link {
            endpoint: Some((client, key)),
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
