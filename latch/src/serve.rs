//! `latch serve`: the host. It runs in the foreground (as launchd and other
//! service managers expect) until it is stopped.
//!
//! On start it checks its audit log, starts its iroh endpoint, and keeps
//! itself registered with Plow (see `heartbeat.rs`). It stops cleanly on
//! Ctrl-C (SIGINT) or SIGTERM, which launchd sends, telling Plow it is
//! going offline (waiting at most 2 seconds) before it exits. Every caller
//! is known by the key its connection is authenticated with, never by
//! anything the caller says. The host asks Plow who that key is when the connection
//! opens and again before every operation, and any failure to get a clear
//! "yes" (unknown key, another account's key, a revoked credential, Plow
//! unreachable) denies. Every allow and every deny goes into the audit log.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, RelayMode};
use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::audit::{self, AuditLog, Record};
use crate::credential;
use crate::dashboard::{self, Dashboard, Row};
use crate::exec;
use crate::failure::Failure;
use crate::files;
use crate::heartbeat;
use crate::log::{self, log};
use crate::plow::{Identity, Plow};
use crate::state::{Config, StateDir};
use crate::wire::{self, Request, Response};
use crate::worker::WorkerSetup;

/// What every connection handler shares. `Arc` lets many tasks hold the
/// same `Host`; the `Mutex` lets one task at a time append to the log.
struct Host {
    plow: Plow,
    audit: Mutex<AuditLog>,
    /// The live view, when `serve` shows one (see `dashboard.rs`).
    dashboard: Option<Dashboard>,
    workers: WorkerSetup,
    /// Open browser sessions, one per profile.
    browsers: crate::browser::host::Browsers,
    /// The host's vault, shared by every engine and profile.
    vault: std::sync::Arc<crate::vault::host::Vault>,
}

/// How an allowed operation ended: the final answer for the client, and
/// what goes into its audit record (`args` and `outcome`).
pub struct Done {
    pub response: Response,
    pub audit_args: Value,
    pub outcome: String,
}

impl Done {
    pub fn failed(code: &str, message: String, audit_args: Value) -> Done {
        Done {
            response: failed(code, message),
            audit_args,
            outcome: code.to_string(),
        }
    }
}

/// How long the host waits for a relay connection when it starts.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);

/// Runs the host until it is asked to stop. With `show_dashboard`, a live
/// view takes the terminal and the log goes to `serve.log` in the state
/// directory; the terminal is given back however the host stops.
/// With `headless_browsers`, every browser session runs with no window.
pub async fn run(
    state: &StateDir,
    folders: &[PathBuf],
    show_dashboard: bool,
    headless_browsers: bool,
) -> Result<(), Failure> {
    // A host that could not sandbox its workers would refuse every call;
    // say so once, now, instead of starting. The quick check names a
    // missing kernel feature; the probe worker then installs the sandbox
    // for real, which is the only way to know it can be.
    crate::sandbox::check().map_err(|message| Failure::failed("sandbox_unavailable", message))?;
    // The sandbox needs the state directory's real path (symlinks resolved).
    // One that does not exist yet has never logged in: say that, rather
    // than let the sandbox fail on the missing folder.
    let state_dir = match state.path.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            state.require_login()?;
            state.path.clone()
        }
    };
    let workers = WorkerSetup {
        areas: shared_folders(folders)?,
        state_dir,
    };
    workers.probe().await.map_err(|err| {
        let message = format!("latch cannot sandbox host operations here: {}", err.message);
        Failure::failed("sandbox_unavailable", message)
    })?;
    log(&format!("worker sandbox: {}", crate::sandbox::name()));
    let config = state.require_login()?;
    let dashboard = if show_dashboard {
        let path = state.path.join("serve.log");
        log::to_file(&path)
            .map_err(|err| Failure::failed("log_file", format!("{}: {err}", path.display())))?;
        Some(Dashboard::start(&config.peer.name))
    } else {
        None
    };
    let result = run_host(state, config, workers, dashboard.clone(), headless_browsers).await;
    if let Some(dashboard) = &dashboard {
        dashboard.stop();
    }
    result
}

