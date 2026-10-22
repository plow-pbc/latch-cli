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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, RelayMode};
use serde_json::Value;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tracing::Instrument;
use tracing::field::{Empty, display};

use crate::audit::{self, AuditLog, Record};
use crate::credential;
use crate::dashboard::{self, Dashboard, Row};
use crate::exec;
use crate::failure::Failure;
use crate::files;
use crate::gate::rules::RuleFiles;
use crate::gate::{self, Caller, Gate, Gatekeeper};
use crate::heartbeat;
use crate::lifecycle::{self, Phase};
use crate::log::{self, log};
use crate::plow::{Identity, Plow};
use crate::state::{self, Config, StateDir};
use crate::state_folders::StateFolders;
use crate::vault::link;
use crate::wire::{self, Request, Response};
use crate::worker::WorkerSetup;

/// What every connection handler shares. `Arc` lets many tasks hold the
/// same `Host`; the `Mutex` lets one task at a time append to the log.
struct Host {
    plow: Plow,
    /// Also held by the blocking thread that appends (see `Host::append`).
    audit: Arc<Mutex<AuditLog>>,
    /// The live view, when `serve` shows one (see `dashboard.rs`).
    dashboard: Option<Dashboard>,
    workers: WorkerSetup,
    /// Open browser sessions, one per profile.
    browsers: crate::browser::host::Browsers,
    /// The host's vault, shared by every engine and profile.
    vault: std::sync::Arc<crate::vault::host::Vault>,
    /// Our own endpoint and name, which a phone link points at.
    endpoint: Endpoint,
    name: String,
    deny_exec: DenyExec,
    /// Judges agents' operations against the owner's chat and rules.
    gatekeeper: Gatekeeper,
    binary: Binary,
    /// How far the host is into stopping.
    stopping: watch::Sender<Stopping>,
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
/// With `headless_browsers`, every browser session runs with no window.
pub async fn run(
    state: &StateDir,
    folders: &[PathBuf],
    allow_read: &[PathBuf],
    show_dashboard: bool,
    headless_browsers: bool,
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
    // Held until this process ends, however it ends.
    let _lock = lock_state(&state_dir)?;
    // From here on, other processes see this host's steps (`lifecycle.rs`).
    lifecycle::record(&state.path, Phase::Starting, None)?;
    let result = run_locked(
        state,
        folders,
        allow_read,
        show_dashboard,
        headless_browsers,
        deny_exec,
        state_dir,
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
    allow_read: &[PathBuf],
    show_dashboard: bool,
    headless_browsers: bool,
    deny_exec: DenyExec,
    state_dir: PathBuf,
) -> Result<(), Failure> {
    let program = std::env::current_exe()
        .map_err(|err| Failure::failed("sandbox_unavailable", err.to_string()))?;
    // Named folders (with their `--allow-read`) win; with none, what
    // `latch shares set` saved.
    let saved;
    let (folders, allow_read) = if folders.is_empty() {
        saved = state.load_shares()?;
        (&saved.folders[..], &saved.allow_read[..])
    } else {
        (folders, allow_read)
    };
    let hidden = hidden_paths();
    let areas = shared_folders(folders, std::env::home_dir().as_deref())?;
    let reads = allowed_reads(
        allow_read,
        std::env::home_dir().as_deref(),
        &hidden,
        &state_dir,
    )?;
    let roots: Vec<PathBuf> = areas.iter().chain(&reads).cloned().collect();
    let workers = WorkerSetup {
        state_folders: StateFolders::new(&roots, &state_dir),
        areas,
        reads,
        hidden,
        state_dir,
        program,
    };
    let rules = RuleFiles::new(&state.path, &workers.areas)?;
    workers.probe().await.map_err(|err| {
        let message = format!("latch cannot sandbox host operations here: {}", err.message);
        Failure::failed("sandbox_unavailable", message)
    })?;
    log(&format!("worker sandbox: {}", crate::sandbox::name()));
    exec::sweep_tmpdirs();
    let config = state.require_login()?;
    let dashboard = if show_dashboard {
        let path = state.path.join("serve.log");
        log::to_file(&path)
            .map_err(|err| Failure::failed("log_file", format!("{}: {err}", path.display())))?;
        let note = workers.areas.is_empty().then_some(SHARING_NONE);
        Some(Dashboard::start(&config.peer.name, note))
    } else {
        None
    };
    let result = run_host(
        state,
        config,
        workers,
        dashboard.clone(),
        headless_browsers,
        rules,
        deny_exec,
    )
    .await;
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
    let audit = AuditLog::open(state.path.join(crate::audit::FILE))?;
    log(&format!("audit log verified: {} records", audit.len()));
    log(&format!("commands in this log: {}", crate::redact::NOTE));
    let names: Vec<String> = workers
        .areas
        .iter()
        .map(|area| area.display().to_string())
        .collect();
    if names.is_empty() {
        log(SHARING_NONE);
    } else {
        log(&format!("sharing: {}", names.join(", ")));
    }
    if !workers.reads.is_empty() {
        let reads: Vec<String> = workers
            .reads
            .iter()
            .map(|read| read.display().to_string())
            .collect();
        log(&format!(
            "also readable, never written: {}",
            reads.join(", ")
        ));
    }
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
    if let Some(dashboard) = &dashboard {
        dashboard.step("connecting to the relay…");
    }
    if tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
        .await
        .is_err()
    {
        endpoint.close().await;
        let message = "no iroh relay connection within 15 seconds".to_string();
        return Err(Failure::failed("relay_unreachable", message));
    }
    if let Some(dashboard) = &dashboard {
        dashboard.step("registering with Plow…");
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
    if headless_browsers {
        log("browsers: headless, with no window");
    }
    let browsers = crate::browser::host::Browsers::new(
        workers.state_dir.clone(),
        vault.clone(),
        headless_browsers,
    );
    if let Some(dashboard) = &dashboard {
        browsers.show_on(dashboard.clone());
    }
    let host = Arc::new(Host {
        plow,
        audit: Arc::new(Mutex::new(audit)),
        workers,
        dashboard,
        browsers,
        vault,
        endpoint: endpoint.clone(),
        name,
        deny_exec,
        gatekeeper,
        binary,
        stopping: watch::Sender::new(Stopping::No),
    });
    lifecycle::record(&state.path, Phase::Serving, None)?;
    // `Box::pin` gives the stop signal a fixed place in memory, so the loop
    // can wait on the same future again each time round.
    let mut stop = Box::pin(stop_requested(terminate));
    let mut connections = JoinSet::new();
    let moved = loop {
        tokio::select! {
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
            // The heartbeat only ends when Plow gave our name to another
            // key.
            ended = &mut heartbeat => {
                break Some(ended.unwrap_or_else(|err| {
                    Failure::failed("heartbeat", format!("the heartbeat stopped: {err}"))
                }));
            }
        }
    };
    let step = |step: &str| {
        if let Some(dashboard) = &host.dashboard {
            dashboard.step(&format!("stopping: {step}…"));
        }
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
    moved.map_or(Ok(()), Err)
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
    let try_lock = || file.try_lock().is_ok();
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

/// What a host sharing no folders says in its log and on its dashboard.
pub const SHARING_NONE: &str = "Sharing no folders — browser and Google only. \
    Add some with `latch shares set ~/code`, then restart `latch serve`.";

/// The folders `latch serve` was given, which its workers may use: each
/// made absolute (a worker runs in another directory, the first folder) and
/// checked now, so a typo is a plain error rather than refused files later.
/// Any folder may be shared but `home` (the home folder) and the folders
/// that hold its `Library`, where macOS keeps mail, messages and the like:
/// share the projects in it instead. The sandbox hides the state folder and
/// the never-readable places even inside a shared folder. None at all is a
/// host for the browser and Google only: its `exec` and `file` refuse.
pub fn shared_folders(folders: &[PathBuf], home: Option<&Path>) -> Result<Vec<PathBuf>, Failure> {
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
        if holds_library(&area, home) {
            return Err(refuse(HOLDS_LIBRARY));
        }
        areas.push(area);
    }
    Ok(areas)
}

/// Whether the real path `path` is the home folder's `Library`, where macOS
/// keeps mail, messages and the like, or a folder that holds it (the home
/// folder, `/Users`, `/`). Such a folder is neither shared nor opted in to.
fn holds_library(path: &Path, home: Option<&Path>) -> bool {
    home.is_some_and(|home| {
        let home = home.canonicalize().unwrap_or(home.to_path_buf());
        home.join("Library").starts_with(path)
    })
}

const HOLDS_LIBRARY: &str = "it holds the home folder's Library (mail, messages, keychains); name the folders in it you need";

/// The places no worker may read or write, even inside a shared folder:
/// the credentials in the home folder (`sandbox::never_readable`). The
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

/// The paths `--allow-read` names, which workers may read but not write:
/// each made absolute and checked now, as shared folders are. One inside a
/// hidden path or the state folder is refused, since no worker could read
/// it anyway; one that holds them is allowed, and they stay hidden in it,
/// but not one that holds the home folder's `Library`, as with shares.
pub fn allowed_reads(
    paths: &[PathBuf],
    home: Option<&Path>,
    hidden: &[PathBuf],
    state_dir: &Path,
) -> Result<Vec<PathBuf>, Failure> {
    let mut reads = Vec::new();
    for path in paths {
        let refuse = |code: &str, why: String| {
            let message = format!("cannot allow reads of {}: {why}", path.display());
            Failure::usage(code, message)
        };
        let real = path
            .canonicalize()
            .map_err(|_| refuse("bad_folder", "it does not exist".to_string()))?;
        if holds_library(&real, home) {
            return Err(refuse("bad_folder", HOLDS_LIBRARY.to_string()));
        }
        let in_state = real.ancestors().find(|dir| state::is_state_folder(dir));
        let hidden = hidden.iter().map(PathBuf::as_path).chain([state_dir]);
        let never = hidden
            .map(|never| never.canonicalize().unwrap_or(never.to_path_buf()))
            .find(|never| real.starts_with(never))
            .or(in_state.map(Path::to_path_buf));
        if let Some(never) = never {
            let why = format!(
                "{} holds credentials or latch's own state, which no caller may ever read",
                never.display()
            );
            return Err(refuse("never_readable", why));
        }
        reads.push(real);
    }
    Ok(reads)
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
    let refusal = match host.check(caller).await {
        Ok(identity) => {
            let who = format!("{} ({})", identity.name, identity.kind);
            let record = audit::record(caller, identity.agent_uid, "connect", "allow", "connected");
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
            let record = audit::record(caller, None, "connect", "deny", &denial.code);
            let _ = host.append(record).await;
            host.show_refusal(caller, "connect", &denial.code);
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
        serve_operation(host, caller, connection, send, recv)
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
async fn next_stream(host: &Host, connection: &Connection) -> Option<(SendStream, RecvStream)> {
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
    connection: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) {
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
            let _ = host
                .append(audit::record(caller, None, op, "deny", &denial.code))
                .await;
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
            // Any caller's exec, file or Google operation runs only if the
            // gatekeeper allows it.
            let gate = host.gate(&identity, request.chat.as_deref());
            // An exec this host's owner denies is refused without asking.
            let denied_exec = op == "exec" && host.deny_exec.refuses(&identity);
            // A worker started now would run the new latch under this old
            // host: nothing runs, and nothing is judged.
            let upgraded = host.binary.replaced();
            // With no shared folder, exec and file refuse: nothing to judge.
            let unshared =
                host.workers.areas.is_empty() && matches!(op, "exec" | "file_get" | "file_put");
            let first_area = host.workers.areas.first().map(PathBuf::as_path);
            let gated = gate::gated(op, &request.args, first_area)
                .filter(|_| !denied_exec && !upgraded && !unshared);
            let unjudgeable = match (&gate, &gated) {
                (Ok(_), Some(gated)) => gated.unjudgeable.as_ref(),
                _ => None,
            };
            // A one-time action (send one email) is judged, run and
            // recorded before the same caller's next one is judged, so two
            // sent at once do not both run.
            let turn = match (&gate, &gated) {
                (Ok(gate), Some(gated)) if gated.once => Some(gate.one_at_a_time().await),
                _ => None,
            };
            let judgment = match (&gate, &gated) {
                (Ok(gate), Some(gated)) if unjudgeable.is_none() => {
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
                        audit_args: gate::gated(op, &request.args, first_area)
                            .map_or(Value::Null, |gated| gated.audit),
                        outcome: "exec_denied".to_string(),
                    },
                    "exec" => {
                        exec::run(&host.workers, &request.args, cut_off(host, connection)).await
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
                    "browser" => match &gate {
                        Ok(gate) => {
                            crate::browser::host::run(
                                &host.browsers,
                                caller,
                                &identity.kind,
                                Some(*gate),
                                request.args,
                                &mut send,
                                &mut recv,
                            )
                            .await
                        }
                        Err(failure) => Done {
                            response: Response::error(failure),
                            audit_args: Value::Null,
                            outcome: failure.code.clone(),
                        },
                    },
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
            if let (Ok(gate), Some(gated), Some(judgment)) = (&gate, &gated, &judgment)
                && judgment.verdict.is_ok()
                && gated.once
            {
                gate.did(&gated.action, &done.outcome);
            }
            drop(turn);
            let decision = decision(&done.response);
            log(&format!(
                "{op}: {caller}: {decision}: {} {}",
                done.outcome,
                crate::redact::shown(op, &done.audit_args)
            ));
            let dashboard = host.dashboard.as_ref().map(|dashboard| {
                (
                    dashboard,
                    dashboard::describe(op, &done.audit_args, &done.outcome, started.elapsed()),
                )
            });
            let mut record = audit::record(
                caller,
                identity.agent_uid.clone(),
                op,
                decision,
                &done.outcome,
            );
            record.args = done.audit_args;
            record.gate = judgment.map(|judgment| judgment.gate);
            let response = match host.append(record).await {
                Ok(audit_id) => with_audit_id(done.response, audit_id),
                Err(message) => failed("audit_failed", message),
            };
            if let Some((dashboard, (target, result))) = dashboard {
                dashboard.call(Row {
                    at: jiff::Timestamp::now(),
                    caller: host.shown_name(&identity).await,
                    op: op.to_string(),
                    target,
                    result,
                    refused: decision == "deny",
                });
            }
            response
        }
    };
    answer(&mut send, &response).await;
}

/// Finishes when an exec is cut off by the host or loses its caller.
async fn cut_off(host: &Host, connection: &Connection) -> &'static str {
    tokio::select! {
        () = host.cut_off() => "interrupted",
        _ = connection.closed() => "caller_gone",
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
        let response = match host.append(record).await {
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
    /// Finishes when the host cuts off the operations still running.
    async fn cut_off(&self) {
        let mut stopping = self.stopping.subscribe();
        let _ = stopping
            .wait_for(|stopping| *stopping == Stopping::CuttingOff)
            .await;
    }

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
    /// The gate a caller's operations pass: the owner's own devices as
    /// the owner, an agent in the chat it named. An agent Plow gives no uid
    /// can't be judged, so nothing of it runs.
    fn gate<'a>(
        &'a self,
        identity: &'a Identity,
        chat: Option<&'a str>,
    ) -> Result<Gate<'a>, Failure> {
        if identity.kind == "device" {
            return Ok(Gate {
                keeper: &self.gatekeeper,
                caller: Caller::Owner,
            });
        }
        match identity.agent_uid.as_deref() {
            Some(uid) => Ok(Gate {
                keeper: &self.gatekeeper,
                caller: Caller::Agent { uid, chat },
            }),
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
            Err(Failure::failed(
                "audit_failed",
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

        std::fs::create_dir_all(home.join("Library/Developer")).unwrap();
        let shared = |folders: &[PathBuf]| shared_folders(folders, Some(&home));

        assert_eq!(
            shared(&[code.join("../docs"), code.clone()]).unwrap(),
            [docs, code]
        );
        // Inside the Library, but not what holds it.
        assert_eq!(
            shared(&[home.join("Library/Developer")]).unwrap(),
            [home.join("Library/Developer")]
        );
        assert_eq!(shared(&[]).unwrap(), Vec::<PathBuf>::new());
        for (folder, why) in [
            (home.join("missing"), "does not exist"),
            (home.join("notes.txt"), "not a folder"),
            (home.clone(), "Library"),
            (home.join("Library"), "Library"),
            (home.join(".."), "Library"),
            (PathBuf::from("/"), "Library"),
        ] {
            let failure = shared(std::slice::from_ref(&folder)).unwrap_err();
            assert_eq!(failure.code, "bad_folder", "{folder:?}");
            assert!(
                failure.message.contains(why),
                "{folder:?}: {}",
                failure.message
            );
        }
    }

    #[test]
    fn an_opt_in_inside_credentials_or_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let state = home.join("state");
        for path in [".ssh", ".config/gh", "code", "state/keys"] {
            std::fs::create_dir_all(home.join(path)).unwrap();
        }
        std::fs::write(home.join(".ssh/id_ed25519"), b"").unwrap();
        let hidden = crate::sandbox::never_readable(&home);
        let allowed = |path: &str| allowed_reads(&[home.join(path)], Some(&home), &hidden, &state);

        // A folder that holds them is allowed (the sandbox hides them in
        // it), unless it holds the Library: the home folder, or above it.
        std::fs::create_dir_all(home.join("code/.ssh")).unwrap();
        std::fs::create_dir_all(home.join("Library")).unwrap();
        let in_code = crate::sandbox::never_readable(&home.join("code"));
        let reads = allowed_reads(&[home.join("code")], None, &in_code, &state);
        assert_eq!(reads.unwrap(), [home.join("code")]);
        assert_eq!(allowed("code/../code").unwrap(), [home.join("code")]);
        for path in [".", "..", "Library"] {
            let failure = allowed(path).unwrap_err();
            assert_eq!(failure.code, "bad_folder", "{path}");
            assert!(failure.message.contains("Library"), "{path}");
        }
        for path in [".ssh", ".ssh/id_ed25519", ".config/gh", "state/keys"] {
            let failure = allowed(path).unwrap_err();
            assert_eq!(failure.code, "never_readable", "{path}");
        }
        assert_eq!(allowed("missing").unwrap_err().code, "bad_folder");
    }

    #[test]
    fn an_opt_in_inside_another_state_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for path in ["share/a/peer/sub", "share/b", "share/d/old"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        let marker = r#"{"format": 1, "written_by": "test"}"#;
        std::fs::write(root.join("share/a/peer/latch.json"), marker).unwrap();
        // Made before the marker: a key and a Plow login, no latch.json.
        std::fs::write(root.join("share/d/old/key"), "k").unwrap();
        std::fs::write(root.join("share/d/old/plow-token"), "t").unwrap();
        // A key alone is anyone's.
        std::fs::write(root.join("share/b/key"), "k").unwrap();

        let state = root.join("own-state");
        for path in ["share/a/peer/sub", "share/d/old"] {
            let inside = allowed_reads(&[root.join(path)], None, &[], &state);
            assert_eq!(inside.unwrap_err().code, "never_readable", "{path}");
        }
        let beside = allowed_reads(&[root.join("share/b")], None, &[], &state);
        assert_eq!(beside.unwrap(), [root.join("share/b")]);
    }

    #[test]
    fn a_linked_credential_keeps_its_target_hidden_once_the_link_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".docker")).unwrap();
        std::fs::create_dir_all(home.join("private")).unwrap();
        std::fs::write(home.join("private/login.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            home.join("private/login.json"),
            home.join(".docker/config.json"),
        )
        .unwrap();
        let hidden = with_targets(crate::sandbox::never_readable(&home));
        std::fs::remove_file(home.join(".docker/config.json")).unwrap();
        assert!(hidden.contains(&home.join(".docker/config.json")));
        assert!(hidden.contains(&home.join("private/login.json")));
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
