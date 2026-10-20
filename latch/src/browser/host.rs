//! The host's side of `latch browser`: the `browser` operation. It keeps
//! one session per profile (a session is named for its profile), starts a
//! session's worker on `open`, and passes each call to it, one at a time.
//! A session belongs to the caller that opened it: while it runs, any
//! other caller's `open` of its profile is refused with `profile_busy`,
//! every other command on it with `not_owner`, and `sessions` leaves it
//! out.
//!
//! The audit record of each call holds what the worker reports (tab,
//! origin and path, snapshot, target ref and role, how it was resolved),
//! never page text, field values or typed text: `type` records the text's
//! length and sha256.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::{RecvStream, SendStream};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::session::Session;
use super::tags;
use crate::failure::{Exit, Failure};
use crate::serve::Done;
use crate::state::to_hex;
use crate::vault::host::Vault;
use crate::wire::{self, Response};

/// The profile, and so the session, a call names when it names none.
pub const DEFAULT_SESSION: &str = "default";
/// The most of an `eval` script the audit keeps, as with `exec` argv.
const MAX_SCRIPT_AUDIT: usize = 4096;
/// The largest file `upload` takes.
const MAX_UPLOAD: u64 = 100 * 1024 * 1024;
/// How long a closing session may take before its worker is killed.
const CLOSING: Duration = Duration::from_secs(15);

pub struct Browsers {
    state_dir: PathBuf,
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
    pub(super) provider: Option<Arc<dyn super::model::Provider>>,
}

/// One open session, and what the host knows about it between calls.
pub(super) struct Entry {
    pub(super) session: Mutex<Session>,
    /// The key of the caller that opened the session.
    owner: String,
    /// A vault fill holds the session: every other call is answered
    /// `session_paused` at once rather than waiting for it.
    pub(super) filling: AtomicBool,
    /// Agent code (`eval`) has run in this session, so a value filled
    /// later may have been seen by it: every fill's audit says so.
    pub(super) agent_code: AtomicBool,
    /// One plan request at a time; the session itself is held only for
    /// each worker call, never while the decision model answers.
    pub(super) serial: Mutex<()>,
    /// The confirmations issued in this session, used or not.
    pub(super) confirmations: std::sync::Mutex<Vec<super::steps::Confirmation>>,
    /// The snapshot of the page the latest stop returned, for the `stop`
    /// step policy.
    pub(super) last_stop: std::sync::Mutex<Option<String>>,
}

impl Browsers {
    pub fn new(state_dir: PathBuf, vault: Arc<Vault>) -> Self {
        Browsers {
            state_dir,
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

    /// The decision model for one request.
    pub(super) fn model(&self) -> super::model::Model {
        #[cfg(test)]
        if let Some(provider) = &self.provider {
            return super::model::Model::new(Box::new(provider.clone()));
        }
        super::model::Model::for_host(&self.state_dir)
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

    /// The live session `name`, if there is one. A session whose worker
    /// ended (it closed itself after sitting idle) is forgotten; one busy
    /// with a call is alive.
    async fn get(&self, name: &str) -> Option<Arc<Entry>> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get(name)?.clone();
        if let Ok(mut session) = entry.session.try_lock()
            && !session.alive()
        {
            sessions.remove(name);
            return None;
        }
        Some(entry)
    }

    /// The running session `name`, if `caller` opened it.
    pub(super) async fn session(&self, name: &str, caller: &str) -> Result<Arc<Entry>, Failure> {
        let entry = self.get(name).await.ok_or_else(|| no_session(name))?;
        if entry.owner != caller {
            return Err(not_owner(name));
        }
        Ok(entry)
    }

    /// The session on `profile`, started for `caller` if it is not running.
    /// The caller that opened a running session gets it back; any other
    /// caller is refused, and the session is left as it is.
    pub(super) async fn open(&self, profile: &str, caller: &str) -> Result<Arc<Entry>, Failure> {
        if let Some(entry) = self.get(profile).await {
            if entry.owner != caller {
                return Err(profile_busy(profile));
            }
            return Ok(entry);
        }
        #[cfg(test)]
        let started = match &self.test {
            Some((latch, _)) => {
                Session::start_with(latch, &self.state_dir, profile, super::session::IDLE).await?
            }
            None => Session::start(&self.state_dir, profile).await?,
        };
        #[cfg(not(test))]
        let started = Session::start(&self.state_dir, profile).await?;
        let entry = Arc::new(Entry {
            session: Mutex::new(started),
            owner: caller.to_string(),
            filling: AtomicBool::new(false),
            agent_code: AtomicBool::new(false),
            serial: Mutex::new(()),
            confirmations: Default::default(),
            last_stop: Default::default(),
        });
        self.sessions
            .lock()
            .await
            .insert(profile.to_string(), entry.clone());
        Ok(entry)
    }

    /// Closes every session, for a host that is stopping.
    pub async fn close_all(&self) {
        let sessions: Vec<_> = self.sessions.lock().await.drain().collect();
        for (_, entry) in sessions {
            let close = async {
                entry
                    .session
                    .lock()
                    .await
                    .call(json!({"op": "close"}))
                    .await
            };
            let _ = tokio::time::timeout(CLOSING, close).await;
        }
    }

    /// The sessions `caller` opened, with their tabs. Another caller's
    /// sessions are left out before any worker is asked.
    pub(super) async fn list(&self, caller: &str) -> Result<Value, Failure> {
        let names: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut listed = Vec::new();
        for name in names {
            let Ok(entry) = self.session(&name, caller).await else {
                continue;
            };
            let mut session = entry.session.lock().await;
            let engine = session.ready["engine"].clone();
            let tabs = session.call(json!({"op": "tabs"})).await?.result["tabs"].clone();
            listed.push(json!({"session": name, "profile": name, "engine": engine, "tabs": tabs}));
        }
        Ok(json!({"sessions": listed}))
    }

    pub(super) async fn close(&self, name: &str, caller: &str) -> Result<Value, Failure> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions.get(name).ok_or_else(|| no_session(name))?;
            if entry.owner != caller {
                return Err(not_owner(name));
            }
            sessions.remove(name).expect("found above")
        };
        let mut session = entry.session.lock().await;
        let closed = session.call(json!({"op": "close"})).await?;
        Ok(json!({"session": name, "closed": closed.result["closed"]}))
    }
}