async fn run_host(
    state: &StateDir,
    config: Config,
    workers: WorkerSetup,
    dashboard: Option<Dashboard>,
    headless_browsers: bool,
) -> Result<(), Failure> {
    let plow = Plow::new(&config.api_url, &credential::token(state)?);
    let audit = AuditLog::open(state.path.join("audit.ndjson"))?;
    log(&format!("audit log verified: {} records", audit.len()));
    let names: Vec<String> = workers
        .areas
        .iter()
        .map(|area| area.display().to_string())
        .collect();
    log(&format!("sharing: {}", names.join(", ")));

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(state.load_key()?)
        .relay_mode(RelayMode::Default)
        .alpns(vec![wire::ALPN.to_vec()])
        .bind()
        .await
        .map_err(|err| Failure::failed("iroh", format!("could not start iroh: {err}")))?;
    log(&format!(
        "serving as {:?}, key {}",
        config.peer.name,
        endpoint.id()
    ));

    // Clients dial us through our relay, so register only once we have one.
    if tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
        .await
        .is_err()
    {
        endpoint.close().await;
        let message = "no iroh relay connection within 15 seconds".to_string();
        return Err(Failure::failed("relay_unreachable", message));
    }
    let terminate = signal(SignalKind::terminate())
        .map_err(|err| Failure::failed("signal", format!("cannot listen for SIGTERM: {err}")))?;
    let heartbeat_state = StateDir {
        path: state.path.clone(),
    };
    let mut heartbeat = tokio::spawn(heartbeat::run(
        plow.clone(),
        endpoint.clone(),
        heartbeat_state,
        config,
        dashboard.clone(),
    ));

    let vault = crate::vault::host::Vault::new(workers.state_dir.clone());
    if headless_browsers {
        log("browsers: headless, with no window");
    }
    let browsers = crate::browser::host::Browsers::new(
        workers.state_dir.clone(),
        vault.clone(),
        headless_browsers,
    );
    let host = Arc::new(Host {
        plow,
        audit: Mutex::new(audit),
        workers,
        dashboard,
        browsers,
        vault,
    });
    // `Box::pin` gives the stop signal a fixed place in memory, so the loop
    // can wait on the same future again each time round.
    let mut stop = Box::pin(stop_requested(terminate));
    loop {
        tokio::select! {
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    tokio::spawn(serve_connection(host.clone(), incoming));
                }
                None => break,
            },
            () = &mut stop => {
                log("stopping: signal received");
                break;
            }
            // The heartbeat only ends when Plow gave our name to another
            // key. That key is not ours any more, so there is nothing to
            // take offline.
            ended = &mut heartbeat => {
                endpoint.close().await;
                log("stopped");
                return Err(ended.unwrap_or_else(|err| {
                    Failure::failed("heartbeat", format!("the heartbeat stopped: {err}"))
                }));
            }
        }
    }
    // Stop heartbeating first, so no late heartbeat lists us online again.
    heartbeat.abort();
    host.browsers.close_all().await;
    go_offline(&host.plow, &endpoint.id().to_string()).await;
    endpoint.close().await;
    log("stopped");
    Ok(())
}

/// Longest a stopping host waits for Plow: stopping never hangs on Plow.
const OFFLINE_TIMEOUT: Duration = Duration::from_secs(2);

/// Tells Plow once that this host is going offline, so clients see it
/// offline now rather than when its heartbeat runs out. Any failure is
/// only logged: the listing then catches up when the heartbeat expires.
async fn go_offline(plow: &Plow, key: &str) {
    match tokio::time::timeout(OFFLINE_TIMEOUT, plow.go_offline(key)).await {
        Ok(Ok(_)) => log("told Plow we are offline"),
        Ok(Err(failure)) => log(&format!(
            "could not tell Plow we are offline: {}",
            failure.message
        )),
        Err(_) => log("could not tell Plow we are offline: no answer within 2 seconds"),
    }
}

