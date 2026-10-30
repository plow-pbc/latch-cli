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

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, Incoming, VarInt};
use iroh::{Endpoint, RelayMode, Watcher};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tracing::Instrument;
use tracing::field::{Empty, display};

use crate::audit::{self, AuditLog, Record};
use crate::credential;
use crate::dashboard::{self, Dashboard};
use crate::exec;
use crate::failure::{Code, Failure, code};
use crate::files;
use crate::gate::rules::RuleFiles;
use crate::gate::{self, Caller, Gate, Gatekeeper};
use crate::heartbeat;
use crate::lifecycle::{self, Phase};
use crate::log::log;
use crate::plow::{Identity, Plow};
use crate::state::{HeldKey, StateDir};
use crate::vault::link;
use crate::wire::{self, Request, Response};
use crate::worker::WorkerSetup;
pub use control::DETACH;
use control::Lifetime;

/// What every connection handler shares. `Arc` lets many tasks hold the
/// same `Host`; the `Mutex` lets one task at a time append to the log.
struct Host {
    plow: Plow,
    /// Also held by the blocking thread that appends (see `Host::append`).
    audit: Arc<Mutex<AuditLog>>,
    /// The board viewers of this host watch (see `dashboard.rs`).
    dashboard: Dashboard,
    /// Whose life this host's is, and how it is asked to stop (see
    /// `serve/control.rs`).
    lifetime: Lifetime,
    workers: WorkerSetup,
    /// Open browser sessions, each in a browser of its own.
    browsers: crate::browser::host::Browsers,
    /// The host's vault, shared by every session.
    vault: std::sync::Arc<crate::vault::host::Vault>,
    /// Our own endpoint and name, which a phone link points at.
    endpoint: Endpoint,
    name: String,
    deny_exec: DenyExec,
    /// Judges agents' operations against the owner's chat and rules.
    gatekeeper: Gatekeeper,
    binary: Binary,
    /// The connectors its owner turned on, and their running operations.
    plugins: crate::plugins::Plugins,
    /// The owner's home folder, whose Messages and address book
    /// `messages` and `contacts` read.
    home: PathBuf,
    /// How far the host is into stopping.
    stopping: watch::Sender<Stopping>,
    /// Each agent's name in Plow as last looked up, by agent uid: what
    /// its audit records are named by, since an audit never waits on Plow.
    agent_names: Mutex<HashMap<String, String>>,
}

/// What a request's checks read of a host: a serving host's own, or this
/// machine's, for a preview judged in the previewing process
/// (`rules_try::preview_here`).
struct Checks<'a> {
    name: &'a str,
    plow: &'a Plow,
    workers: &'a WorkerSetup,
    gatekeeper: &'a Gatekeeper,
    deny_exec: &'a DenyExec,
    binary: &'a Binary,
    /// `None` for a preview by the rules alone, whatever connectors are on.
    plugins: Option<&'a crate::plugins::Plugins>,
}

impl<'a> Checks<'a> {
    /// The gate a caller's operations pass: the owner's own devices as
    /// the owner, an agent in the chat it named. An agent Plow gives no uid
    /// can't be judged, so nothing of it runs.
    fn gate(
        &self,
        identity: &'a Identity,
        local: bool,
        chat: Option<&'a str>,
    ) -> Result<Gate<'a>, Failure> {
        if local || identity.kind == "device" {
            if chat.is_some() {
                return Err(Failure::new(
                    code::plow_chat_not_supported,
                    "--plow-chat and LATCH_PLOW_CHAT select an agent's Plow chat; omit them on owner calls".into(),
                ));
            }
            return Ok(Gate {
                keeper: self.gatekeeper,
                caller: if local {
                    Caller::OwnerOnHost
                } else {
                    Caller::Owner
                },
            });
        }
        match identity.agent_uid.as_deref() {
            Some(uid) => Ok(Gate {
                keeper: self.gatekeeper,
                caller: Caller::Agent { uid, chat },
            }),
            None => Err(Failure::new(
                code::gatekeeper_unavailable,
                "The gatekeeper could not judge this, so it did not run: Plow names no agent for this caller".to_string(),
            )),
        }
    }
}

/// How far a host is into stopping.
#[derive(Clone, Copy, PartialEq)]
enum Stopping {
    No,
    /// Each connection finishes the operation it is on and takes no other.
    Draining,
    /// Running execs are killed, with all they started, and audited as
    /// `interrupted`.
    CuttingOff,
}

/// `--deny-exec`: which agents may not run `exec` here, by verified agent
/// uid, never by name. It only ever refuses; devices are never refused.
#[derive(Default, Debug, serde::Serialize)]
pub struct DenyExec {
    every_agent: bool,
    #[serde(rename = "agent_uids")]
    agents: Vec<String>,
}

impl DenyExec {
    /// Every agent is refused `exec`, not only some.
    pub fn every_agent(&self) -> bool {
        self.every_agent
    }

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
    pub fn failed(code: Code, message: String, audit_args: Value) -> Done {
        Done::failure(Failure::new(code, message), audit_args)
    }

    pub fn failure(failure: Failure, audit_args: Value) -> Done {
        Done {
            response: Response::error(&failure),
            audit_args,
            outcome: failure.code,
        }
    }

    /// What a worker reported, by its code.
    pub fn worker(error: crate::worker::WorkerError, audit_args: Value) -> Done {
        Done::failure(Failure::reported(&error.code, error.message), audit_args)
    }

    /// Sends a file to the client: `first` as its own answer, then the
    /// bytes, then the last answer with their size and sha256, which the
    /// audit record keeps too.
    pub async fn send_bytes(
        send: &mut crate::transport::SendStream,
        first: Value,
        bytes: &[u8],
        mut audit: serde_json::Map<String, Value>,
    ) -> Done {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;
        let result = json!({
            "size": bytes.len(),
            "sha256": crate::state::to_hex(&Sha256::digest(bytes)),
        });
        audit.insert("size".into(), result["size"].clone());
        audit.insert("sha256".into(), result["sha256"].clone());
        let first = serde_json::to_vec(&Response::Ok(first)).expect("serializes");
        if wire::write_frame(send, &first).await.is_err() || send.write_all(bytes).await.is_err() {
            let message = "the client left".into();
            return Done::failed(code::connection_lost, message, Value::Object(audit));
        }
        Done {
            response: Response::Ok(result),
            audit_args: Value::Object(audit),
            outcome: "ok".into(),
        }
    }
}

/// How long the host waits for a relay connection when it starts.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);

/// Waits for the endpoint's relay connection. A host `serve -d` started
/// waits as long as it takes, saying each `ONLINE_TIMEOUT` what is wrong;
/// any other fails after the first.
async fn come_online(
    endpoint: &Endpoint,
    state_dir: &Path,
    lifetime: &Lifetime,
    relays: &[String],
) -> Result<(), Failure> {
    loop {
        if tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
            .await
            .is_ok()
        {
            return Ok(());
        }
        let homes: Vec<(String, Option<String>)> = endpoint
            .home_relay_status()
            .get()
            .iter()
            .map(|status| {
                let error = status.last_error().map(|error| format!("{error:#}"));
                (status.url().to_string(), error)
            })
            .collect();
        let probes = if homes.is_empty() {
            ping_relays(relays).await
        } else {
            Vec::new()
        };
        let trouble = relay_trouble(&homes, &probes);
        if !lifetime.managed {
            let message = format!(
                "no iroh relay connection within {} seconds: {trouble}",
                ONLINE_TIMEOUT.as_secs()
            );
            return Err(Failure::new(code::relay_unreachable, message));
        }
        log(&format!("relay: {trouble}; still trying"));
        let why = format!("trying to reach a relay ({trouble})");
        let attached = lifetime.attached.load(Ordering::SeqCst);
        if let Err(failure) = lifecycle::record_waiting(state_dir, attached, &why) {
            log(&format!("could not record the wait: {}", failure.message));
        }
    }
}

