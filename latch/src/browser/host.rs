//! The host's side of `latch browser`: the `browser` operation.
//!
//! Each `open` starts a new session in a runtime of its own (`runtime.rs`):
//! a browser and worker on the engine `open` names, on a fresh profile that
//! lasts as long as the session and is removed with it, on `close`, idle
//! expiry, a failed launch or the host stopping. Sessions share nothing:
//! no cookies, logins, tabs, downloads or dialogs.
//!
//! A session is reachable only with its id, which `open` returns once (128
//! random bits), and only by the caller that opened it: an id this caller
//! has no session for is `no_such_session`, another caller's is
//! `not_owner`, and nothing reaches the session either way. `sessions`
//! lists the caller's sessions without their ids. The audit holds a short
//! hash of an id, never the id.
//!
//! The audit record of each call holds what the worker reports (tab,
//! origin and path, snapshot, target ref and role, how it was resolved),
//! never page text, field values or typed text: `type` records the text's
//! length and sha256.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::transport::{RecvStream, SendStream};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::judged;
use super::plan::Op;
use super::runtime::Runtime;
use super::session::Session;
use super::tags;
use crate::failure::{Exit, Failure};
use crate::gate::Gate;
use crate::serve::Done;
use crate::state::to_hex;
use crate::vault::host::Vault;
use crate::vault::sites;
use crate::wire::{self, Response};

/// The most of an `eval` script the audit keeps, as with `exec` argv.
const MAX_SCRIPT_AUDIT: usize = 4096;
/// The largest file `upload` takes.
const MAX_UPLOAD: u64 = 100 * 1024 * 1024;
/// How long a closing runtime may take before its worker is killed.
const CLOSING: Duration = Duration::from_secs(15);

pub struct Browsers {
    state_dir: PathBuf,
    /// Every session's engine runs with no window (`serve --headless-browsers`).
    headless: bool,
    /// The host's live view, which shows pending handoffs.
    dashboard: std::sync::Mutex<Option<crate::dashboard::Dashboard>>,
    /// Tests only: whether the host has a screen, in place of the check.
    #[cfg(test)]
    pub(super) screen: Option<bool>,
    /// Open sessions, by id.
    sessions: Mutex<HashMap<String, Arc<Entry>>>,
    pub(super) vault: Arc<Vault>,
    /// Plan requests under way, as `session/request`: a journal with no
    /// end reads as running while its request is here, else interrupted.
    running: std::sync::Mutex<HashSet<String>>,
    /// Tests only: the latch binary to start workers with, and local test
    /// origins read as the HTTPS sites they stand in for.
    #[cfg(test)]
    pub(super) test: Option<(PathBuf, Vec<(String, String)>)>,
    /// Tests only: the decision model's provider, in place of the host's.
    #[cfg(test)]
    pub(super) provider: Option<Arc<dyn crate::model::Provider>>,
}

/// Who a command comes from: the client key the gate verified, whether
/// Plow knows it as one of the owner's devices, and the gatekeeper's gate
/// its judged commands pass (none only for the browser's own tests).
#[derive(Clone, Copy)]
pub(super) struct Caller<'a> {
    pub(super) key: &'a str,
    pub(super) device: bool,
    pub(super) gate: Option<Gate<'a>>,
}

/// A caller that is not one of the owner's devices, and is not judged: a
/// caller in tests of the browser itself.
impl<'a> From<&'a str> for Caller<'a> {
    fn from(key: &'a str) -> Self {
        Caller {
            key,
            device: false,
            gate: None,
        }
    }
}

/// One open session, and what the host knows about it between calls.
pub(super) struct Entry {
    /// Held for the whole of each request: a session's requests take turns.
    pub(super) session: Mutex<Session>,
    pub(super) runtime: Arc<Runtime>,
    /// The key of the caller that opened the session.
    owner: String,
    /// A short hash of the session's id: what the audit records.
    pub(super) hash: String,
    /// The label `open --task` gave it, for listings and the audit.
    task: Option<String>,
    /// When the session last had a call, and the site its page was on.
    seen: std::sync::Mutex<(Instant, Option<String>)>,
    /// One plan request at a time; the session itself is held only for
    /// each worker call, never while the decision model answers.
    pub(super) serial: Mutex<()>,
    /// The confirmations issued in this session, used or not.
    pub(super) confirmations: std::sync::Mutex<Vec<super::steps::Confirmation>>,
    /// The owner has the session (`handoff`): its other calls wait outside.
    handing: std::sync::atomic::AtomicBool,
    /// The sites the gatekeeper let this session go to.
    judged_sites: std::sync::Mutex<HashSet<String>>,
}