/// The folders `latch serve` was given, which its workers may use: each
/// made absolute (a worker runs in another directory, the first folder) and
/// checked now, so a typo is a plain error rather than refused files later.
/// Any folder may be shared, the home folder and `/` included: the sandbox
/// always hides the state folder, even inside a shared one.
fn shared_folders(folders: &[PathBuf]) -> Result<Vec<PathBuf>, Failure> {
    if folders.is_empty() {
        let message = "name the folders to share, e.g. `latch serve ~/code`".to_string();
        return Err(Failure::usage("no_folders", message));
    }
    let mut areas = Vec::new();
    for folder in folders {
        let refuse = |why: &str| {
            let message = format!("cannot share {}: {why}", folder.display());
            Failure::usage("bad_folder", message)
        };
        let area = folder
            .canonicalize()
            .map_err(|_| refuse("it does not exist"))?;
        if !area.is_dir() {
            return Err(refuse("it is not a folder"));
        }
        areas.push(area);
    }
    Ok(areas)
}

/// Finishes when the host is asked to stop: Ctrl-C (SIGINT) in a terminal,
/// or SIGTERM from launchd and other service managers.
async fn stop_requested(mut terminate: Signal) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

async fn serve_connection(host: Arc<Host>, incoming: Incoming) {
    let Ok(accepting) = incoming.accept() else {
        return;
    };
    let Ok(connection) = accepting.await else {
        return;
    };
    // The caller's key, proven by the connection's own handshake.
    let caller = connection.remote_id().to_string();

    // First check: on connect. A refused caller's connection is closed with
    // the "denied" code, which its client reports as exit 4.
    let refusal = match host.check(&caller).await {
        Ok(identity) => {
            let who = format!("{} ({})", identity.name, identity.kind);
            let record =
                audit::record(&caller, identity.agent_uid, "connect", "allow", "connected");
            match host.append(record) {
                Ok(_) => {
                    log(&format!("connect: {who} {caller}: allowed"));
                    None
                }
                Err(message) => Some(message),
            }
        }
        Err(denial) => {
            // Already refusing, so a failed audit write changes nothing here.
            let record = audit::record(&caller, None, "connect", "deny", &denial.code);
            let _ = host.append(record);
            host.show_refusal(&caller, "connect", &denial.code);
            Some(denial.message)
        }
    };
    if let Some(message) = refusal {
        log(&format!("connect: {caller}: denied: {message}"));
        connection.close(VarInt::from_u32(wire::CLOSE_DENIED), message.as_bytes());
        return;
    }

    // One stream per operation, until the client closes the connection.
    while let Ok((send, recv)) = connection.accept_bi().await {
        serve_operation(&host, &caller, send, recv).await;
        log(&format!(
            "paths to {caller}: {}",
            describe_paths(&connection)
        ));
    }
}