/// What keeps the host off its relay: each home relay iroh chose, with the
/// last error it had connecting; or, before it chose one, each relay it
/// could choose from, as an HTTPS check of it went.
fn relay_trouble(
    homes: &[(String, Option<String>)],
    probes: &[(String, Result<(), String>)],
) -> String {
    if homes.is_empty() {
        let checked: Vec<String> = probes
            .iter()
            .map(|(url, probe)| match probe {
                Ok(()) => format!("{url} answers HTTPS"),
                Err(why) => format!("{url} failed: {why}"),
            })
            .collect();
        return format!("iroh reached none of its relays: {}", checked.join("; "));
    }
    homes
        .iter()
        .map(|(url, error)| match error {
            Some(error) => format!("{url} failed: {error}"),
            None => format!("{url} has not connected yet"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// An HTTPS check of each relay (its `/ping`), for when iroh has no error
/// to show: how each went, in `relays`' order.
async fn ping_relays(relays: &[String]) -> Vec<(String, Result<(), String>)> {
    let client = match crate::plow::http(reqwest::Client::builder().timeout(Duration::from_secs(5)))
    {
        Ok(client) => client,
        Err(failure) => {
            let why = failure.message;
            return relays
                .iter()
                .map(|url| (url.clone(), Err(why.clone())))
                .collect();
        }
    };
    let mut pings = tokio::task::JoinSet::new();
    for (index, url) in relays.iter().enumerate() {
        let ping = client.get(format!("{}/ping", url.trim_end_matches('/')));
        pings.spawn(async move {
            let result = match ping.send().await {
                Ok(answer) if answer.status().is_success() => Ok(()),
                Ok(answer) => Err(format!("its /ping answered {}", answer.status())),
                Err(err) => Err(error_chain(&err)),
            };
            (index, result)
        });
    }
    let mut results: Vec<_> = relays
        .iter()
        .map(|url| (url.clone(), Err("its check did not finish".to_string())))
        .collect();
    while let Some(Ok((index, result))) = pings.join_next().await {
        results[index].1 = result;
    }
    results
}

/// `err` and each error under it, in one line.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut reasons = vec![err.to_string()];
    let mut source = err.source();
    while let Some(err) = source {
        reasons.push(err.to_string());
        source = err.source();
    }
    reasons.join(": ")
}

/// Runs the host until it is asked to stop. With `headless_browsers`,
/// every browser session runs with no window. An `attached` host stops
/// when the `latch serve` that started it goes away (see
/// `serve/control.rs`).
pub async fn run(state: &StateDir, args: &crate::cli::ServeArgs) -> Result<(), Failure> {
    let (headless_browsers, attached) = (args.headless_browsers, args.attached);
    let lifetime = Lifetime::new(attached, args.managed);
    let deny_exec = DenyExec::from_flags(&args.deny_exec);
    // A host that could not sandbox its workers would refuse every call;
    // say so once, now, instead of starting. The quick check names a
    // missing kernel feature; the probe worker then installs the sandbox
    // for real, which is the only way to know it can be.
    crate::sandbox::check().map_err(|message| Failure::new(code::sandbox_unavailable, message))?;
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
    // Held until this process ends, however it ends.
    let _lock = lock_state(&state_dir)?;
    // Only the host holding the folder saves its flags, so they are the
    // running host's, or the last one's (`managed::saved_deny_exec`).
    crate::managed::save_given(&state.path, &crate::managed::given_now(args))?;
    // From here on, other processes see this host's steps (`lifecycle.rs`).
    lifecycle::record(&state.path, Phase::Starting, attached, None)?;
    let result = run_locked(state, headless_browsers, deny_exec, lifetime, state_dir).await;
    let (phase, error) = match &result {
        Ok(()) => (Phase::Stopped, None),
        Err(failure) => (Phase::Error, Some(failure)),
    };
    if let Err(failure) = lifecycle::record(&state.path, phase, attached, error) {
        log(&format!(
            "could not record the host's last step: {}",
            failure.message
        ));
    }
    result
}

/// The folder's key, once no command on this machine dials with it: a
/// direct follow sees this host start and ends (`remote::stream`); any
/// other call is waited for, and the wait is logged and recorded.
async fn own_key(state: &StateDir, attached: bool) -> Result<HeldKey, Failure> {
    let state = StateDir {
        path: state.path.clone(),
    };
    let path = state.path.clone();
    let waiting = move || {
        let why = "waiting for latch commands on this machine that call other hosts with this \
                   state folder's key to finish";
        log(&format!(
            "{why}: while this host serves, only it may use the key"
        ));
        if let Err(failure) = lifecycle::record_waiting(&path, attached, why) {
            log(&format!("could not record the wait: {}", failure.message));
        }
    };
    tokio::task::spawn_blocking(move || state.own_key(waiting))
        .await
        .expect("taking the key does not panic")
}

/// The workers a host on `state_dir` starts: this latch, confined.
pub(crate) fn workers(state_dir: PathBuf) -> Result<WorkerSetup, Failure> {
    let program = std::env::current_exe()
        .map_err(|err| Failure::new(code::sandbox_unavailable, err.to_string()))?;
    let home = std::env::home_dir()
        .and_then(|home| home.canonicalize().ok())
        .ok_or_else(|| {
            Failure::new(
                code::bad_home,
                "the host's home folder cannot be resolved".into(),
            )
        })?;
    Ok(WorkerSetup {
        home,
        areas: vec![],
        reads: vec![],
        hidden: hidden_paths(),
        state_dir,
        program,
    })
}

/// `run`, once this serve holds the state folder.
async fn run_locked(
    state: &StateDir,
    headless_browsers: bool,
    deny_exec: DenyExec,
    lifetime: Lifetime,
    state_dir: PathBuf,
) -> Result<(), Failure> {
    let workers = workers(state_dir)?;
    let rules = RuleFiles::new(&state.path)?;
    workers.probe().await.map_err(|err| {
        let message = format!("latch cannot sandbox host operations here: {}", err.message);
        Failure::new(code::sandbox_unavailable, message)
    })?;
    log(&format!("worker sandbox: {}", crate::sandbox::name()));
    exec::sweep_tmpdirs();
    // Holding the folder, no other host runs here to use an older latch's.
    match crate::plugins::prune(&state.path) {
        Ok(removed) => {
            for path in removed {
                log(&format!(
                    "removed {}, which this latch does not run",
                    path.display()
                ));
            }
        }
        Err(failure) => log(&format!(
            "could not remove old helper versions: {}",
            failure.message
        )),
    }
    let key = own_key(state, lifetime.attached.load(Ordering::SeqCst)).await?;
    run_host(
        state,
        key,
        workers,
        headless_browsers,
        rules,
        deny_exec,
        lifetime,
    )
    .await
}

async fn run_host(
    state: &StateDir,
    key: HeldKey,
    workers: WorkerSetup,
    headless_browsers: bool,
    rules: RuleFiles,
    deny_exec: DenyExec,
    lifetime: Lifetime,
) -> Result<(), Failure> {
    let config = state.require_login()?;
    let binary = Binary::at(&workers.program)?;
    let plow = Plow::new(&config.api_url, &credential::token(state)?)?;
    let gatekeeper = Gatekeeper::new(
        plow.clone(),
        Gatekeeper::provider_for_host(&state.path)
            .map_err(|why| Failure::new(code::not_logged_in, why))?,
        Box::new(rules),
    )
    .keeping_requests(&state.path);
    let mut audit = AuditLog::open(state.path.join(crate::audit::FILE))?;
    log(&format!("audit log verified: {} records", audit.len()));
    let home = std::env::home_dir().unwrap_or_default();
    let plugins = crate::plugins::Plugins::load(state, &home)?;
    log(&format!("commands in this log: {}", crate::redact::NOTE));
    if let Some(refused) = deny_exec.describe() {
        log(&refused);
    }

    // Tests run a relay of their own, so they need no internet.
    let relay_mode = match std::env::var("LATCH_TEST_RELAY_URL") {
        Err(_) => RelayMode::Default,
        Ok(url) => {
            let url = url.parse().map_err(|_| {
                let message = format!("LATCH_TEST_RELAY_URL {url:?} is not a relay URL");
                Failure::new(code::bad_relay, message)
            })?;
            RelayMode::custom([url])
        }
    };
    let relays: Vec<String> = relay_mode
        .relay_map()
        .urls::<Vec<_>>()
        .iter()
        .map(ToString::to_string)
        .collect();
    let endpoint = key
        .endpoint()
        .relay_mode(relay_mode)
        .alpns(vec![wire::ALPN.to_vec(), link::ALPN.to_vec()])
        .bind()
        .await
        .map_err(|err| Failure::new(code::iroh, format!("could not start iroh: {err}")))?;
    log(&format!(
        "serving as {:?}, key {}",
        config.peer.name,
        endpoint.id()
    ));

    // Clients dial us through our relay, so register only once we have one.
    let dashboard = Dashboard::new(&config.peer.name);
    // The board as it was: the calls the log holds from before this start.
    let own = endpoint.id().to_string();
    dashboard.restore(
        audit
            .take_recent()
            .iter()
            .filter_map(|record| dashboard::recorded(record, &own))
            .collect(),
    );
    dashboard.step("connecting to the relay…");
    if let Err(failure) = come_online(&endpoint, &state.path, &lifetime, &relays).await {
        endpoint.close().await;
        return Err(failure);
    }
    dashboard.step("registering with Plow…");
    let terminate = signal(SignalKind::terminate())
        .map_err(|err| Failure::new(code::signal, format!("cannot listen for SIGTERM: {err}")))?;
    let name = config.peer.name.clone();
    let heartbeat_state = StateDir {
        path: state.path.clone(),
    };

    let vault = crate::vault::host::Vault::new(workers.state_dir.clone());
    if headless_browsers {
        log("browsers: headless, with no window");
    }
    let browsers = crate::browser::host::Browsers::new(
        workers.state_dir.clone(),
        vault.clone(),
        headless_browsers,
    );
    browsers.show_on(dashboard.clone());
    let host = Arc::new(Host {
        plow,
        audit: Arc::new(Mutex::new(audit)),
        workers,
        dashboard,
        lifetime,
        browsers,
        vault,
        endpoint: endpoint.clone(),
        name,
        deny_exec,
        gatekeeper,
        binary,
        plugins,
        home,
        stopping: watch::Sender::new(Stopping::No),
        agent_names: Mutex::default(),
    });
    watch_binary(&host, BINARY_CHECK_EVERY);
    sweep_browsers(&host);
    crate::relay_link::watch(&endpoint);
    crate::update::watch(&state.path, &host.dashboard);
    let local_socket = crate::transport::LocalSocket::bind(&state.path)?;
    // Held while this host serves; it lets go as `run_host` returns.
    let _awake = crate::keep_awake::start(&state.path);
    lifecycle::record(
        &state.path,
        Phase::Serving,
        host.lifetime.attached.load(Ordering::SeqCst),
        None,
    )?;
    host.dashboard.socket(true);
    host.dashboard.connectors(host.plugins.enabled());
    host.dashboard.serving();
    control::await_holder(&host);
    let mut heartbeat = tokio::spawn(heartbeat::run(
        host.plow.clone(),
        endpoint.clone(),
        heartbeat_state,
        config,
        host.dashboard.clone(),
    ));
    // `Box::pin` gives the stop signal a fixed place in memory, so the loop
    // can wait on the same future again each time round.
    let mut stop = Box::pin(stop_requested(terminate));
    let mut connections = JoinSet::new();
    let mut accept_failure = None;
    let moved = loop {
        tokio::select! {
            accepted = local_socket.listener.accept() => {
                match accepted {
                    Ok((socket, _)) => { connections.spawn(serve_local(host.clone(), socket)); }
                    Err(err) => {
                        log(&format!("stopping: local socket accept failed: {err}"));
                        accept_failure = Some(Failure::new(code::local_socket, err.to_string()));
                        break None;
                    }
                }
            }
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    let connection = tracing::info_span!("connection", caller = Empty);
                    connections.spawn(serve_connection(host.clone(), incoming).instrument(connection));
                }
                None => break None,
            },
            // Connections that ended are let go of as they end.
            Some(_) = connections.join_next() => {}
            () = &mut stop => {
                log("stopping: signal received");
                break None;
            }
            () = host.lifetime.stop.notified() => break None,
            // The heartbeat only ends when Plow gave our name to another
            // key.
            ended = &mut heartbeat => {
                break Some(ended.unwrap_or_else(|err| {
                    Failure::new(code::heartbeat, format!("the heartbeat stopped: {err}"))
                }));
            }
        }
    };
    let step = |step: &str| {
        host.dashboard.step(&format!("stopping: {step}…"));
    };
    step("finishing the operations running");
    shut_down(&host, moved.is_none().then_some(heartbeat), connections).await;
    // Every browser closes, however the host stopped.
    step("closing browsers");
    host.browsers.close_all().await;
    // A name that moved is not ours any more: there is nothing of ours to
    // take offline.
    if moved.is_none() {
        step("telling Plow this host is offline");
        go_offline(&host.plow, &endpoint.id().to_string()).await;
    }
    step("closing connections");
    endpoint.close().await;
    log("stopped");
    moved.or(accept_failure).map_or(Ok(()), Err)
}

/// Longest a stopping host waits for the operations it is running, and
/// then for the execs it cut off to be killed and audited.
const DRAIN_WITHIN: Duration = Duration::from_secs(10);
const CUT_OFF_WITHIN: Duration = Duration::from_secs(10);

/// What a host does once it stops serving, however it stopped: no new
/// connections (the accept loop is over), no heartbeat that could list it
/// online again, and every connection's operation finished. Once
/// `DRAIN_WITHIN` has passed, running execs are killed with everything they
/// started and audited as `interrupted`; anything still left after
/// `CUT_OFF_WITHIN` is dropped.
async fn shut_down(
    host: &Host,
    heartbeat: Option<JoinHandle<Failure>>,
    mut connections: JoinSet<()>,
) {
    if let Some(heartbeat) = heartbeat {
        heartbeat.abort();
        let _ = heartbeat.await;
    }
    host.stopping.send_replace(Stopping::Draining);
    if within(DRAIN_WITHIN, &mut connections).await {
        return;
    }
    log(&format!(
        "stopping: cutting off {} connection(s) still running after {} seconds",
        connections.len(),
        DRAIN_WITHIN.as_secs()
    ));
    host.stopping.send_replace(Stopping::CuttingOff);
    if !within(CUT_OFF_WITHIN, &mut connections).await {
        log("stopping: a cut-off operation did not end; dropping it");
        connections.shutdown().await;
    }
}

/// Whether every connection task ended within `limit`.
async fn within(limit: Duration, connections: &mut JoinSet<()>) -> bool {
    let ended = async { while connections.join_next().await.is_some() {} };
    tokio::time::timeout(limit, ended).await.is_ok()
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
            Failure::new(code::no_binary, message)
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

/// How often a host looks whether its binary was replaced, for its
/// dashboard (an operation looks for itself).
const BINARY_CHECK_EVERY: Duration = Duration::from_secs(10);

/// Shows on `host`'s dashboard once its binary was replaced, looking each
/// `every`, and which build is there now. Stops with the host.
fn watch_binary(host: &Arc<Host>, every: Duration) {
    let host = Arc::downgrade(host);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let Some(host) = host.upgrade() else {
                return;
            };
            if !host.binary.replaced() {
                continue;
            }
            let version = tokio::process::Command::new(&host.binary.path)
                .arg("--version")
                .output()
                .await
                .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
                .unwrap_or_default();
            host.dashboard.updated(updated_to(&version));
            return;
        }
    });
}