impl Entry {
    /// The session's part of a call's audit record: its id hash, its task
    /// label and its engine.
    fn note(&self, audit: &mut Map<String, Value>) {
        audit.insert("session".into(), json!(self.hash));
        if let Some(task) = &self.task {
            audit.insert("task".into(), json!(task));
        }
        audit.insert("engine".into(), self.runtime.ready["engine"].clone());
    }

    /// The session's worker for one agent call: refused while the owner has
    /// the session, whenever the call was asked for.
    pub(super) async fn agent_session(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Session>, Failure> {
        let session = self.session.lock().await;
        if self.handing.load(Ordering::SeqCst) {
            return Err(super::handoff::handed_off());
        }
        Ok(session)
    }

    /// Starts a handoff; false if one is already under way.
    pub(super) fn begin_handoff(&self) -> bool {
        self.handing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(super) fn end_handoff(&self) {
        self.handing.store(false, Ordering::SeqCst);
    }

    /// The start of the caller's key, as a pending handoff names it.
    pub(super) fn owner_hint(&self) -> String {
        self.owner.chars().take(12).collect()
    }
}

impl Browsers {
    pub fn new(state_dir: PathBuf, vault: Arc<Vault>, headless: bool) -> Self {
        // Handoffs and sessions end with the host: none is left in a new one.
        let _ = std::fs::remove_dir_all(super::handoff::folder(&state_dir));
        super::profile::reap(&state_dir);
        Browsers {
            state_dir,
            headless,
            dashboard: Default::default(),
            #[cfg(test)]
            screen: None,
            sessions: Mutex::new(HashMap::new()),
            vault,
            running: Default::default(),
            #[cfg(test)]
            test: None,
            #[cfg(test)]
            provider: None,
        }
    }

    pub(super) fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    pub(super) fn running(&self, request: &str, on: bool) {
        let mut running = self.running.lock().expect("running lock");
        if on {
            running.insert(request.to_string());
        } else {
            running.remove(request);
        }
    }

    pub(super) fn is_running(&self, request: &str) -> bool {
        self.running.lock().expect("running lock").contains(request)
    }

    /// Shows pending handoffs on `dashboard` from now on.
    pub fn show_on(&self, dashboard: crate::dashboard::Dashboard) {
        *self.dashboard.lock().expect("dashboard lock") = Some(dashboard);
    }

    /// Whether a person at this host could act in its browser windows.
    pub(super) fn screen(&self) -> bool {
        #[cfg(test)]
        if let Some(screen) = self.screen {
            return screen;
        }
        super::handoff::screen(self.headless)
    }

    /// Shows on the dashboard that a plan in session `hash` finished one
    /// step (`shown`, as its answer has it).
    pub(super) fn progress(&self, hash: &str, request: &str, shown: &Value) {
        if let Some(dashboard) = &*self.dashboard.lock().expect("dashboard lock") {
            dashboard.progress(hash, request, shown);
        }
    }

    /// Shows the pending handoffs in the log, and on the dashboard.
    pub(super) fn notice_handoffs(&self) {
        let lines: Vec<String> = super::handoff::pending(&self.state_dir)
            .iter()
            .map(super::handoff::line)
            .collect();
        for line in &lines {
            crate::log::log(line);
        }
        if let Some(dashboard) = &*self.dashboard.lock().expect("dashboard lock") {
            dashboard.notices(lines);
        }
    }

    /// The decision model for one request.
    pub(super) fn model(&self) -> crate::model::Model {
        #[cfg(test)]
        if let Some(provider) = &self.provider {
            return crate::model::Model::new(Box::new(provider.clone()));
        }
        crate::model::Model::for_host(&self.state_dir)
    }

    /// An origin as the site rules read it. Outside tests, itself.
    pub(super) fn origin_as(&self, origin: &str) -> String {
        #[cfg(test)]
        if let Some((_, origins)) = &self.test
            && let Some((_, stands_for)) = origins.iter().find(|(local, _)| local == origin)
        {
            return stands_for.clone();
        }
        origin.to_string()
    }

    /// The live session `id`, if there is one. A session whose runtime
    /// ended (its worker ends it once it sat idle too long) is forgotten.
    async fn get(&self, id: &str) -> Option<Arc<Entry>> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get(id)?.clone();
        if !entry.runtime.alive() {
            sessions.remove(id);
            return None;
        }
        Some(entry)
    }

    /// Tests only: the session `id`, whoever opened it.
    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub(super) async fn session_for_test(&self, id: &str) -> Arc<Entry> {
        self.get(id).await.expect("a live session")
    }

    /// The running session `id`, if `caller` opened it.
    pub(super) async fn session(&self, id: &str, caller: &str) -> Result<Arc<Entry>, Failure> {
        let entry = self.get(id).await.ok_or_else(no_such_session)?;
        if entry.owner != caller {
            return Err(not_owner());
        }
        entry.seen.lock().expect("seen lock").0 = Instant::now();
        Ok(entry)
    }

    /// A new session on `engine` (`cft` or `camoufox`) for `caller`, in a
    /// runtime of its own. Answers the session's id, its only copy outside
    /// this map, and the session.
    pub(super) async fn open(
        &self,
        engine: &str,
        caller: &str,
        task: Option<&str>,
    ) -> Result<(String, Arc<Entry>), Failure> {
        let runtime = Arc::new(self.start(engine).await?);
        let session = Session::new(runtime.clone());
        let id = format!("s{}", letters(&rand::random::<[u8; 16]>()));
        let entry = Arc::new(Entry {
            session: Mutex::new(session),
            runtime,
            owner: caller.to_string(),
            hash: hash_of(&id),
            task: task.map(str::to_string),
            seen: std::sync::Mutex::new((Instant::now(), None)),
            serial: Mutex::new(()),
            confirmations: Default::default(),
            handing: Default::default(),
            judged_sites: Default::default(),
        });
        self.sessions.lock().await.insert(id.clone(), entry.clone());
        Ok((id, entry))
    }

    async fn start(&self, engine: &str) -> Result<Runtime, Failure> {
        // Checked here first, outside the worker's sandbox, so that a full
        // hash leaves its receipt and the worker's own check is quick.
        let (state_dir, name) = (self.state_dir.clone(), engine.to_string());
        tokio::task::spawn_blocking(move || super::engine::verify(&state_dir, &name))
            .await
            .expect("the engine check does not panic")?;
        #[cfg(test)]
        if let Some((latch, _)) = &self.test {
            return Runtime::start_with(
                latch,
                &self.state_dir,
                engine,
                super::runtime::IDLE,
                self.headless,
            )
            .await;
        }
        Runtime::start(&self.state_dir, engine, self.headless).await
    }

    /// Forgets the sessions whose runtime ended or that sat idle too long:
    /// their profiles go with them.
    pub async fn sweep(&self) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        for id in ids {
            self.get(&id).await;
        }
    }

