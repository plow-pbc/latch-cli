//! `latch serve`: the host. It runs in the foreground (as launchd and other
//! service managers expect) until it is stopped.
//!
//! One `latch serve` runs per state folder; a second one refuses to start.
//! On start it checks its audit log, starts its iroh endpoint, and keeps
//! itself registered with Plow (see `heartbeat.rs`). It stops cleanly on
//! Ctrl-C (SIGINT) or SIGTERM, which launchd sends, telling Plow it is
//! going offline (waiting at most 2 seconds) before it exits. Every caller
//! is known by the key its connection is authenticated with, never by
//! anything the caller says. The host asks Plow who that key is when the connection
//! opens and again before every operation, and any failure to get a clear
//! "yes" (unknown key, another account's key, a revoked credential, Plow
//! unreachable) denies. Every allow and every deny goes into the audit log.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, RelayMode};
use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing::Instrument;
use tracing::field::{Empty, display};

use crate::audit::{self, AuditLog, Record};
use crate::credential;
use crate::dashboard::{self, Dashboard, Row};
use crate::exec;
use crate::failure::Failure;
use crate::files;
use crate::gate::rules::RuleFiles;
use crate::gate::{self, Gate, Gatekeeper};
use crate::heartbeat;
use crate::lifecycle::{self, Phase};
use crate::log::{self, log};
use crate::plow::{Identity, Plow};
use crate::state::{Config, StateDir};
use crate::vault::link;
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
    /// The host's vault, shared by every engine and profile.
    vault: std::sync::Arc<crate::vault::host::Vault>,
    /// Our own endpoint and name, which a phone link points at.
    endpoint: Endpoint,
    name: String,
    deny_exec: DenyExec,
    /// Judges agents' operations against the owner's chat and rules.
    gatekeeper: Gatekeeper,
    binary: Binary,
}

/// `--deny-exec`: which agents may not run `exec` here, by verified agent
/// uid, never by name. It only ever refuses; devices are never refused.
#[derive(Default, Debug)]
pub struct DenyExec {
    every_agent: bool,
    agents: Vec<String>,
}

impl DenyExec {
    pub fn from_flags(values: &[String]) -> DenyExec {
        DenyExec {
            every_agent: values.iter().any(|value| value == crate::cli::EVERY_AGENT),
            agents: values
                .iter()
                .filter(|value| *value != crate::cli::EVERY_AGENT)
                .cloned()
                .collect(),
        }
    }

    fn refuses(&self, identity: &Identity) -> bool {
        identity.kind == "agent"
            && (self.every_agent
                || identity
                    .agent_uid
                    .as_ref()
                    .is_some_and(|uid| self.agents.contains(uid)))
    }