/// Every minute, lets go of the browser sessions that ended or idled out,
/// and with them their profiles, for as long as the host runs.
fn sweep_browsers(host: &Arc<Host>) {
    let host = Arc::downgrade(host);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Some(host) = host.upgrade() else {
                return;
            };
            host.browsers.sweep().await;
        }
    });
}

/// `latch was updated; restart to use build 1327`, from what the new
/// latch's `--version` said (`{"version": "0.1.0 (1327): commit …"}`).
fn updated_to(said: &str) -> String {
    let said: Value = serde_json::from_str(said).unwrap_or_default();
    let number = said["version"]
        .as_str()
        .unwrap_or_default()
        .split_once(" (")
        .and_then(|(_, rest)| rest.split_once("):"))
        .map(|(number, _)| number)
        .filter(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
    match number {
        Some(number) => format!("latch was updated; restart to use build {number}"),
        None => "latch was updated; restart to use the new build".to_string(),
    }
}

/// An exec this host's owner refuses to the caller (`latch serve --deny-exec`).
fn exec_denied() -> Failure {
    Failure::new(
        code::exec_denied,
        "this host's owner does not let this agent run exec here \
         (latch serve --deny-exec); asking again will not help"
            .to_string(),
    )
}

/// What every operation gets once the host's binary was replaced.
fn host_upgraded() -> Failure {
    let message = "latch on this host was replaced since `latch serve` started; nothing ran. \
                   Restart `latch serve` on the host to run the new latch, then try again";
    Failure::new(code::host_upgraded, message.to_string())
}

/// Makes this the one `latch serve` on `state_dir`: two hosts on one state
/// would share one key and one audit log. The lock is an exclusive `flock` on
/// `serve.lock`, which the system releases when its holder exits, so one
/// left behind by a crash never blocks. The file says who holds it, for the
/// refusal a second serve prints. A lock held for less than a second is
/// another process asking whether a serve runs (`lifecycle::holds_lock`),
/// so this waits that long before refusing.
fn lock_state(state_dir: &Path) -> Result<File, Failure> {
    hold_state(state_dir, "latch serve", Duration::from_secs(1), |_| true)?.map_err(|holder| {
        if !holder.starts_with("latch serve") {
            let message = format!(
                "{} is held by {holder}; start latch once it finishes",
                state_dir.display()
            );
            return Failure::new(code::state_busy, message);
        }
        let message = format!(
            "{} is already served by {holder}; stop that `latch serve` first",
            state_dir.display()
        );
        Failure::new(code::already_serving, message)
    })
}

/// Takes `serve.lock` for `who` (see `lock_state`), trying for up to
/// `within`, but past the first second only while `wait_for` says its
/// holder will let go. Not taken: who holds it, as its file says.
pub(crate) fn hold_state(
    state_dir: &Path,
    who: &str,
    within: Duration,
    wait_for: impl Fn(&str) -> bool,
) -> Result<Result<File, String>, Failure> {
    let path = state_dir.join("serve.lock");
    let io =
        |err: std::io::Error| Failure::new(code::state_io, format!("{}: {err}", path.display()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(io)?;
    let started = std::time::Instant::now();
    while file.try_lock().is_err() {
        let mut holder = String::new();
        let _ = file
            .seek(std::io::SeekFrom::Start(0))
            .and_then(|_| file.read_to_string(&mut holder));
        let holder = match holder.trim() {
            "" => "another process".to_string(),
            holder => holder.to_string(),
        };
        let waited = started.elapsed();
        if waited >= within || (waited >= Duration::from_secs(1) && !wait_for(&holder)) {
            return Ok(Err(holder));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let exe =
        std::env::current_exe().map_or_else(|_| "latch".into(), |exe| exe.display().to_string());
    let holder = format!("{who} (pid {}, {exe})\n", std::process::id());
    file.set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|()| file.write_all(holder.as_bytes()))
        .map_err(io)?;
    Ok(Ok(file))
}

/// The places no worker may read or write (`sandbox::never_readable`). The
/// state folder is hidden as well, on its own terms (see `sandbox.rs`).
pub fn hidden_paths() -> Vec<PathBuf> {
    let paths = std::env::home_dir()
        .map(|home| crate::sandbox::never_readable(&home))
        .unwrap_or_default();
    with_targets(paths)
}

/// `paths`, each followed by where it really is when a link leads
/// elsewhere (`~/.docker/config.json` to `~/private/login.json`, say).
/// Resolved once, when the host starts, and kept: a worker that removes
/// the link must not uncover its target to the next.
fn with_targets(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for path in paths {
        let target = path.canonicalize().ok().filter(|real| *real != path);
        all.push(path);
        all.extend(target);
    }
    all
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
        serve_phone(&host, &caller, &connection).await;
    } else {
        serve_caller(&host, &caller, &connection).await;
    }
    // A stopping host gives the client a moment to close: dropping the
    // connection at once could cut off the answer it was just sent.
    if *host.stopping.borrow() != Stopping::No {
        let _ = tokio::time::timeout(LINGER, connection.closed()).await;
    }
}

/// How long a stopping host waits for a client to close its connection.
const LINGER: Duration = Duration::from_secs(1);

/// A latch client's connection: checked once on connect, then one
/// operation per stream.
async fn serve_caller(host: &Host, caller: &str, connection: &Connection) {
    // First check: on connect. A refused caller's connection is closed with
    // the "denied" code, which its client reports as exit 4.
    let checking = Instant::now();
    let checked = host.check(caller).await;
    let timings = Some(audit::Timings {
        caller_check_ms: checking.elapsed().as_millis() as u64,
        ..audit::Timings::default()
    });
    let refusal = match checked {
        Ok(identity) => {
            let who = format!("{} ({})", identity.name, identity.kind);
            let mut record = audit::record(
                caller,
                identity.agent_uid.clone(),
                "connect",
                "allow",
                "connected",
            );
            record.caller_name = host.known_name(&identity);
            record.timings = timings;
            match host.append(record).await {
                Ok(_) => {
                    log(&format!("connect: {who} {caller}: allowed"));
                    None
                }
                Err(message) => Some(message),
            }
        }
        Err(denial) => {
            // Already refusing, so a failed audit write changes nothing here.
            let mut record = audit::record(caller, None, "connect", "deny", &denial.code);
            record.timings = timings;
            let _ = host.append(record).await;
            host.show_refusal(caller, "connect", &denial);
            Some(denial.message)
        }
    };
    if let Some(message) = refusal {
        log(&format!("connect: {caller}: denied: {message}"));
        connection.close(VarInt::from_u32(wire::CLOSE_DENIED), message.as_bytes());
        return;
    }

    // One stream per operation, until the client closes the connection or
    // the host stops.
    while let Some((send, recv)) = next_stream(host, connection).await {
        let operation = tracing::info_span!("operation", op = Empty);
        serve_operation(
            host,
            caller,
            false,
            send.into(),
            recv.into(),
            cut_off(host, connection),
        )
        .instrument(operation)
        .await;
        log(&format!(
            "paths to {caller}: {}",
            describe_paths(connection)
        ));
    }
}

/// The connection's next operation, until the client closes it or the host
/// is stopping.
async fn next_stream(
    host: &Host,
    connection: &Connection,
) -> Option<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream)> {
    let mut stopping = host.stopping.subscribe();
    // Stopping wins over a stream that is also ready: no new operation
    // starts once the host is stopping.
    tokio::select! {
        biased;
        _ = stopping.wait_for(|stopping| *stopping != Stopping::No) => None,
        stream = connection.accept_bi() => stream.ok(),
    }
}

async fn serve_operation(
    host: &Host,
    caller: &str,
    local: bool,
    mut send: crate::transport::SendStream,
    mut recv: crate::transport::RecvStream,
    cut_off: impl std::future::Future<Output = Code>,
) {
    let Some(request) = read_request(&mut send, &mut recv).await else {
        return;
    };
    serve_request(host, caller, local, request, send, recv, cut_off).await;
}

/// The operation's request frame. A frame that is not one is answered
/// `bad_request`; then, as when the stream ends first, there is none.
async fn read_request(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Option<Request> {
    let json = wire::read_frame(recv).await.ok()?;
    match serde_json::from_slice::<Request>(&json) {
        Ok(request) => Some(request),
        Err(_) => {
            let response = failed(
                code::bad_request,
                "the request frame is not valid".to_string(),
            );
            answer(send, &response).await;
            None
        }
    }
}

async fn serve_request(
    host: &Host,
    caller: &str,
    local: bool,
    mut request: Request,
    mut send: crate::transport::SendStream,
    mut recv: crate::transport::RecvStream,
    cut_off: impl std::future::Future<Output = Code>,
) {
    let op = request.op.clone();
    let op = op.as_str();
    let exec_permissions = (op == "exec").then(|| {
        json!({
            "writes": request.args.get("writes").cloned().unwrap_or_else(|| json!([])),
            "network": request.args.get("network").cloned().unwrap_or(json!(false)),
        })
    });
    tracing::Span::current().record("op", display(op));
    let started = Instant::now();

    // Second check: before the operation, in case the caller was revoked
    // since it connected.
    let checking = Instant::now();
    let identity = if local {
        Ok(Identity {
            key: caller.to_string(),
            kind: "device".into(),
            name: host.name.clone(),
            agent_uid: None,
            permissions: crate::plow::Permissions { call: true },
        })
    } else {
        host.check(caller).await
    };
    let caller_check_ms = checking.elapsed().as_millis() as u64;
    let timings = (!local).then(|| audit::Timings {
        caller_check_ms,
        ..audit::Timings::default()
    });
    let response = match identity {
        Err(denial) => {
            let mut record = audit::record(caller, None, op, "deny", &denial.code);
            record.timings = timings;
            let _ = host.append(record).await;
            log(&format!("{op}: {caller}: denied: {}", denial.message));
            host.show_refusal(caller, op, &denial);
            failed(code::caller_denied, denial.message)
        }
        // An import's preview saves nothing: neither audited nor on the board.
        Ok(identity) if op == "vault" && request.args["command"] == "preview" => {
            let response = crate::vault::host::preview(
                &host.vault,
                &identity.kind,
                &request.args,
                &mut send,
                &mut recv,
            )
            .await;
            let outcome = match &response {
                Response::Ok(_) => "ok",
                Response::Error { code, .. } => code,
            };
            log(&format!("vault preview: {caller}: {outcome}"));
            response
        }
        // A preview runs nothing, so it is neither audited nor on the board.
        Ok(identity) if op == "rules_try" => {
            let response = rules_try::run(&host.checks(), &identity, local, &request.args).await;
            let outcome = match &response {
                Response::Ok(answer) => answer["verdict"].as_str().unwrap_or_default(),
                Response::Error { code, .. } => code,
            };
            log(&format!("{op}: {caller}: {outcome}"));
            response
        }
        Ok(identity) => {
            // On the board, running, from now until it is audited.
            let asked = request.args.clone();
            let begun = dashboard::begun(op, &asked);
            let begun_at = begun.at;
            let on_board = host.dashboard.begin(
                host.board_caller(&identity, local, None),
                dashboard::session(op, &asked),
                begun,
            );
            let chat = request.chat.clone();
            let checks = host.checks();
            let gate = checks.gate(&identity, local, chat.as_deref());
            let Checked {
                gate,
                prepared_exec,
                running,
                names,
                judgment,
                stop,
            } = check(&checks, &identity, gate, chat.as_deref(), &mut request).await;
            let mut phases = None;
            let done = if let Some(stop) = stop {
                stop.done()
            } else {
                match op {
                    "file_get" => files::get(&host.workers, &request.args, &mut send).await,
                    "file_put" => {
                        files::put(&host.workers, &request.args, &mut send, &mut recv).await
                    }
                    "exec" => {
                        exec::run(
                            &host.workers,
                            prepared_exec.expect("exec was validated"),
                            async {
                                tokio::select! {
                                    reason = cut_off => reason,
                                    _ = recv.read_u8(), if local => code::caller_gone,
                                }
                            },
                        )
                        .await
                    }
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
                    "messages" | "contacts" => {
                        crate::messages::host::run(
                            &host.workers,
                            &host.home,
                            op != "messages" || names.is_some(),
                            op,
                            &request.args,
                        )
                        .await
                    }
                    "browser" => match &gate {
                        Ok(gate) => {
                            let (done, took) = crate::browser::host::run(
                                &host.browsers,
                                caller,
                                &identity.kind,
                                Some(*gate),
                                request.args,
                                &mut send,
                                &mut recv,
                            )
                            .await;
                            phases = Some(took);
                            done
                        }
                        Err(failure) => Done {
                            response: Response::error(failure),
                            audit_args: Value::Null,
                            outcome: failure.code.clone(),
                        },
                    },
                    "plugins" => {
                        crate::plugins::host::run(
                            &host.plugins,
                            &host.workers,
                            &host.plow,
                            local,
                            &request.args,
                        )
                        .await
                    }
                    "status" | "audit" | "audit_shot" | "audit_judge" => {
                        reads::run(host, &identity.kind, op, &request.args, &mut send, cut_off)
                            .await
                    }
                    "google" => {
                        crate::google::host::run(
                            &host.workers,
                            &host.plow,
                            &host.name,
                            local,
                            request.args,
                            &mut send,
                            &mut recv,
                        )
                        .await
                    }
                    _ => Done::failed(
                        code::not_built_yet,
                        format!("this host does not run `{op}` yet"),
                        Value::Null,
                    ),
                }
            };
            // A browser call's record has its phases, the owner's included.
            let timings = match phases {
                Some(browser) => Some(audit::Timings {
                    caller_check_ms,
                    total_ms: Some(started.elapsed().as_millis() as u64),
                    ..browser
                }),
                None => timings,
            };
            let decision = decision(&done.response);
            log(&format!(
                "{op}: {caller}: {decision}: {} {}",
                done.outcome,
                crate::redact::shown(op, &done.audit_args)
            ));
            let judged = judgment.map(|judgment| judgment.gate);
            let mut record = audit::record(
                caller,
                identity.agent_uid.clone(),
                op,
                decision,
                &done.outcome,
            );
            record.caller_name = host.known_name(&identity);
            record.args = done.audit_args.clone();
            record.started = Some(begun_at.to_string());
            record.timings = timings;
            record.gate = judged
                .clone()
                .or_else(|| local.then(|| serde_json::json!({"caller":"owner_on_host"})));
            let appended = host.append(record).await;
            let finished = dashboard::finished(
                op,
                &asked,
                &done.audit_args,
                &done.response,
                judged.as_ref(),
                appended.as_deref().ok(),
                started.elapsed(),
            );
            let response = match appended {
                Ok(audit_id) => with_audit_id(done.response, audit_id),
                Err(message) => failed(code::audit_failed, message),
            };
            // Answered before the board's name for the caller, which may ask Plow.
            answer(&mut send, &with_permissions(response, exec_permissions)).await;
            let name = host.shown_name(&identity).await;
            host.dashboard.end(
                on_board,
                host.board_caller(&identity, local, Some(name)),
                finished,
            );
            if op == "plugins" {
                host.dashboard.connectors(host.plugins.enabled());
            }
            drop(running);
            drop(names);
            return;
        }
    };
    answer(&mut send, &with_permissions(response, exec_permissions)).await;
}

/// An exec's refusal, with the writes and network it asked for.
fn with_permissions(mut response: Response, exec_permissions: Option<Value>) -> Response {
    if let (Some(permissions), Response::Error { details, .. }) = (exec_permissions, &mut response)
    {
        if details.is_null() {
            *details = json!({});
        }
        details["writes"] = permissions["writes"].clone();
        details["network"] = permissions["network"].clone();
    }
    response
}

/// Everything a request meets before it runs, in order: its file path
/// resolved, the caller's gate, the host's own refusals (`--deny-exec`, a
/// connector this host can't run or that is off, a replaced binary), an exec's paths checked, a
/// messages read planned, then the gatekeeper's judgment. Nothing the
/// request asks for runs here; `rules_try` stops after it.
struct Checked<'a> {
    gate: Result<Gate<'a>, Failure>,
    prepared_exec: Option<exec::PreparedExec>,
    /// The connectors it uses, counted as running until it is audited.
    running: Option<crate::plugins::Running<'a>>,
    names: Option<crate::plugins::Running<'a>>,
    judgment: Option<gate::Judgment>,
    /// Why it does not run, if it does not: the first refusal it met.
    stop: Option<Stop>,
}

/// Why a checked request does not run, as its caller is answered.
enum Stop {
    /// The host's own refusal, made without asking the judge: an exec its
    /// owner turned off (`--deny-exec`), a connector this host can't run or
    /// that is off, a path it never lets a worker use.
    Host(Done),
    /// Any other: a replaced binary, a bad request, the gatekeeper's no.
    Refused(Done),
}

impl Stop {
    /// A path the host turns down: its own refusal when it is one of the
    /// paths it never allows (exit 4), else a bad request.
    fn path(done: Done) -> Stop {
        match done.response {
            Response::Error {
                exit: crate::failure::Exit::Denied,
                ..
            } => Stop::Host(done),
            _ => Stop::Refused(done),
        }
    }

    fn done(self) -> Done {
        match self {
            Stop::Host(done) | Stop::Refused(done) => done,
        }
    }
}

async fn check<'a>(
    host: &Checks<'a>,
    identity: &Identity,
    gate: Result<Gate<'a>, Failure>,
    chat: Option<&str>,
    request: &mut Request,
) -> Checked<'a> {
    // A file's host path is resolved before anything is judged.
    let unplaced = files::resolve(&request.op, &mut request.args, host.workers);
    let op = request.op.clone();
    let op = op.as_str();
    // An exec this host's owner denies is refused without asking.
    let denied_exec = op == "exec" && host.deny_exec.refuses(identity);
    // A worker started now would run the new latch under this old
    // host: nothing runs, and nothing is judged.
    let upgraded = host.binary.replaced();
    let (prepared_exec, exec_failure) = if op == "exec" && !denied_exec && !upgraded {
        match exec::normalize(host.workers, &request.args) {
            Ok(args) => {
                request.args = serde_json::to_value(&args).expect("prepared exec serializes");
                (Some(args), None)
            }
            Err(failure) => (None, Some(failure)),
        }
    } else {
        (None, None)
    };
    // Nor for a connector this host can't run, or one its owner has not
    // enabled. One that is counts as running until its answer is
    // audited, so `disable` waits for it.
    let (running, not_enabled) = match crate::messages::host::supported_here(op) {
        Err(failure) => (None, Some(failure)),
        Ok(()) => match crate::plugins::Connector::of_op(op)
            .zip(host.plugins)
            .map(|(connector, plugins)| plugins.start(connector))
        {
            Some(Ok(running)) => (Some(running), None),
            Some(Err(failure)) => (None, Some(failure)),
            None => (None, None),
        },
    };
    // A messages call reads the address book for names only while
    // Contacts is enabled too, held the same way.
    let names = (op == "messages")
        .then(|| crate::plugins::Connector::of_op("contacts"))
        .flatten()
        .zip(host.plugins)
        .and_then(|(contacts, plugins)| plugins.start(contacts).ok());
    // A messages or contacts call is judged, and then run, as one
    // effective request: the reader's reading of the caller's words
    // and any cursor's filters. One the reader refuses is not judged.
    let mut unplanned = None;
    if (op == "messages" || op == "contacts") && !upgraded && not_enabled.is_none() && gate.is_ok()
    {
        match crate::messages::host::plan(host.workers, op, &request.args).await {
            Ok(planned) => request.args = planned,
            Err(done) => unplanned = Some(*done),
        }
    }
    // The vault is the owner's own setting: only an agent's list is judged.
    let gated = gate::gated(op, &request.args).filter(|_| {
        (op != "vault" || identity.kind == "agent")
            && !denied_exec
            && !upgraded
            && not_enabled.is_none()
            && exec_failure.is_none()
            && unplanned.is_none()
            && unplaced.is_none()
    });
    let unjudgeable = match (&gate, &gated) {
        (Ok(_), Some(gated)) => gated.unjudgeable.as_ref(),
        _ => None,
    };
    let judgment = match (&gate, &gated) {
        (Ok(gate), Some(gated)) if unjudgeable.is_none() => Some(gate.judge(&gated.action).await),
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
    let refused_now = |failure: Failure, audit_args: Value| Done {
        response: Response::error(&failure),
        audit_args,
        outcome: failure.code,
    };
    // A chat named where none can be is refused before anything else.
    let stop = if chat.is_some()
        && let Err(failure) = &gate
    {
        Some(Stop::Refused(Done {
            response: Response::error(failure),
            audit_args: Value::Null,
            outcome: failure.code.clone(),
        }))
    } else if upgraded {
        Some(Stop::Refused(refused_now(host_upgraded(), Value::Null)))
    } else if let Some(failure) = exec_failure {
        Some(Stop::path(refused_now(failure, Value::Null)))
    } else if let Some(failure) = not_enabled {
        // A Google call keeps its command, grammar words only, as a judged
        // one does. A Messages read's details hold the reader's words: none.
        let audit = match op {
            "google" => gate::gated(op, &request.args).map_or(Value::Null, |gated| gated.audit),
            _ => Value::Null,
        };
        Some(Stop::Host(refused_now(failure, audit)))
    } else if let Some(unplanned) = unplanned {
        Some(Stop::Refused(unplanned))
    } else if let Some(refused) = refused {
        Some(Stop::Refused(refused))
    } else if let Some(unplaced) = unplaced {
        Some(Stop::path(unplaced))
    } else if denied_exec {
        // As a refused judgment keeps it: no stdin.
        let audit = gate::gated(op, &request.args).map_or(Value::Null, |gated| gated.audit);
        Some(Stop::Host(refused_now(exec_denied(), audit)))
    } else {
        None
    };
    Checked {
        gate,
        prepared_exec,
        running,
        names,
        judgment,
        stop,
    }
}

/// Local callers are trusted by the private socket, never by a request field.
async fn serve_local(host: Arc<Host>, socket: tokio::net::UnixStream) {
    let (mut recv, mut send) = socket.into_split();
    let Some(request) = read_request(&mut send, &mut recv).await else {
        return;
    };
    if control::OPS.contains(&request.op.as_str()) {
        return control::serve(&host, &request, send, recv).await;
    }
    if request.op == crate::proxy::OP {
        return proxy::serve(&host, &request, send, recv).await;
    }
    let caller = host.endpoint.id().to_string();
    log(&format!(
        "connect: {caller}: owner_on_host over Unix socket"
    ));
    let operation = tracing::info_span!("operation", op = Empty);
    serve_request(
        &host,
        &caller,
        true,
        request,
        crate::transport::SendStream::Local(Some(send)),
        crate::transport::RecvStream::Local(recv),
        async {
            host.cut_off().await;
            code::interrupted
        },
    )
    .instrument(operation)
    .await;
}

/// Finishes when an exec is cut off by the host or loses its caller.
async fn cut_off(host: &Host, connection: &Connection) -> Code {
    tokio::select! {
        () = host.cut_off() => code::interrupted,
        _ = connection.closed() => code::caller_gone,
    }
}

/// A phone page's connection (see `vault/link.rs`). Its key is a
/// throwaway one Plow has never heard of: the link's code in each request
/// is what lets it in, so Plow is not asked. It can fetch its link's form
/// and save its link's item, nothing else.
async fn serve_phone(host: &Host, caller: &str, connection: &Connection) {
    log(&format!(
        "phone {caller}: connected over {}",
        describe_paths(connection)
    ));
    while let Some((mut send, mut recv)) = next_stream(host, connection).await {
        let Ok(json) = wire::read_frame(&mut recv).await else {
            return;
        };
        let Ok(request) = serde_json::from_slice::<Request>(&json) else {
            let response = failed(
                code::bad_request,
                "the request frame is not valid".to_string(),
            );
            answer(&mut send, &response).await;
            continue;
        };
        let started = Instant::now();
        let mut audit_args = serde_json::Map::new();
        audit_args.insert("command".into(), Value::String(request.op.clone()));
        // An agent's link names its request, not a code: the host serves it
        // only to a phone Plow vouches is the owner, for that request.
        let granted = match request.args["link"].is_string() && !host.binary.replaced() {
            true => host.plow.phone_grant(caller).await,
            false => Ok(None),
        };
        let result = match (request.op.as_str(), granted) {
            // The link's item would be saved by this old host's code.
            _ if host.binary.replaced() => Err(host_upgraded()),
            (_, Err(failure)) => Err(failure),
            ("page", Ok(granted)) => link::page(
                &host.vault,
                &host.here(),
                &request.args,
                granted.as_deref(),
                &mut audit_args,
            ),
            ("submit", Ok(granted)) => link::submit(
                &host.vault,
                &request.args,
                granted.as_deref(),
                &mut audit_args,
            ),
            (op, Ok(_)) => Err(Failure::new(
                code::not_on_a_phone_link,
                format!("a phone link cannot run `{op}`"),
            )),
        };
        let done = crate::vault::host::done(result, audit_args);
        let decision = decision(&done.response);
        log(&format!(
            "vault_link: phone {caller}: {decision}: {} {}",
            done.outcome, done.audit_args
        ));
        let mut record = audit::record(caller, None, "vault_link", decision, &done.outcome);
        record.args = done.audit_args.clone();
        let appended = host.append(record).await;
        let finished = dashboard::finished(
            "vault_link",
            &Value::Null,
            &done.audit_args,
            &done.response,
            None,
            appended.as_deref().ok(),
            started.elapsed(),
        );
        let phone = dashboard::Caller {
            label: "phone link".to_string(),
            detail: format!("the phone page of a vault link, key {caller}"),
        };
        host.dashboard.once(phone, finished);
        let response = match appended {
            Ok(audit_id) => with_audit_id(done.response, audit_id),
            Err(message) => failed(code::audit_failed, message),
        };
        answer(&mut send, &response).await;
    }
}

/// The audit decision for an operation the caller check let through: a
/// refusal (such as the sandbox's) is a "deny", anything else an "allow" —
/// a failure that is not a refusal (a missing file) was allowed to run.
/// A worker that could not apply its sandbox is a "deny" too: the host
/// refused to run the operation unconfined, so it never ran. So is an
/// operation the gatekeeper could not judge, and one this host can't run
/// at all: one it doesn't know, or one its platform lacks.
pub(crate) fn decision(response: &Response) -> &'static str {
    match response {
        Response::Error {
            exit: crate::failure::Exit::Denied | crate::failure::Exit::Usage,
            ..
        } => "deny",
        Response::Error { code, .. }
            if code == "sandbox_failed"
                || code == "gatekeeper_unavailable"
                || code == "host_upgraded"
                || code == "not_built_yet"
                || code == "unsupported_platform" =>
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

fn failed(code: Code, message: String) -> Response {
    Response::error(&Failure::new(code, message))
}

/// Sends the one response frame and ends our side of the stream.
async fn answer(send: &mut (impl tokio::io::AsyncWrite + Unpin), response: &Response) {
    let json = serde_json::to_vec(response).expect("a response serializes");
    if wire::write_frame(send, &json).await.is_ok() {
        let _ = tokio::io::AsyncWriteExt::shutdown(send).await;
    }
}

/// Why a caller was refused: a short code for the audit log, and a message
/// for the caller and the host's log.
struct Denial {
    code: String,
    message: String,
}

impl Host {
    /// Finishes when the host cuts off the operations still running.
    async fn cut_off(&self) {
        let mut stopping = self.stopping.subscribe();
        let _ = stopping
            .wait_for(|stopping| *stopping == Stopping::CuttingOff)
            .await;
    }

    fn checks(&self) -> Checks<'_> {
        Checks {
            name: &self.name,
            plow: &self.plow,
            workers: &self.workers,
            gatekeeper: &self.gatekeeper,
            deny_exec: &self.deny_exec,
            binary: &self.binary,
            plugins: Some(&self.plugins),
        }
    }

    /// Where a phone link points: our key, name and current relay.
    fn here(&self) -> link::Here {
        link::Here {
            key: self.endpoint.id().to_string(),
            name: self.name.clone(),
            relay: self.endpoint.addr().relay_urls().next().cloned(),
            plow: self.plow.api_url().to_string(),
        }
    }

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

    /// The caller's name for people to read: an agent's current name in
    /// Plow, which follows a rename, else its peer name, which keeps the
    /// name it registered with.
    async fn shown_name(&self, identity: &Identity) -> String {
        if let Some(uid) = &identity.agent_uid
            && let Ok(agent) = self.plow.agent(uid).await
        {
            let mut names = self.agent_names.lock().expect("names lock");
            names.insert(uid.clone(), agent.name.clone());
            return agent.name;
        }
        identity.name.clone()
    }

    /// The caller's name without asking Plow: an agent's as last looked
    /// up (none before the first lookup), else its machine's.
    fn known_name(&self, identity: &Identity) -> Option<String> {
        match &identity.agent_uid {
            Some(uid) => self
                .agent_names
                .lock()
                .expect("names lock")
                .get(uid)
                .cloned(),
            None => Some(identity.name.clone()),
        }
    }

    /// A caller refused at the door, on the board by its key.
    fn show_refusal(&self, caller: &str, op: &str, denial: &Denial) {
        let shown = dashboard::Caller {
            label: format!("{}…", &caller[..8.min(caller.len())]),
            detail: format!("key {caller}, not let in"),
        };
        self.dashboard
            .once(shown, dashboard::refused(op, &denial.code, &denial.message));
    }

    /// Who called, as the board shows them: the owner here or at one of
    /// their machines, or the agent by its `name` in Plow.
    fn board_caller(
        &self,
        identity: &Identity,
        local: bool,
        name: Option<String>,
    ) -> dashboard::Caller {
        if local {
            return dashboard::Caller {
                label: "you (local)".to_string(),
                detail: format!("{}, on this host's own socket", identity.name),
            };
        }
        if identity.kind == "device" {
            return dashboard::Caller {
                label: "you (device)".to_string(),
                detail: format!("{}, key {}", identity.name, identity.key),
            };
        }
        let uid = identity.agent_uid.as_deref().unwrap_or("unknown");
        dashboard::Caller {
            label: name.unwrap_or_else(|| identity.name.clone()),
            detail: format!("agent {uid}, peer {}, key {}", identity.name, identity.key),
        }
    }

    /// Appends to the audit log and returns the record's hash, once the
    /// record is on disk. On failure, returns what the caller is told. The
    /// lock, write and sync run on a blocking thread, so a slow disk holds
    /// up only the callers waiting on the log.
    async fn append(&self, record: Record) -> Result<String, String> {
        let audit = self.audit.clone();
        let appending = tokio::task::spawn_blocking(move || {
            audit.lock().expect("audit log lock").append(record)
        });
        let result = appending.await.unwrap_or_else(|stopped| {
            Err(Failure::new(
                code::audit_failed,
                format!("the append stopped: {stopped}"),
            ))
        });
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

mod control;
#[cfg(test)]
mod gated_tests;
#[cfg(test)]
mod plugins_tests;
mod proxy;
mod reads;
#[cfg(test)]
mod reads_tests;
mod rules_try;
pub(crate) use rules_try::{not_previewable, preview_here};
#[cfg(test)]
mod rules_try_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_names_its_build_when_it_has_one() {
        let release =
            r#"{"version":"0.1.0 (1327): commit 1a2b3c4d5, built 2026-10-22, standalone"}"#;
        assert_eq!(
            updated_to(release),
            "latch was updated; restart to use build 1327"
        );
        let local =
            r#"{"version":"0.1.0 (commit 1a2b3c4d5, built 2026-10-22 03:45 UTC, local build)"}"#;
        for said in [local, ""] {
            assert_eq!(
                updated_to(said),
                "latch was updated; restart to use the new build"
            );
        }
    }

    /// Before iroh picks a relay it has no error to show: each relay is
    /// checked over HTTPS, in order, and a relay that answers is told apart
    /// from one that does not.
    #[tokio::test]
    async fn a_host_off_its_relays_says_how_each_answered() {
        use iroh_relay::server::{RelayConfig, Server, ServerConfig};
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new(([127, 0, 0, 1], 0)));
        let server = Server::spawn(config).await.unwrap();
        let up = format!("http://{}/", server.http_addr().unwrap());
        let dead = "http://127.0.0.1:9/".to_string();

        let probes = ping_relays(&[dead.clone(), up.clone()]).await;
        let said = relay_trouble(&[], &probes);
        assert!(
            said.starts_with(&format!("iroh reached none of its relays: {dead} failed: ")),
            "{said}"
        );
        assert!(said.ends_with(&format!("; {up} answers HTTPS")), "{said}");

        // Once iroh has picked one, its own last error is the reason.
        let homes = [(up.clone(), Some("tls: access denied".to_string()))];
        assert_eq!(
            relay_trouble(&homes, &[]),
            format!("{up} failed: tls: access denied")
        );
    }

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
            next: None,
        };
        assert_eq!(decision(&refused), "deny");
        assert_eq!(decision(&Response::Ok(Value::Null)), "allow");
        assert_eq!(
            decision(&failed(code::not_found, "gone".to_string())),
            "allow"
        );
        let unconfined = failed(
            code::sandbox_failed,
            "the sandbox could not be applied".to_string(),
        );
        assert_eq!(decision(&unconfined), "deny");
        let upgraded = failed(code::host_upgraded, "restart the host".to_string());
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
    fn linked_state_keeps_its_target_hidden_once_the_link_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("private")).unwrap();
        std::os::unix::fs::symlink(home.join("private"), home.join(".latch")).unwrap();
        let hidden = with_targets(crate::sandbox::never_readable(&home));
        std::fs::remove_file(home.join(".latch")).unwrap();
        assert!(hidden.contains(&home.join(".latch")));
        assert!(hidden.contains(&home.join("private")));
    }

    #[test]
    fn successful_results_carry_their_audit_id() {
        let ok = Response::Ok(serde_json::json!({"path": "/tmp/a"}));
        let Response::Ok(result) = with_audit_id(ok, "abc".to_string()) else {
            panic!("still ok");
        };
        assert_eq!(result["audit_id"], "abc");
        assert_eq!(result["path"], "/tmp/a");

        let failure = with_audit_id(failed(code::not_found, "y".to_string()), "abc".to_string());
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