    /// Closes every runtime at once, for a host that is stopping.
    pub async fn close_all(&self) {
        let mut closing = tokio::task::JoinSet::new();
        for (_, entry) in self.sessions.lock().await.drain() {
            closing.spawn(async move {
                let _ = tokio::time::timeout(CLOSING, entry.runtime.close()).await;
            });
        }
        closing.join_all().await;
    }

    /// The sessions `caller` opened, without their ids: task label,
    /// engine, the site it was last on, its state and how long since its
    /// last call. No worker is asked.
    pub(super) async fn list(&self, caller: &str) -> Value {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut listed = Vec::new();
        for id in ids {
            let Some(entry) = self.get(&id).await else {
                continue;
            };
            if entry.owner != caller {
                continue;
            }
            let (seen, site) = entry.seen.lock().expect("seen lock").clone();
            let state = if entry.runtime.filling.load(Ordering::SeqCst) {
                "paused"
            } else if entry.session.try_lock().is_err() {
                "busy"
            } else {
                "idle"
            };
            listed.push(json!({
                "task": entry.task,
                "engine": super::engine::named(entry.runtime.ready["engine"].as_str().unwrap_or_default()),
                "site": site,
                "state": state,
                "idle_s": seen.elapsed().as_secs(),
            }));
        }
        json!({"sessions": listed})
    }