/// `browser`: one `latch browser` command.
pub async fn run(
    browsers: &Browsers,
    caller: &str,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let command = args["command"].as_str().unwrap_or_default().to_string();
    let name = args["session"]
        .as_str()
        .or(args["profile"].as_str())
        .unwrap_or(DEFAULT_SESSION)
        .to_string();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    audit.insert("session".into(), json!(name));
    audit.insert("profile".into(), json!(name));
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
    let result = match command.as_str() {
        "login" | "fill" | "fill_code" => match browsers.session(&name, caller).await {
            Ok(entry) => fill(browsers, &entry, &name, caller, &command, &args, &mut audit)
                .await
                .map(|result| (result, None)),
            Err(failure) => Err(failure),
        },
        // A request's history outlives its session and the host's restarts.
        "do_history" => {
            let request = args["request"].as_str().unwrap_or_default();
            audit.insert("request".into(), json!(request));
            match args["shot"].as_str() {
                Some(shot) => {
                    audit.insert("shot".into(), json!(shot));
                    super::steps::shot(browsers, &name, caller, request, shot).map(|png| {
                        let meta = json!({"request": request, "shot": shot, "size": png.len()});
                        (meta, Some(png))
                    })
                }
                None => super::steps::history(browsers, &name, caller, request)
                    .map(|result| (with_session(result, &name), None)),
            }
        }
        "do" | "find" | "wait" => match browsers.session(&name, caller).await {
            Ok(entry) => match command.as_str() {
                "do" => super::steps::run(browsers, &entry, &name, caller, &args, &mut audit).await,
                "find" => super::steps::find(&entry, &args, &mut audit).await,
                _ => super::steps::wait(&entry, &args, &mut audit).await,
            }
            .map(|result| (with_session(result, &name), None)),
            Err(failure) => Err(failure),
        },
        "sessions" => browsers.list(caller).await.map(|listed| (listed, None)),
        "close" => browsers
            .close(&name, caller)
            .await
            .map(|closed| (closed, None)),
        _ => {
            call(
                browsers,
                caller,
                &command,
                &name,
                &args,
                input.as_deref(),
                &mut audit,
            )
            .await
        }
    };
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

/// `login`, `fill-code`, and a `fill`: one that releases a secret field
/// stops for the agent's confirmation before any value is released.
pub(super) async fn fill(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: &str,
    command: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    if command == "fill" {
        audit.insert("item".into(), args["item"].clone());
        if let Some(title) = super::fill::releases_secret(browsers, args)? {
            if entry.filling.load(Ordering::SeqCst) {
                return Err(paused());
            }
            let mut args = args.clone();
            args["item_title"] = json!(title);
            return Err(
                super::steps::stop_fill(browsers, entry, session, caller, &args, audit).await,
            );
        }
    }
    super::fill::run(browsers, entry, command, args, audit).await
}

/// Passes one call to the session's worker, starting the session on `open`.
pub(super) async fn call(
    browsers: &Browsers,
    caller: &str,
    command: &str,
    name: &str,
    args: &Value,
    input: Option<&[u8]>,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Option<Vec<u8>>), Failure> {
    let call = worker_call(command, args)?;
    let entry = if command == "open" {
        browsers.open(name, caller).await?
    } else {
        browsers.session(name, caller).await?
    };
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    if command == "eval" {
        entry.agent_code.store(true, Ordering::SeqCst);
    }
    let mut session = entry.session.lock().await;
    audit.insert("engine".into(), session.ready["engine"].clone());
    let reply = match session.call_with(call, input).await {
        Ok(reply) => reply,
        Err(mut failure) => {
            take_audit(&mut failure, audit);
            return Err(failure);
        }
    };
    if let Some(worker) = reply.audit.as_object() {
        audit.extend(worker.clone());
    }
    let mut result = reply.result;
    if command == "observe" {
        let page = audit["origin"]
            .as_str()
            .map(|origin| browsers.origin_as(origin));
        let fields: Vec<String> = field_frames(&result)
            .iter()
            .map(|origin| browsers.origin_as(origin))
            .collect();
        result["vault"] = json!(fitting(&browsers.vault, page.as_deref(), &fields));
    }
    if let Some(object) = result.as_object_mut() {
        object.insert("session".into(), json!(name));
    }
    Ok((result, reply.bytes))
}

fn with_session(mut result: Value, name: &str) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("session".into(), json!(name));
    }
    result
}