    /// For the log at startup, if anything is refused.
    fn describe(&self) -> Option<String> {
        if self.every_agent {
            Some("exec: refused to every agent".to_string())
        } else if !self.agents.is_empty() {
            Some(format!(
                "exec: refused to agents {}",
                self.agents.join(", ")
            ))
        } else {
            None
        }
    }
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
pub async fn run(
    state: &StateDir,
    folders: &[PathBuf],
    show_dashboard: bool,
    deny_exec: DenyExec,
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
    let program = std::env::current_exe()
        .map_err(|err| Failure::failed("sandbox_unavailable", err.to_string()))?;
    // Held until this process ends, however it ends.
    let _lock = lock_state(&state_dir)?;
    // From here on, other processes see this host's steps (`lifecycle.rs`).
    lifecycle::record(&state.path, Phase::Starting, None)?;
    let result = run_locked(
        state,
        folders,
        show_dashboard,
        deny_exec,
        state_dir,
        program,
    )
    .await;
    let (phase, error) = match &result {
        Ok(()) => (Phase::Stopped, None),
        Err(failure) => (Phase::Error, Some(failure)),
    };
    if let Err(failure) = lifecycle::record(&state.path, phase, error) {
        log(&format!(
            "could not record the host's last step: {}",
            failure.message
        ));
    }
    result
}

/// `run`, once this serve holds the state folder.
async fn run_locked(
    state: &StateDir,
    folders: &[PathBuf],
    show_dashboard: bool,
    deny_exec: DenyExec,
    state_dir: PathBuf,
    program: PathBuf,
) -> Result<(), Failure> {
    // Named folders win; with none, the ones `latch shares set` saved.
    let saved;
    let folders = if folders.is_empty() {
        saved = state.load_shares()?;
        &saved[..]
    } else {
        folders
    };
    let workers = WorkerSetup {
        areas: shared_folders(folders)?,
        state_dir,
        program,
    };
    let rules = RuleFiles::new(&state.path, &workers.areas)?;
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
    let result = run_host(state, config, workers, dashboard.clone(), rules, deny_exec).await;
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
    rules: RuleFiles,
    deny_exec: DenyExec,
) -> Result<(), Failure> {
    let binary = Binary::at(&workers.program)?;
    let plow = Plow::new(&config.api_url, &credential::token(state)?);
    let gatekeeper = Gatekeeper::new(
        plow.clone(),
        Gatekeeper::provider_for_host(&state.path)
            .map_err(|why| Failure::failed("not_logged_in", why))?,
        Box::new(rules),
    );
    let audit = AuditLog::open(state.path.join("audit.ndjson"))?;
    log(&format!("audit log verified: {} records", audit.len()));
    let names: Vec<String> = workers
        .areas
        .iter()
        .map(|area| area.display().to_string())
        .collect();
    log(&format!("sharing: {}", names.join(", ")));
    if let Some(refused) = deny_exec.describe() {
        log(&refused);
    }

    // Tests run a relay of their own, so they need no internet.
    let relay_mode = match std::env::var("LATCH_TEST_RELAY_URL") {
        Err(_) => RelayMode::Default,
        Ok(url) => {
            let url = url.parse().map_err(|_| {
                let message = format!("LATCH_TEST_RELAY_URL {url:?} is not a relay URL");
                Failure::usage("bad_relay", message)
            })?;
            RelayMode::custom([url])
        }
    };
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(state.load_key()?)
        .relay_mode(relay_mode)
        .alpns(vec![wire::ALPN.to_vec(), link::ALPN.to_vec()])
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
    let name = config.peer.name.clone();
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
    let host = Arc::new(Host {
        plow,
        audit: Mutex::new(audit),
        workers,
        dashboard,
        vault,
        endpoint: endpoint.clone(),
        name,
        deny_exec,
        gatekeeper,
        binary,
    });
    lifecycle::record(&state.path, Phase::Serving, None)?;
    // `Box::pin` gives the stop signal a fixed place in memory, so the loop
    // can wait on the same future again each time round.
    let mut stop = Box::pin(stop_requested(terminate));
    loop {
        tokio::select! {
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    let connection = tracing::info_span!("connection", caller = Empty);
                    tokio::spawn(serve_connection(host.clone(), incoming).instrument(connection));
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

/// The latch binary this host started from, as it was on disk then. A host
/// starts its workers from this path, so once the file there changes (an
/// upgrade renames a new one over it, or rewrites it) the host refuses new
/// work and says to restart it. It does not restart itself: a restart would
/// drop open browser sessions and in-flight calls, and the new latch may
/// refuse this state folder, leaving the host silently gone.
struct Binary {
    path: PathBuf,
    on_disk: OnDisk,
    told: AtomicBool,
}

/// What tells one file at a path from another, or from itself rewritten.
#[derive(PartialEq)]
struct OnDisk {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
}

impl OnDisk {
    /// Follows a symlink, so one pointed at another latch counts as replaced.
    fn of(path: &Path) -> Option<OnDisk> {
        let meta = std::fs::metadata(path).ok()?;
        Some(OnDisk {
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.size(),
            modified: (meta.mtime(), meta.mtime_nsec()),
        })
    }
}

impl Binary {
    /// `path`, the latch the workers run, as it is on disk now.
    fn at(path: &Path) -> Result<Binary, Failure> {
        let on_disk = OnDisk::of(path).ok_or_else(|| {
            let message = format!("latch cannot find its own binary {}", path.display());
            Failure::failed("no_binary", message)
        })?;
        Ok(Binary {
            path: path.to_path_buf(),
            on_disk,
            told: AtomicBool::new(false),
        })
    }

    /// Whether the file is no longer the one this host started from (gone
    /// counts too). The host's log says so once.
    fn replaced(&self) -> bool {
        let replaced = OnDisk::of(&self.path).as_ref() != Some(&self.on_disk);
        if replaced && !self.told.swap(true, Ordering::Relaxed) {
            log(&format!(
                "{} was replaced since this host started: refusing every operation; \
                 restart `latch serve` to run the new latch",
                self.path.display()
            ));
        }
        replaced
    }
}

/// What every operation gets once the host's binary was replaced.
fn host_upgraded() -> Failure {
    let message = "latch on this host was replaced since `latch serve` started; nothing ran. \
                   Restart `latch serve` on the host to run the new latch, then try again";
    Failure::failed("host_upgraded", message.to_string())
}

/// Makes this the one `latch serve` on `state_dir`: two hosts on one state
/// would share one key and one audit log. The lock is an exclusive `flock` on
/// `serve.lock`, which the system releases when its holder exits, so one
/// left behind by a crash never blocks. The file says who holds it, for the
/// refusal a second serve prints. A lock held for less than a second is
/// another process asking whether a serve runs (`lifecycle::holds_lock`),
/// so this waits that long before refusing.
fn lock_state(state_dir: &Path) -> Result<File, Failure> {
    let path = state_dir.join("serve.lock");
    let io =
        |err: std::io::Error| Failure::failed("state_io", format!("{}: {err}", path.display()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(io)?;
    // SAFETY: flock only reads the descriptor, which `file` keeps open.
    let try_lock = || unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    let mut locked = try_lock();
    for _ in 0..20 {
        if locked {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        locked = try_lock();
    }
    if !locked {
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = holder.trim();
        let holder = if holder.is_empty() {
            "another process"
        } else {
            holder
        };
        let message = format!(
            "{} is already served by {holder}; stop that `latch serve` first",
            state_dir.display()
        );
        return Err(Failure::failed("already_serving", message));
    }
    let exe =
        std::env::current_exe().map_or_else(|_| "latch".into(), |exe| exe.display().to_string());
    let holder = format!("latch serve (pid {}, {exe})\n", std::process::id());
    file.set_len(0)
        .and_then(|()| file.write_all(holder.as_bytes()))
        .map_err(io)?;
    Ok(file)
}

/// The folders `latch serve` was given, which its workers may use: each
/// made absolute (a worker runs in another directory, the first folder) and
/// checked now, so a typo is a plain error rather than refused files later.
/// Any folder may be shared, the home folder and `/` included: the sandbox
/// always hides the state folder, even inside a shared one.
pub fn shared_folders(folders: &[PathBuf]) -> Result<Vec<PathBuf>, Failure> {
    if folders.is_empty() {
        let message = "name the folders to share, e.g. `latch serve ~/code`, or save them \
                       with `latch shares set ~/code`"
            .to_string();
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
    tracing::Span::current().record("caller", display(&caller));
    if connection.alpn() == link::ALPN {
        return serve_phone(&host, &caller, connection).await;
    }

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
        let operation = tracing::info_span!("operation", op = Empty);
        serve_operation(&host, &caller, send, recv)
            .instrument(operation)
            .await;
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
    tracing::Span::current().record("op", display(op));
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
            // An agent's exec, file or Google operation runs only if the
            // gatekeeper allows it.
            let gate = host.gate(&identity, request.chat.as_deref());
            // An exec this host's owner denies is refused without asking.
            let denied_exec = op == "exec" && host.deny_exec.refuses(&identity);
            // A worker started now would run the new latch under this old
            // host: nothing runs, and nothing is judged.
            let upgraded = host.binary.replaced();
            let gated = gate::gated(op, &request.args, &host.workers.areas[0])
                .filter(|_| !denied_exec && !upgraded);
            let unjudgeable = match (&gate, &gated) {
                (Ok(Some(_)), Some(gated)) => gated.unjudgeable.as_ref(),
                _ => None,
            };
            // A one-time action (send one email) is judged, run and
            // recorded before the same agent's next one is judged, so two
            // sent at once do not both run.
            let turn = match (&gate, &gated) {
                (Ok(Some(gate)), Some(gated)) if gated.once => Some(gate.one_at_a_time().await),
                _ => None,
            };
            let judgment = match (&gate, &gated) {
                (Ok(Some(gate)), Some(gated)) if unjudgeable.is_none() => {
                    Some(gate.judge(&gated.action).await)
                }
                _ => None,
            };
            let refusal = match (&gate, &judgment) {
                (Err(failure), _) => Some(failure),
                _ if unjudgeable.is_some() => unjudgeable,
                (_, Some(judgment)) => judgment.verdict.as_ref().err(),
                _ => None,
            };
            let refused = gated.as_ref().zip(refusal).map(|(gated, failure)| Done {
                response: Response::error(failure),
                audit_args: gated.audit.clone(),
                outcome: failure.code.clone(),
            });
            let done = if upgraded {
                let failure = host_upgraded();
                Done::failed(&failure.code, failure.message, Value::Null)
            } else if let Some(refused) = refused {
                refused
            } else {
                match op {
                    "file_get" => files::get(&host.workers, &request.args, &mut send).await,
                    "file_put" => {
                        files::put(&host.workers, &request.args, &mut send, &mut recv).await
                    }
                    "exec" if denied_exec => Done {
                        response: Response::Error {
                            exit: crate::failure::Exit::Denied,
                            details: Value::Null,
                            code: "exec_denied".to_string(),
                            message: "this host's owner does not let this agent run exec here \
                                      (latch serve --deny-exec); asking again will not help"
                                .to_string(),
                        },
                        // As a refused judgment keeps it: no stdin.
                        audit_args: gate::gated(op, &request.args, &host.workers.areas[0])
                            .map_or(Value::Null, |gated| gated.audit),
                        outcome: "exec_denied".to_string(),
                    },
                    "exec" => exec::run(&host.workers, &request.args).await,
                    "vault" => {
                        crate::vault::host::run(
                            &host.vault,
                            &host.here(),
                            &identity.kind,
                            request.args,
                            &mut send,
                            &mut recv,
                        )
                        .await
                    }
                    "rules" => {
                        crate::gate::owner::run(
                            &host.workers.state_dir,
                            &host.plow,
                            &host.name,
                            &identity.kind,
                            &request.args,
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
                }
            };
            if let (Ok(Some(gate)), Some(gated), Some(judgment)) = (&gate, &gated, &judgment)
                && judgment.verdict.is_ok()
                && gated.once
            {
                gate.did(&gated.action, &done.outcome);
            }
            drop(turn);
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
                    caller: host.shown_name(&identity).await,
                    op: op.to_string(),
                    target,
                    result,
                    refused: decision == "deny",
                });
            }
            let mut record = audit::record(caller, identity.agent_uid, op, decision, &done.outcome);
            record.args = done.audit_args;
            record.gate = judgment.map(|judgment| judgment.gate);
            match host.append(record) {
                Ok(audit_id) => with_audit_id(done.response, audit_id),
                Err(message) => failed("audit_failed", message),
            }
        }
    };
    answer(&mut send, &response).await;
}

/// A phone page's connection (see `vault/link.rs`). Its key is a
/// throwaway one Plow has never heard of: the link's code in each request
/// is what lets it in, so Plow is not asked. It can fetch its link's form
/// and save its link's item, nothing else.
async fn serve_phone(host: &Host, caller: &str, connection: Connection) {
    log(&format!(
        "phone {caller}: connected over {}",
        describe_paths(&connection)
    ));
    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
        let Ok(json) = wire::read_frame(&mut recv).await else {
            return;
        };
        let Ok(request) = serde_json::from_slice::<Request>(&json) else {
            let response = failed("bad_request", "the request frame is not valid".to_string());
            answer(&mut send, &response).await;
            continue;
        };
        let started = Instant::now();
        let mut audit_args = serde_json::Map::new();
        audit_args.insert("command".into(), Value::String(request.op.clone()));
        let result = match request.op.as_str() {
            // The link's item would be saved by this old host's code.
            _ if host.binary.replaced() => Err(host_upgraded()),
            "page" => link::page(&host.vault, &host.here(), &request.args, &mut audit_args),
            "submit" => link::submit(&host.vault, &request.args, &mut audit_args),
            op => Err(Failure::new(
                crate::failure::Exit::Denied,
                "not_on_a_phone_link",
                format!("a phone link cannot run `{op}`"),
            )),
        };
        let done = crate::vault::host::done(result, audit_args);
        let decision = decision(&done.response);
        log(&format!(
            "vault_link: phone {caller}: {decision}: {} {}",
            done.outcome, done.audit_args
        ));
        if let Some(dashboard) = &host.dashboard {
            let (target, result) = dashboard::describe(
                "vault_link",
                &done.audit_args,
                &done.outcome,
                started.elapsed(),
            );
            dashboard.call(Row {
                at: jiff::Timestamp::now(),
                caller: "phone link".to_string(),
                op: "vault_link".to_string(),
                target,
                result,
                refused: decision == "deny",
            });
        }
        let mut record = audit::record(caller, None, "vault_link", decision, &done.outcome);
        record.args = done.audit_args;
        let response = match host.append(record) {
            Ok(audit_id) => with_audit_id(done.response, audit_id),
            Err(message) => failed("audit_failed", message),
        };
        answer(&mut send, &response).await;
    }
}

/// The audit decision for an operation the caller check let through: a
/// refusal (such as the sandbox's) is a "deny", anything else an "allow" —
/// a failure that is not a refusal (a missing file) was allowed to run.
/// A worker that could not apply its sandbox is a "deny" too: the host
/// refused to run the operation unconfined, so it never ran. So is an
/// operation the gatekeeper could not judge.
pub(crate) fn decision(response: &Response) -> &'static str {
    match response {
        Response::Error {
            exit: crate::failure::Exit::Denied | crate::failure::Exit::Usage,
            ..
        } => "deny",
        Response::Error { code, .. }
            if code == "sandbox_failed"
                || code == "gatekeeper_unavailable"
                || code == "host_upgraded" =>
        {
            "deny"
        }
        _ => "allow",
    }
}

/// Adds the audit record's hash to a successful result as `audit_id`, so
/// the agent can point at exactly what the host recorded.
pub(crate) fn with_audit_id(response: Response, audit_id: String) -> Response {
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
    /// Where a phone link points: our key, name and current relay.
    fn here(&self) -> link::Here {
        link::Here {
            key: self.endpoint.id().to_string(),
            name: self.name.clone(),
            relay: self.endpoint.addr().relay_urls().next().cloned(),
        }
    }

    /// Asks Plow who `caller` is. Returns the caller's identity if it may
    /// call, or why not. Nothing is cached: every check asks Plow again, so
    /// a revoked key stops working at once.
    /// The gate an agent's operations pass, in the chat it named; none for
    /// the owner's own devices, which are never judged. An agent Plow
    /// gives no uid can't be judged, so nothing of it runs.
    fn gate<'a>(
        &'a self,
        identity: &'a Identity,
        chat: Option<&'a str>,
    ) -> Result<Option<Gate<'a>>, Failure> {
        if identity.kind == "device" {
            return Ok(None);
        }
        match identity.agent_uid.as_deref() {
            Some(agent) => Ok(Some(Gate {
                keeper: &self.gatekeeper,
                agent,
                chat,
            })),
            None => Err(Failure::new(
                crate::failure::Exit::Unreachable,
                "gatekeeper_unavailable",
                "The gatekeeper could not judge this, so it did not run: Plow names no agent for this caller".to_string(),
            )),
        }
    }

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

    /// The caller's name for people to read: an agent's current name in
    /// Plow, which follows a rename, else its peer name, which keeps the
    /// name it registered with.
    async fn shown_name(&self, identity: &Identity) -> String {
        if let Some(uid) = &identity.agent_uid
            && let Ok(agent) = self.plow.agent(uid).await
        {
            return agent.name;
        }
        identity.name.clone()
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
mod gated_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(kind: &str, name: &str, uid: Option<&str>) -> Identity {
        serde_json::from_value(serde_json::json!({
            "key": "k", "kind": kind, "name": name, "agent_uid": uid,
            "permissions": {"call": true},
        }))
        .unwrap()
    }

    #[test]
    fn deny_exec_follows_the_agent_uid_and_leaves_devices_alone() {
        let device = caller("device", "mac", None);
        let bot = caller("agent", "bot", Some("ab12"));
        let renamed = caller("agent", "Research bot", Some("ab12"));
        let other = caller("agent", "bot", Some("cd34"));

        let everyone = DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]);
        assert!(everyone.refuses(&bot) && everyone.refuses(&renamed) && everyone.refuses(&other));
        assert!(!everyone.refuses(&device));

        // Another agent taking the name "bot" is not refused; the renamed one is.
        let one = DenyExec::from_flags(&["ab12".to_string()]);
        assert!(one.refuses(&bot) && one.refuses(&renamed));
        assert!(!one.refuses(&other) && !one.refuses(&device));

        assert!(!DenyExec::default().refuses(&bot));
    }

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
        let upgraded = failed("host_upgraded", "restart the host".to_string());
        assert_eq!(decision(&upgraded), "deny");
    }

    #[test]
    fn a_binary_renamed_over_rewritten_or_removed_counts_as_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latch");
        let binary = |path: &Path| Binary::at(path).unwrap();
        std::fs::write(&path, b"old latch").unwrap();
        let running = binary(&path);
        assert!(!running.replaced());

        // How install.sh upgrades: a new file renamed over the old one.
        std::fs::write(dir.path().join("latch.tmp"), b"new latch").unwrap();
        std::fs::rename(dir.path().join("latch.tmp"), &path).unwrap();
        assert!(running.replaced());

        let running = binary(&path);
        std::fs::write(&path, b"new latch, again").unwrap();
        assert!(running.replaced());

        let running = binary(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(running.replaced());
    }

    #[test]
    fn one_serve_holds_a_state_folder_until_it_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock_state(dir.path()).unwrap();

        let refused = lock_state(dir.path()).unwrap_err();
        assert_eq!(refused.code, "already_serving");
        let pid = format!("pid {}", std::process::id());
        assert!(refused.message.contains(&pid), "{}", refused.message);

        drop(first);

        // What a crashed serve leaves: the file, naming it, but no lock.
        let crashed = tempfile::tempdir().unwrap();
        let stale = "latch serve (pid 999999, /old/latch)\n";
        std::fs::write(crashed.path().join("serve.lock"), stale).unwrap();
        let _held = lock_state(crashed.path()).unwrap();
        let now = std::fs::read_to_string(crashed.path().join("serve.lock")).unwrap();
        assert!(now.contains(&pid), "{now}");
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