    /// Ends session `id`, if `caller` opened it.
    pub(super) async fn close(&self, id: &str, caller: &str) -> Result<Value, Failure> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions.get(id).ok_or_else(no_such_session)?;
            if entry.owner != caller {
                return Err(not_owner());
            }
            sessions.remove(id).expect("found above")
        };
        self.end(&entry).await
    }

    /// Ends the session whose id hashes to `hash`, whoever opened it: how
    /// an owner device ends a session it sees on the dashboard. None when
    /// no session has that hash.
    async fn close_by_hash(
        &self,
        hash: &str,
        audit: &mut Map<String, Value>,
    ) -> Option<Result<Value, Failure>> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            let id = sessions
                .iter()
                .find(|(_, entry)| entry.hash == hash)
                .map(|(id, _)| id.clone())?;
            sessions.remove(&id)?
        };
        entry.note(audit);
        audit.insert("by_owner_device".into(), json!(true));
        Some(self.end(&entry).await)
    }

    /// A session the map no longer has: its browser closes, and its
    /// profile goes once the last call holding it has ended.
    async fn end(&self, entry: &Entry) -> Result<Value, Failure> {
        let _turn = entry.session.lock().await;
        let _ = tokio::time::timeout(CLOSING, entry.runtime.close()).await;
        Ok(json!({"closed": true}))
    }
}

/// `browser`: one `latch browser` command.
pub async fn run(
    browsers: &Browsers,
    caller: &str,
    kind: &str,
    gate: Option<Gate<'_>>,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let command = args["command"].as_str().unwrap_or_default().to_string();
    // `open` names an engine; every other session command, a session id.
    let name = if command == "open" {
        args["engine"].as_str().unwrap_or("chrome")
    } else {
        args["session"].as_str().unwrap_or_default()
    }
    .to_string();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    for key in ["ppid", "turn"] {
        if !args[key].is_null() {
            audit.insert(key.into(), args[key].clone());
        }
    }
    if command == "eval" {
        eval_audit(args["script"].as_str().unwrap_or_default(), &mut audit);
    }
    let mut input = None;
    if command == "upload" {
        match receive(&args, send, recv, &mut audit).await {
            Ok(bytes) => input = Some(bytes),
            Err(failure) => {
                return Done {
                    response: Response::error(&failure),
                    audit_args: Value::Object(audit),
                    outcome: failure.code,
                };
            }
        }
    }
    let caller = Caller {
        key: caller,
        device: kind == "device",
        gate,
    };
    let result = call(
        browsers,
        caller,
        &command,
        &name,
        &args,
        input.as_deref(),
        &mut audit,
    )
    .await;
    let (result, bytes) = match result {
        Ok(done) => done,
        Err(failure) => {
            return Done {
                response: Response::error(&failure),
                audit_args: Value::Object(audit),
                outcome: failure.code,
            };
        }
    };
    let Some(bytes) = bytes else {
        return Done {
            response: Response::Ok(result),
            audit_args: Value::Object(audit),
            outcome: "ok".into(),
        };
    };
    // A screenshot or a download: its details first, then its bytes, then
    // the last answer with their sha256.
    let sha256 = to_hex(&Sha256::digest(&bytes));
    audit.insert("size".into(), json!(bytes.len()));
    audit.insert("sha256".into(), json!(sha256));
    let first = serde_json::to_vec(&Response::Ok(result)).expect("serializes");
    let sent = match wire::write_frame(send, &first).await {
        Ok(()) => send.write_all(&bytes).await.is_ok(),
        Err(_) => false,
    };
    if !sent {
        return Done::failed(
            "connection_lost",
            "the client left".into(),
            Value::Object(audit),
        );
    }
    Done {
        response: Response::Ok(json!({"size": bytes.len(), "sha256": sha256})),
        audit_args: Value::Object(audit),
        outcome: "ok".into(),
    }
}

/// A `fill`: a suggestion taken by its id becomes the mapping it stands
/// for; one that releases a card or identity secret stops for the agent's
/// confirmation before any value is released.
pub(super) async fn fill(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: Caller<'_>,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    audit.insert("item".into(), args["item"].clone());
    let args = super::fill::resolve(browsers, entry, args).await?;
    if caller.gate.is_some() {
        let site = judged::site(browsers, entry).await?;
        judged::judge(
            caller.gate,
            judged::fill(browsers, &args, site, false),
            audit,
        )
        .await?;
    }
    fill_resolved(browsers, entry, session, caller.key, &args, audit).await
}

async fn fill_resolved(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    if let Some(title) = super::fill::releases_secret(browsers, args)? {
        if entry.runtime.filling.load(Ordering::SeqCst) {
            return Err(paused());
        }
        let mut args = args.clone();
        args["item_title"] = json!(title);
        return Err(super::steps::stop_fill(browsers, entry, session, caller, &args, audit).await);
    }
    super::fill::run(browsers, entry, args, audit).await
}