async fn serve_operation(host: &Host, caller: &str, mut send: SendStream, mut recv: RecvStream) {
    let Ok(json) = wire::read_frame(&mut recv).await else {
        return;
    };
    let Ok(request) = serde_json::from_slice::<Request>(&json) else {
        let response = failed("bad_request", "the request frame is not valid".to_string());
        return answer(&mut send, &response).await;
    };
    let op = request.op.as_str();
    let started = Instant::now();

    // Second check: before the operation, in case the caller was revoked
    // since it connected.
    let response = match host.check(caller).await {
        Err(denial) => {
            let _ = host.append(audit::record(caller, None, op, "deny", &denial.code));
            log(&format!("{op}: {caller}: denied: {}", denial.message));
            host.show_refusal(caller, op, &denial.code);
            Response::Error {
                exit: crate::failure::Exit::Denied,
                details: Value::Null,
                code: "caller_denied".to_string(),
                message: denial.message,
            }
        }
        Ok(identity) => {
            let done = match op {
                "file_get" => files::get(&host.workers, &request.args, &mut send).await,
                "file_put" => files::put(&host.workers, &request.args, &mut send, &mut recv).await,
                "exec" => exec::run(&host.workers, &request.args).await,
                "vault" => {
                    crate::vault::host::run(
                        &host.vault,
                        caller,
                        &identity.kind,
                        request.args,
                        &mut send,
                        &mut recv,
                    )
                    .await
                }
                "browser" => {
                    crate::browser::host::run(
                        &host.browsers,
                        caller,
                        &identity.kind,
                        request.args,
                        &mut send,
                        &mut recv,
                    )
                    .await
                }
                "google" => {
                    crate::google::host::run(
                        &host.workers,
                        &host.plow,
                        request.args,
                        &mut send,
                        &mut recv,
                    )
                    .await
                }
                _ => Done::failed(
                    "not_built_yet",
                    format!("this host does not run `{op}` yet"),
                    Value::Null,
                ),
            };
            let decision = decision(&done.response);
            log(&format!(
                "{op}: {caller}: {decision}: {} {}",
                done.outcome, done.audit_args
            ));
            if let Some(dashboard) = &host.dashboard {
                let (target, result) =
                    dashboard::describe(op, &done.audit_args, &done.outcome, started.elapsed());
                dashboard.call(Row {
                    at: jiff::Timestamp::now(),
                    caller: identity.name.clone(),
                    op: op.to_string(),
                    target,
                    result,
                    refused: decision == "deny",
                });
            }
            let mut record = audit::record(caller, identity.agent_uid, op, decision, &done.outcome);
            record.args = done.audit_args;
            match host.append(record) {
                Ok(audit_id) => with_audit_id(done.response, audit_id),
                Err(message) => failed("audit_failed", message),
            }
        }
    };
    answer(&mut send, &response).await;
}

/// The audit decision for an operation the caller check let through: a
/// refusal (such as the sandbox's) is a "deny", anything else an "allow" —
/// a failure that is not a refusal (a missing file) was allowed to run.
/// A worker that could not apply its sandbox is a "deny" too: the host
/// refused to run the operation unconfined, so it never ran.
fn decision(response: &Response) -> &'static str {
    match response {
        Response::Error {
            exit: crate::failure::Exit::Denied | crate::failure::Exit::Usage,
            ..
        } => "deny",
        Response::Error { code, .. } if code == "sandbox_failed" => "deny",
        _ => "allow",
    }
}

/// Adds the audit record's hash to a successful result as `audit_id`, so
/// the agent can point at exactly what the host recorded.
fn with_audit_id(response: Response, audit_id: String) -> Response {
    match response {
        Response::Ok(Value::Object(mut result)) => {
            result.insert("audit_id".to_string(), Value::String(audit_id));
            Response::Ok(Value::Object(result))
        }
        other => other,
    }
}

fn failed(code: &str, message: String) -> Response {
    Response::Error {
        exit: crate::failure::Exit::Failed,
        details: Value::Null,
        code: code.to_string(),
        message,
    }
}

/// Sends the one response frame and ends our side of the stream.
async fn answer(send: &mut SendStream, response: &Response) {
    let json = serde_json::to_vec(response).expect("a response serializes");
    if wire::write_frame(send, &json).await.is_ok() {
        let _ = send.finish();
    }
}

/// Why a caller was refused: a short code for the audit log, and a message
/// for the caller and the host's log.
struct Denial {
    code: String,
    message: String,
}

impl Host {
    /// Asks Plow who `caller` is. Returns the caller's identity if it may
    /// call, or why not. Nothing is cached: every check asks Plow again, so
    /// a revoked key stops working at once.
    async fn check(&self, caller: &str) -> Result<Identity, Denial> {
        match self.plow.lookup(caller).await {
            Ok(identity) if identity.permissions.call => Ok(identity),
            Ok(_) => Err(Denial {
                code: "not_permitted".to_string(),
                message: "Plow does not let this caller call hosts".to_string(),
            }),
            Err(failure) => Err(Denial {
                message: denial_message(&failure.code),
                code: failure.code,
            }),
        }
    }