/// The vault items that fit this page, for `observe`: allowed by their
/// kind's site rule to fill a field in one of `fields` (the origins of
/// frames holding a field) under the top-level `page`. Titles and handles
/// only, never a value.
fn fitting(vault: &Vault, page: Option<&str>, fields: &[String]) -> Vec<Value> {
    let Some(page) = page else {
        return Vec::new();
    };
    vault
        .list_if_present()
        .into_iter()
        .filter(|meta| {
            fields
                .iter()
                .any(|frame| crate::vault::sites::allows(meta, frame, page).is_ok())
        })
        .map(|meta| json!({"item": meta.item, "kind": meta.kind, "title": meta.title}))
        .collect()
}

/// The origins of the frames in an observed element list that hold a field
/// a value could go into.
fn field_frames(observed: &Value) -> Vec<String> {
    let origin_of = |label: &str| {
        observed["frames"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["frame"] == label)
            .and_then(|f| f["origin"].as_str())
            .or(if label == "f0" {
                observed["url"].as_str()
            } else {
                None
            })
            .map(|origin| crate::browser::core::origin_and_path(origin).0)
    };
    let mut origins: Vec<String> = Vec::new();
    for element in observed["elements"].as_array().into_iter().flatten() {
        if !matches!(
            element["role"].as_str(),
            Some("textbox" | "searchbox" | "combobox" | "spinbutton")
        ) {
            continue;
        }
        let label = element["frame"].as_str().unwrap_or("f0");
        if let Some(origin) = origin_of(label)
            && !origins.contains(&origin)
        {
            origins.push(origin);
        }
    }
    origins
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
        "open" => pick(&["url"]),
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
    call.insert("op".into(), json!(command));
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
        "a vault fill holds this session; nothing can be read or done until it finishes".into(),
    )
}

fn profile_busy(profile: &str) -> Failure {
    let mut failure = Failure::failed(
        "profile_busy",
        format!(
            "profile {profile} is in use by another caller's session; \
             open another profile with --profile, or try again once that session closes"
        ),
    );
    failure.details = json!({"profile": profile});
    failure
}

fn not_owner(name: &str) -> Failure {
    let mut failure = Failure::new(
        Exit::Denied,
        "not_owner",
        format!(
            "session {name} was opened by another caller, and only that caller can use it; \
             open a session of your own with `latch browser open --profile`"
        ),
    );
    failure.details = json!({"session": name});
    failure
}

pub(super) fn no_session(name: &str) -> Failure {
    Failure::new(
        Exit::Usage,
        "no_session",
        format!("there is no open session {name}; start one with `latch browser open`"),
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
    fn field_frames_are_the_origins_of_frames_holding_fields() {
        let observed = json!({
            "url": "https://www.portal.example/home?x=1",
            "frames": [
                {"frame": "f0", "origin": "https://www.portal.example"},
                {"frame": "f1", "origin": "https://login.shop.example", "parent": "f0"},
                {"frame": "f2", "origin": "https://ads.example", "parent": "f0"},
            ],
            "elements": [
                {"ref": "e1", "role": "link"},
                {"ref": "e2", "role": "textbox", "frame": "f1"},
                {"ref": "e3", "role": "button", "frame": "f2"},
            ],
        });
        assert_eq!(field_frames(&observed), ["https://login.shop.example"]);
        let single =
            json!({"url": "https://shop.example/login", "elements": [{"role": "textbox"}]});
        assert_eq!(field_frames(&single), ["https://shop.example"]);
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