/// One command for `caller`, as `answer` runs it, the page in its answer
/// (or under its `page`) bounded to what the agent is shown (`shown`),
/// unless it asked for every element: `observe --all`, or `--scope`.
pub(super) async fn call(
    browsers: &Browsers,
    caller: Caller<'_>,
    command: &str,
    name: &str,
    args: &Value,
    input: Option<&[u8]>,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let mut answered = answer(browsers, caller, command, name, args, input, audit).await;
    if args["all"] != true && args["scope"].is_null() {
        match &mut answered {
            Ok((result, _)) => super::shown::bound(result),
            Err(failure) => super::shown::bound(&mut failure.details),
        }
    }
    answered
}

/// One command for `caller`: `open` on engine `name`, `sessions`, or a
/// command on session `name` (its id). An owner device may also `close`
/// any session by its hash.
async fn answer(
    browsers: &Browsers,
    caller: Caller<'_>,
    command: &str,
    name: &str,
    args: &Value,
    input: Option<&[u8]>,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let key = caller.key;
    match command {
        "sessions" => return Ok((browsers.list(key).await, None)),
        "open" => return open(browsers, caller, name, args, audit).await,
        "close" if caller.device => {
            if let Some(closed) = browsers.close_by_hash(name, audit).await {
                return closed.map(|closed| (closed, None));
            }
        }
        _ => {}
    }
    // The id a call names is recorded only as its hash, refused or not.
    audit.insert("session".into(), json!(hash_of(name)));
    // A request's history outlives its session and the host's restarts: it
    // is read by the session's id and the caller that sent it.
    if command == "do_history" {
        let request = args["request"].as_str().unwrap_or_default();
        audit.insert("request".into(), json!(request));
        return match args["shot"].as_str() {
            Some(shot) => {
                audit.insert("shot".into(), json!(shot));
                super::steps::shot(browsers, name, key, request, shot).map(|png| {
                    let meta = json!({"request": request, "shot": shot, "size": png.len()});
                    (meta, Some(png))
                })
            }
            None => {
                super::steps::history(browsers, name, key, request).map(|result| (result, None))
            }
        };
    }
    let entry = browsers.session(name, key).await?;
    entry.note(audit);
    // While the owner has the session, only `close` reaches it.
    if entry.handing.load(Ordering::SeqCst) && command != "close" {
        return Err(super::handoff::handed_off());
    }
    // Not on the web: refused before the gatekeeper is asked.
    if matches!(command, "goto" | "tab")
        && let Some(url) = args["url"].as_str()
    {
        off_web(browsers, url).await?;
    }
    if caller.gate.is_some()
        && let Some(action) = judged_action(browsers, &entry, command, args).await?
    {
        judged::judge(caller.gate, action, audit).await?;
    }
    let result = match command {
        "close" => browsers.close(name, key).await.map(|closed| (closed, None)),
        "fill" => fill(browsers, &entry, name, caller, args, audit)
            .await
            .map(|result| (result, None)),
        "do" => super::steps::run(browsers, &entry, name, key, caller.gate, args, audit)
            .await
            .map(|result| (result, None)),
        "handoff" => super::handoff::run(browsers, &entry, args, audit)
            .await
            .map(|result| (result, None)),
        "find" => super::steps::find(&entry, args, audit)
            .await
            .map(|result| (result, None)),
        "wait" => super::steps::wait(&entry, args, audit)
            .await
            .map(|result| (result, None)),
        _ => worker(browsers, &entry, command, args, input, audit).await,
    };
    if matches!(command, "tab" | "goto")
        && result.is_ok()
        && let Some(site) = args["url"].as_str().and_then(sites::site_of)
    {
        entry
            .judged_sites
            .lock()
            .expect("judged sites lock")
            .insert(site);
    }
    if result.is_ok() {
        last_site(&entry, audit);
    }
    result
}