    /// A refused caller as a plain dashboard row, if there is a dashboard.
    fn show_refusal(&self, caller: &str, op: &str, code: &str) {
        if let Some(dashboard) = &self.dashboard {
            dashboard.call(Row {
                at: jiff::Timestamp::now(),
                caller: format!("{}…", &caller[..8.min(caller.len())]),
                op: op.to_string(),
                target: String::new(),
                result: format!("refused: {code}"),
                refused: true,
            });
        }
    }

    /// Appends to the audit log and returns the record's hash. On failure,
    /// returns what the caller is told.
    fn append(&self, record: Record) -> Result<String, String> {
        let result = self.audit.lock().expect("audit log lock").append(record);
        result.map_err(|failure| {
            log(&format!("audit: {}", failure.message));
            "the host could not write its audit log".to_string()
        })
    }
}

/// What a denied caller is told, by the reason Plow's lookup failed.
fn denial_message(code: &str) -> String {
    let message = match code {
        "not_found" => "this key is not a live peer of the host's account",
        "plow_unreachable" => "the host could not reach Plow to check the caller",
        _ => "the host could not check the caller with Plow",
    };
    format!("{message} ({code})")
}

/// The connection's network paths, e.g.
/// `*Ip(192.168.1.20:52313) rtt 1ms, Relay(https://…) rtt 40ms`, where `*`
/// marks the one in use. An `Ip` path is direct; `Relay` goes via the relay.
fn describe_paths(connection: &Connection) -> String {
    let paths: Vec<String> = connection
        .paths()
        .iter()
        .map(|path| {
            let selected = if path.is_selected() { "*" } else { "" };
            let rtt = path.rtt().as_millis();
            format!("{selected}{:?} rtt {rtt}ms", path.remote_addr())
        })
        .collect();
    paths.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_audited_as_deny() {
        let refused = Response::Error {
            exit: crate::failure::Exit::Denied,
            details: Value::Null,
            code: "sandbox_denied".to_string(),
            message: "/etc/hosts: Operation not permitted".to_string(),
        };
        assert_eq!(decision(&refused), "deny");
        assert_eq!(decision(&Response::Ok(Value::Null)), "allow");
        assert_eq!(decision(&failed("not_found", "gone".to_string())), "allow");
        let unconfined = failed(
            "sandbox_failed",
            "the sandbox could not be applied".to_string(),
        );
        assert_eq!(decision(&unconfined), "deny");
    }

    #[test]
    fn any_existing_folder_is_shared() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let code = home.join("code");
        let docs = home.join("docs");
        for path in [&code, &docs] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(home.join("notes.txt"), b"").unwrap();

        assert_eq!(
            shared_folders(&[code.join("../docs"), code.clone()]).unwrap(),
            [docs, code]
        );
        // The home folder and `/` too: the state folder inside is hidden by the sandbox.
        assert_eq!(
            shared_folders(&[home.clone(), PathBuf::from("/")]).unwrap(),
            [home.clone(), PathBuf::from("/")]
        );
        assert_eq!(shared_folders(&[]).unwrap_err().code, "no_folders");
        for (folder, why) in [
            (home.join("missing"), "does not exist"),
            (home.join("notes.txt"), "not a folder"),
        ] {
            let failure = shared_folders(std::slice::from_ref(&folder)).unwrap_err();
            assert_eq!(failure.code, "bad_folder", "{folder:?}");
            assert!(
                failure.message.contains(why),
                "{folder:?}: {}",
                failure.message
            );
        }
    }

    #[test]
    fn successful_results_carry_their_audit_id() {
        let ok = Response::Ok(serde_json::json!({"path": "/tmp/a"}));
        let Response::Ok(result) = with_audit_id(ok, "abc".to_string()) else {
            panic!("still ok");
        };
        assert_eq!(result["audit_id"], "abc");
        assert_eq!(result["path"], "/tmp/a");

        let failure = with_audit_id(failed("x", "y".to_string()), "abc".to_string());
        assert!(matches!(
            failure,
            Response::Error {
                exit: crate::failure::Exit::Failed,
                details: Value::Null,
                ..
            }
        ));
    }
}