/// What the gatekeeper judges of a session command before it runs, if
/// anything: a tab opened or a `goto` on a site the session has not been
/// judged for,
/// a plan (with a card or identity fill it confirms first), a confirmed
/// fill alone, and every `eval`, upload and download. A `fill` is judged
/// once its suggestion is resolved (`fill`).
async fn judged_action(
    browsers: &Browsers,
    entry: &Entry,
    command: &str,
    args: &Value,
) -> Result<Option<judged::Action>, Failure> {
    Ok(Some(match command {
        "tab" | "goto" => {
            let Some(url) = args["url"].as_str() else {
                return Ok(None);
            };
            let known = sites::site_of(url).is_some_and(|site| {
                entry
                    .judged_sites
                    .lock()
                    .expect("judged sites lock")
                    .contains(&site)
            });
            if known {
                return Ok(None);
            }
            judged::open_site(browsers, url)
        }
        "do" => {
            let confirmed = match args["confirm"].as_str() {
                Some(id) => match super::steps::fill_to_confirm(entry, id) {
                    Some(fill) => {
                        let site = judged::site(browsers, entry).await?;
                        Some(judged::fill(browsers, &fill, site, true))
                    }
                    None => None,
                },
                None => None,
            };
            // A malformed plan is refused before anything runs: there is
            // nothing to judge.
            let plan = serde_json::from_value::<super::plan::Plan>(args["plan"].clone())
                .ok()
                .filter(|plan| plan.validate().is_ok());
            // What `act` sends, and says so, is a single click, keypress or
            // scroll: one literal step that is no fill, which the risk
            // decision judges, not the gatekeeper. Every `do` plan is
            // judged, a one-step one too.
            let one_action = |plan: &super::plan::Plan| {
                args["act"] == true
                    && match plan.steps.as_slice() {
                        [step] => step.literal.as_ref().is_some_and(|l| l.op != Op::Fill),
                        _ => false,
                    }
            };
            match plan {
                Some(mut plan) if !plan.steps.is_empty() && !one_action(&plan) => {
                    plan.resolve_facts();
                    let page = judged::page(browsers, entry).await?;
                    judged::plan(browsers, &plan, page, confirmed)
                }
                _ => match confirmed {
                    Some(fill) => fill,
                    None => return Ok(None),
                },
            }
        }
        "eval" => judged::Action::RunScript {
            site: judged::site(browsers, entry).await?,
            script: args["script"].as_str().unwrap_or_default().into(),
        },
        "upload" => judged::upload(args, judged::site(browsers, entry).await?),
        "download_get" => {
            judged::download(
                browsers,
                entry,
                args["download"].as_str().unwrap_or_default(),
            )
            .await?
        }
        _ => return Ok(None),
    }))
}

/// Refuses an address the host's browser does not go to (another scheme,
/// or a local address the owner has not allowed) before a session, an
/// engine or the gatekeeper is involved. The worker checks it again.
async fn off_web(browsers: &Browsers, url: &str) -> Result<(), Failure> {
    let local = super::egress::local_allowed(&browsers.state_dir)?;
    let url = url.to_string();
    tokio::task::spawn_blocking(move || super::egress::requested(&url, local))
        .await
        .expect("the address check does not panic")
}

/// `open`: a new session on engine `name` (`chrome` or `camoufox`), its
/// page loaded. The answer carries the session's id and its engine; a
/// session that could not load its page is ended. An agent's site is
/// judged before any session starts.
async fn open(
    browsers: &Browsers,
    caller: Caller<'_>,
    name: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let engine = super::engine::unnamed(name).ok_or_else(|| {
        Failure::usage(
            "bad_arguments",
            format!("no browser engine {name:?}: use chrome or camoufox"),
        )
    })?;
    let url = args["url"].as_str();
    if let Some(url) = url {
        off_web(browsers, url).await?;
    }
    if let Some(url) = url {
        let action = judged::open_site(browsers, url);
        judged::judge(caller.gate, action, audit).await?;
    }
    open_judged(browsers, caller.key, engine, args, audit).await
}

async fn open_judged(
    browsers: &Browsers,
    caller: &str,
    engine: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let (id, entry) = browsers.open(engine, caller, args["task"].as_str()).await?;
    entry.note(audit);
    let loaded = worker(browsers, &entry, "open", args, None, audit).await;
    match loaded {
        Ok((mut result, bytes)) => {
            last_site(&entry, audit);
            if let Some(site) = args["url"].as_str().and_then(sites::site_of) {
                entry
                    .judged_sites
                    .lock()
                    .expect("judged sites lock")
                    .insert(site);
            }
            if let Some(object) = result.as_object_mut() {
                object.insert("session".into(), json!(id));
                object.insert("engine".into(), json!(super::engine::named(engine)));
                if crate::sandbox::page_isolation_off() {
                    object.insert(
                        "page_isolation".into(),
                        json!(crate::sandbox::PAGE_ISOLATION_OFF),
                    );
                }
            }
            Ok((result, bytes))
        }
        Err(failure) => {
            let opened = browsers.sessions.lock().await.remove(&id);
            if let Some(entry) = opened {
                let _ = browsers.end(&entry).await;
            }
            Err(failure)
        }
    }
}

/// Passes one call to the session's worker.
async fn worker(
    browsers: &Browsers,
    entry: &Entry,
    command: &str,
    args: &Value,
    input: Option<&[u8]>,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let call = worker_call(command, args)?;
    if entry.runtime.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let mut session = entry.agent_session().await?;
    let reply = match session.call_with(call, input).await {
        Ok(reply) => {
            if let Some(worker) = reply.audit.as_object() {
                audit.extend(worker.clone());
            }
            Ok(reply)
        }
        Err(mut failure) => {
            take_audit(&mut failure, audit);
            Err(failure)
        }
    };
    // Agent code was sent to this page's site: it is noted, refused or
    // not, for every later fill there in this session.
    if command == "eval"
        && let Some(site) = audit
            .get("origin")
            .and_then(Value::as_str)
            .and_then(sites::site_of)
    {
        entry.runtime.note_agent_code(&site);
    }
    let reply = reply?;
    let mut result = reply.result;
    if command == "observe" {
        let inventory = session
            .call(json!({"op": "fill_inventory", "snapshot": result["snapshot"]}))
            .await?;
        result["fill"] = super::fill::offered(browsers, &inventory.result);
    }
    Ok((result, reply.bytes))
}

/// The page the call answered on: its site, for `sessions`.
fn last_site(entry: &Entry, audit: &Map<String, Value>) {
    if let Some(site) = audit
        .get("origin")
        .and_then(Value::as_str)
        .and_then(sites::site_of)
    {
        entry.seen.lock().expect("seen lock").1 = Some(site);
    }
}

/// A short hash of a session id: what the audit records.
pub(crate) fn hash_of(id: &str) -> String {
    letters(&Sha256::digest(id.as_bytes())[..6])
}

/// Bytes written as letters, `a` to `p` for each half byte. Session ids
/// and their hashes are letters only, so none can read as a code or card
/// digits in a grep of results and logs for a vault value.
fn letters(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 15])
        .map(|nibble| char::from(b'a' + nibble))
        .collect()
}

/// The worker's call for a client command: the command's own arguments,
/// renamed into `core::Call`'s shape. Anything else in the request is
/// dropped here.
fn worker_call(command: &str, args: &Value) -> Result<Value, Failure> {
    let pick = |keys: &[&str]| {
        let mut call = Map::new();
        for key in keys {
            if !args[*key].is_null() {
                call.insert((*key).to_string(), args[*key].clone());
            }
        }
        call
    };
    let mut call = match command {
        // The active tab loads the URL, as `open` loads a new session's.
        "open" | "goto" => pick(&["url"]),
        "observe" => pick(&["tab", "scope"]),
        "text" | "screenshot" => Map::new(),
        "tab" => pick(&["action", "tab", "url"]),
        "dialog" => pick(&["accept", "text"]),
        "eval" => pick(&["script"]),
        "upload" => pick(&["snapshot", "target", "name", "size"]),
        "downloads" => Map::new(),
        "download_get" => pick(&["download"]),
        _ => {
            return Err(Failure::usage(
                "bad_command",
                format!("unknown browser command {command:?}"),
            ));
        }
    };
    let op = if command == "goto" { "open" } else { command };
    call.insert("op".into(), json!(op));
    Ok(Value::Object(call))
}

/// An `eval` script as the audit keeps it: verbatim up to 4 KiB, with its
/// full size and sha256, and the workarounds it reaches for.
fn eval_audit(script: &str, audit: &mut Map<String, Value>) {
    let (kept, cut) = tags::capped(script, MAX_SCRIPT_AUDIT);
    audit.insert("script".into(), json!(kept));
    if cut {
        audit.insert("script_truncated".into(), json!(true));
    }
    audit.insert("script_bytes".into(), json!(script.len()));
    audit.insert(
        "script_sha256".into(),
        json!(to_hex(&Sha256::digest(script.as_bytes()))),
    );
    audit.insert(
        "workaround_tags".into(),
        json!(tags::workaround_tags(script)),
    );
}

/// An upload's bytes from the client, after telling it to go ahead. The
/// audit keeps the file's name, size and sha256.
async fn receive(
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
    audit: &mut Map<String, Value>,
) -> Result<Vec<u8>, Failure> {
    let (Some(size), Some(name)) = (args["size"].as_u64(), args["name"].as_str()) else {
        return Err(Failure::usage(
            "bad_arguments",
            "an upload needs its size and name".into(),
        ));
    };
    audit.insert("file_name".into(), json!(name));
    audit.insert("file_size".into(), json!(size));
    if size > MAX_UPLOAD {
        return Err(Failure::usage(
            "file_too_large",
            format!("uploads are at most {MAX_UPLOAD} bytes"),
        ));
    }
    let go = serde_json::to_vec(&Response::Ok(json!({}))).expect("serializes");
    let lost = || {
        Failure::new(
            Exit::Unknown,
            "connection_lost",
            "the upload broke off".into(),
        )
    };
    wire::write_frame(send, &go).await.map_err(|_| lost())?;
    let mut bytes = Vec::with_capacity(size as usize);
    let sha256 = wire::copy(recv, &mut bytes, size)
        .await
        .map_err(|_| lost())?;
    audit.insert("file_sha256".into(), json!(sha256));
    Ok(bytes)
}

/// The worker's audit record of a failed call rides in the failure's
/// details; it goes to the audit, never to the agent.
pub(super) fn take_audit(failure: &mut Failure, audit: &mut Map<String, Value>) {
    if let Some(details) = failure.details.as_object_mut() {
        if let Some(Value::Object(worker)) = details.remove("audit") {
            audit.extend(worker);
        }
        if details.is_empty() {
            failure.details = Value::Null;
        }
    }
}

pub(super) fn paused() -> Failure {
    Failure::failed(
        "session_paused",
        "a vault fill holds this browser; nothing can be read or done until it finishes".into(),
    )
}

fn not_owner() -> Failure {
    Failure::new(
        Exit::Denied,
        "not_owner",
        "that session was opened by another caller, and only that caller can use it; \
         open a session of your own with `latch browser open`"
            .into(),
    )
}

fn no_such_session() -> Failure {
    Failure::failed(
        "no_such_session",
        "you have no session with that id (it may have closed, or idled out); \
         `latch browser open` starts one and answers its id"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_call_carries_only_its_commands_arguments() {
        let args = json!({
            "command": "upload", "session": "default", "ppid": 7,
            "target": "e3", "snapshot": "o2", "name": "a.csv", "size": 3, "url": "ignored"
        });
        assert_eq!(
            worker_call("upload", &args).unwrap(),
            json!({"op": "upload", "target": "e3", "snapshot": "o2", "name": "a.csv", "size": 3})
        );
        assert_eq!(worker_call("text", &args).unwrap(), json!({"op": "text"}));
        assert_eq!(
            worker_call("goto", &args).unwrap(),
            json!({"op": "open", "url": "ignored"}),
            "the active tab loads it, as a new session's does"
        );
        assert_eq!(worker_call("hover", &args).unwrap_err().code, "bad_command");
    }

    #[test]
    fn an_eval_script_is_audited_verbatim_up_to_4_kib_with_its_tags() {
        let script = "window.fetch = (...a) => spy(...a); 1";
        let mut audit = Map::new();
        eval_audit(script, &mut audit);
        assert_eq!(audit["script"], script);
        assert!(audit.get("script_truncated").is_none());
        assert_eq!(audit["workaround_tags"], json!(["patches_network"]));
        assert_eq!(audit["script_bytes"], script.len());

        let long = format!("{}// end", "x;".repeat(3000));
        let mut audit = Map::new();
        eval_audit(&long, &mut audit);
        assert_eq!(audit["script"].as_str().unwrap().len(), MAX_SCRIPT_AUDIT);
        assert_eq!(audit["script_truncated"], true);
        assert_eq!(audit["script_bytes"], long.len());
        assert_eq!(
            audit["script_sha256"],
            to_hex(&Sha256::digest(long.as_bytes()))
        );
    }

    #[test]
    fn a_failed_calls_audit_goes_to_the_record_not_the_agent() {
        let mut failure = Failure::failed("stale_snapshot", "observe again".into());
        failure.details =
            json!({"audit": {"target": "e1", "origin": "https://a.example"}, "snapshot": "o4"});
        let mut audit = Map::new();
        take_audit(&mut failure, &mut audit);
        assert_eq!(
            Value::Object(audit),
            json!({"target": "e1", "origin": "https://a.example"})
        );
        assert_eq!(failure.details, json!({"snapshot": "o4"}));
        let mut bare = Failure::failed("timeout", "slow".into());
        bare.details = json!({"audit": {}});
        take_audit(&mut bare, &mut Map::new());
        assert!(bare.details.is_null());
    }
}
