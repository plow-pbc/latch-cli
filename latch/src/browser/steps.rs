//! The host's half of `act`, `find` and `wait`: targets named by ref,
//! exact name or description; typed and sentence expectations; step lists;
//! the review of risky clicks; and each request's history.
//!
//! The decision model (`model.rs`) runs here, never in the worker. The
//! worker reads the page, acts and waits on typed conditions; the host
//! asks the model which element a description means, whether a stated
//! sentence holds, and whether a click on a page the agent hasn't seen
//! can't be undone.
//!
//! **A list** (`act --steps`, or one `act`, a list of one) is checked whole
//! before its first step runs: shape, the trial cap, and every sentence
//! expectation (refused as `vague_condition` if it names a task, or
//! `assist_off` with no model). Each step's target is resolved on the page
//! as it is at that step. The list stops at the first unresolved target,
//! refusal, unmet or unsure expectation, login wall or CAPTCHA; later
//! steps are not sent, and nothing is ever replayed or retried.
//!
//! **History.** The client names each request with its own id. The host
//! keeps every request's per-step outcomes (not sent, dispatched, unknown;
//! expectation verdicts) for the session's lifetime, updated as each step
//! ends, so `act --history R` answers even while the list runs or after
//! its reply was lost.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::core::Action;
use super::host::{Browsers, Entry, paused, take_audit};
use super::model::{self, CHECKABLE_P, CHOOSE_P, Model, RISKY_P};
use super::session::Session;
use crate::failure::{Exit, Failure};
use crate::state::to_hex;

/// The most steps one list may hold, for the trial.
pub const MAX_STEPS: usize = 10;
/// How long an expectation waits by default, and at most.
pub const WITHIN: f64 = 10.0;
const MAX_WITHIN: f64 = 120.0;
/// How often a sentence is judged again while it waits.
const JUDGE_EVERY: Duration = Duration::from_millis(1000);

/// One step, as the client sends it.
#[derive(Deserialize, Debug, Clone)]
pub struct Step {
    snapshot: Option<String>,
    action: Action,
    target: Option<String>,
    text: Option<String>,
    at: Option<(f64, f64)>,
    #[serde(default)]
    expect: Expect,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Expect {
    url: Option<String>,
    text: Option<String>,
    /// `eN=<value|checked|unchecked>`.
    field: Option<String>,
    /// `--expect-download complete`.
    #[serde(default)]
    download: bool,
    /// A sentence the decision model judges.
    sentence: Option<String>,
    within: Option<f64>,
}

impl Expect {
    fn within(&self) -> Duration {
        Duration::from_secs_f64(self.within.unwrap_or(WITHIN))
    }
}

/// Each session's requests and their per-step outcomes, oldest first.
#[derive(Default)]
pub struct History {
    requests: HashMap<String, Value>,
}

impl History {
    fn start(&mut self, id: &str, steps: usize) -> Result<(), Failure> {
        if self.requests.contains_key(id) {
            return Err(Failure::usage(
                "duplicate_request",
                format!(
                    "request {id} was already sent to this session; read it with `act --history {id}`, never resend it"
                ),
            ));
        }
        let trace: Vec<Value> = (1..=steps)
            .map(|n| json!({"step": n, "dispatch": "not_sent"}))
            .collect();
        self.requests.insert(
            id.to_string(),
            json!({"request": id, "status": "running", "trace": trace}),
        );
        Ok(())
    }

    fn step(&mut self, id: &str, step: &Map<String, Value>) {
        let n = step["step"].as_u64().unwrap_or(1) as usize;
        if let Some(slot) = self
            .requests
            .get_mut(id)
            .and_then(|r| r["trace"].get_mut(n - 1))
        {
            *slot = Value::Object(step.clone());
        }
    }

    fn finish(&mut self, id: &str, status: &str) {
        if let Some(request) = self.requests.get_mut(id) {
            request["status"] = json!(status);
        }
    }
}

/// `act --history R`.
pub fn history(entry: &Entry, request: &str) -> Result<Value, Failure> {
    entry
        .history
        .lock()
        .expect("history lock")
        .requests
        .get(request)
        .cloned()
        .ok_or_else(|| {
            Failure::failed(
                "no_such_request",
                format!("this session has no request {request}"),
            )
        })
}

/// One call to the worker; its audit fills in what the request's audit
/// lacks (tab, origin, path), and is returned for the step's own record.
async fn call(
    session: &mut Session,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Map<String, Value>), Failure> {
    match session.call(call).await {
        Ok(reply) => {
            let worker = reply.audit.as_object().cloned().unwrap_or_default();
            for key in ["tab", "origin", "path"] {
                if let Some(value) = worker.get(key) {
                    audit.entry(key).or_insert_with(|| value.clone());
                }
            }
            Ok((reply.result, worker))
        }
        Err(mut failure) => {
            let mut worker = Map::new();
            take_audit(&mut failure, &mut worker);
            for key in ["tab", "origin", "path"] {
                if let Some(value) = worker.get(key) {
                    audit.entry(key).or_insert_with(|| value.clone());
                }
            }
            Err(failure)
        }
    }
}

fn is_ref(target: &str) -> bool {
    target
        .strip_prefix('e')
        .is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 0))
}

fn usage(message: String) -> Failure {
    Failure::usage("bad_arguments", message)
}

/// The steps of an `act` request, checked for shape.
fn parse_steps(args: &Value) -> Result<Vec<Step>, Failure> {
    let steps: Vec<Step> = serde_json::from_value(args["steps"].clone())
        .map_err(|err| usage(format!("the steps are malformed: {err}")))?;
    if steps.is_empty() {
        return Err(usage("act needs a step".into()));
    }
    if steps.len() > MAX_STEPS {
        return Err(usage(format!(
            "a list holds at most {MAX_STEPS} steps; this one has {}",
            steps.len()
        )));
    }
    for (n, step) in steps.iter().enumerate() {
        let at = |message: &str| usage(format!("step {}: {message}", n + 1));
        operands(step).map_err(|message| at(&message))?;
        if let Some(field) = &step.expect.field {
            match field.split_once('=') {
                Some((target, _)) if is_ref(target) => {}
                _ => return Err(at("--expect-field takes eN=<value|checked|unchecked>")),
            }
        }
        if let Some(within) = step.expect.within
            && !(within > 0.0 && within <= MAX_WITHIN)
        {
            return Err(at(&format!(
                "--within is between 0 and {MAX_WITHIN} seconds"
            )));
        }
        if step
            .expect
            .sentence
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        {
            return Err(at("--expect needs a sentence"));
        }
    }
    Ok(steps)
}

/// What each action takes, so a malformed step is refused before the
/// first step of its list runs.
fn operands(step: &Step) -> Result<(), String> {
    let target = step.target.as_deref();
    let text = step.text.as_deref();
    if step.at.is_some() && (step.action != Action::Click || target.is_some()) {
        return Err("--at is for `click --at X,Y`, without a target".into());
    }
    if step.snapshot.is_some() && target.is_some_and(|t| !is_ref(t) && t != "page") {
        return Err(
            "--snapshot goes with a ref; a name is resolved on the page as it is now".into(),
        );
    }
    let named = |t: Option<&str>| t.is_some_and(|t| !t.trim().is_empty() && t != "page");
    match step.action {
        Action::Back if target.is_some() || text.is_some() => {
            Err("back takes no target or text".into())
        }
        Action::Back => Ok(()),
        Action::Click if !named(target) && step.at.is_none() => {
            Err("click needs a target, or --at X,Y".into())
        }
        Action::Click if text.is_some() => Err("click takes no text".into()),
        Action::Type | Action::Select if !named(target) => {
            Err(format!("{} needs a target", json!(step.action)))
        }
        Action::Type if text.is_none() => Err("type needs the text to type".into()),
        Action::Select if text.is_none_or(|t| t.is_empty()) => {
            Err("select needs the option to choose".into())
        }
        Action::Press if text.is_none_or(|t| t.is_empty()) => {
            Err("press needs a key, such as Enter".into())
        }
        Action::Scroll if !matches!(text, None | Some("up") | Some("down")) => {
            Err("scroll goes up or down".into())
        }
        _ => Ok(()),
    }
}

/// A request id the client made: short, plain characters.
fn request_id(args: &Value) -> Result<String, Failure> {
    let id = args["request"].as_str().unwrap_or_default();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(usage("the request needs an id from the client".into()));
    }
    Ok(id.to_string())
}

/// Refuses sentences before anything runs: all of them when there is no
/// model, and any that names a task rather than something a page shows.
async fn check_sentences(steps: &[Step], model: &mut Model) -> Result<(), Failure> {
    let sentences: Vec<(usize, String)> = steps
        .iter()
        .enumerate()
        .filter_map(|(n, s)| s.expect.sentence.clone().map(|text| (n, text)))
        .collect();
    if sentences.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = sentences.iter().map(|(_, s)| s.clone()).collect();
    let checkable = match model.checkable(&texts).await {
        Ok(checkable) => checkable,
        Err(_) => return Err(assist_off(model)),
    };
    for ((n, sentence), p) in sentences.iter().zip(checkable) {
        if p < CHECKABLE_P {
            let mut failure = Failure::usage(
                "vague_condition",
                format!(
                    "step {}: {sentence:?} names a task, not something a page shows; say what the page will show",
                    n + 1
                ),
            );
            failure.details = json!({"step": n + 1, "p": model::round(p)});
            return Err(failure);
        }
    }
    Ok(())
}

fn assist_off(model: &Model) -> Failure {
    Failure::failed(
        "assist_off",
        format!(
            "{}; use typed checks (--expect-text, --expect-url, --expect-field, --expect-download) instead of a sentence",
            model.why_off()
        ),
    )
}

/// `act`: one step, or a list of them.
pub async fn act(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let request = request_id(args)?;
    audit.insert("request".into(), json!(request));
    let list = args["list"] == true;
    let steps = parse_steps(args)?;
    audit.insert("steps".into(), json!(steps.len()));
    let mut model = browsers.model();
    let checked = check_sentences(&steps, &mut model).await;
    record_model(&model, audit);
    checked?;
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    entry
        .history
        .lock()
        .expect("history lock")
        .start(&request, steps.len())?;
    let mut session = entry.session.lock().await;
    audit.insert("engine".into(), session.ready["engine"].clone());
    let mut trace = Vec::new();
    let mut step_audits = Vec::new();
    let mut stop = None;
    for (n, step) in steps.iter().enumerate() {
        let mut runner = Runner {
            session: &mut session,
            model: &mut model,
            history: (&entry.history, &request),
            audit,
            record: Map::new(),
            trace: Map::new(),
        };
        runner.trace.insert("step".into(), json!(n + 1));
        runner.record.insert("step".into(), json!(n + 1));
        let last = n + 1 == steps.len();
        let outcome = runner.run(step, last).await;
        let (mut step_trace, mut record) = (runner.trace, runner.record);
        // A step that stopped before its action went out was not sent.
        for kept in [&mut step_trace, &mut record] {
            kept.entry("dispatch").or_insert_with(|| json!("not_sent"));
        }
        entry
            .history
            .lock()
            .expect("history lock")
            .step(&request, &step_trace);
        trace.push(Value::Object(step_trace));
        step_audits.push(Value::Object(record));
        if let Err(failure) = outcome {
            stop = Some(failure);
            break;
        }
    }
    let executed = trace
        .iter()
        .filter(|t| t["dispatch"] == "dispatched")
        .count();
    audit.insert("dispatched".into(), json!(executed));
    audit.insert("step_records".into(), json!(step_audits));
    record_model(&model, audit);
    entry.history.lock().expect("history lock").finish(
        &request,
        stop.as_ref()
            .map_or("done", |failure| failure.code.as_str()),
    );
    // Every answer ends with the page as it is now, which the agent then
    // has seen.
    let glance = call(&mut session, json!({"op": "glance"}), audit)
        .await
        .map(|(glance, _)| glance)
        .unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("request".into(), json!(request));
    if list {
        out.insert(
            "status".into(),
            json!(stop.as_ref().map_or("done", |f| f.code.as_str())),
        );
        out.insert("executed".into(), json!(executed));
        out.insert("trace".into(), json!(trace));
    } else if let Some(Value::Object(only)) = trace.first() {
        out.extend(only.clone());
        out.remove("step");
    }
    if let Some(glance) = glance.as_object() {
        for (key, value) in glance {
            out.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    match stop {
        None => Ok(Value::Object(out)),
        Some(mut failure) => {
            if let Some(details) = failure.details.as_object() {
                for (key, value) in details {
                    out.insert(key.clone(), value.clone());
                }
            }
            if list {
                out.insert("step".into(), json!(trace.len()));
            }
            failure.details = Value::Object(out);
            Err(failure)
        }
    }
}

/// The model's calls so far, for the audit: never their content.
fn record_model(model: &Model, audit: &mut Map<String, Value>) {
    if !model.calls.is_empty() {
        audit.insert("model_calls".into(), json!(model.calls));
    }
}

/// One step's run: its trace for the agent and history, its record for
/// the audit.
struct Runner<'a> {
    session: &'a mut Session,
    model: &'a mut Model,
    /// Where the step's trace goes as soon as its action is out, before
    /// any expectation is waited for.
    history: (&'a Mutex<History>, &'a str),
    audit: &'a mut Map<String, Value>,
    record: Map<String, Value>,
    trace: Map<String, Value>,
}

/// A step's target, resolved.
struct Target {
    snapshot: Option<String>,
    target: Option<String>,
}

impl Runner<'_> {
    async fn run(&mut self, step: &Step, last: bool) -> Result<(), Failure> {
        self.trace.insert("action".into(), json!(step.action));
        self.record.insert("action".into(), json!(step.action));
        if let Some(text) = &step.text
            && step.action == Action::Type
        {
            self.record
                .insert("text_length".into(), json!(text.chars().count()));
            self.record.insert(
                "text_sha256".into(),
                json!(to_hex(&Sha256::digest(text.as_bytes()))),
            );
        }
        let target = self.resolve(step).await?;
        let besides = if step.expect.download {
            let (listed, _) = call(self.session, json!({"op": "downloads"}), self.audit).await?;
            listed["downloads"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d["download"].as_str().map(str::to_string))
                .collect()
        } else {
            Vec::new()
        };
        let acted = self.dispatch(step, &target).await?;
        self.expect(step, besides).await?;
        if !last {
            match acted["wall"].as_str() {
                Some("login") => {
                    return Err(Failure::failed(
                        "needs_login",
                        "this step led to a sign-in form; the rest of the list did not run: sign in with `login`, then go on".into(),
                    ));
                }
                Some("captcha") => {
                    let mut failure = Failure::failed(
                        "needs_owner",
                        "this step led to a CAPTCHA; the rest of the list did not run: hand off to the owner".into(),
                    );
                    failure.details = json!({"reason": "captcha"});
                    return Err(failure);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The step's target as a ref: given as one, an exact name, or one
    /// model choice among the elements that fit the action.
    async fn resolve(&mut self, step: &Step) -> Result<Target, Failure> {
        let Some(named) = step
            .target
            .as_deref()
            .filter(|t| !is_ref(t) && *t != "page")
        else {
            if let Some(target) = &step.target {
                self.record.insert("target".into(), json!(target));
            }
            return Ok(Target {
                snapshot: step.snapshot.clone(),
                target: step.target.clone(),
            });
        };
        // The agent's own words, like typed text, are kept as a length.
        self.record
            .insert("description_length".into(), json!(named.chars().count()));
        let (found, _) = call(
            self.session,
            json!({"op": "resolve", "action": step.action, "name": named}),
            self.audit,
        )
        .await?;
        let snapshot = found["snapshot"].as_str().unwrap_or_default().to_string();
        let options = found["options"].as_array().cloned().unwrap_or_default();
        let exact: Vec<String> = found["exact"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_string))
            .collect();
        let option = |r: &str| {
            options
                .iter()
                .find(|o| o["ref"] == r)
                .cloned()
                .unwrap_or_default()
        };
        if exact.len() == 1 {
            return Ok(self.resolved(&snapshot, &option(&exact[0]), "name", None));
        }
        if self.model.is_on() {
            let choices = options
                .iter()
                .map(|o| {
                    (
                        o["ref"].as_str().unwrap_or_default().to_string(),
                        describe(o),
                    )
                })
                .collect();
            let page = json!({"title": found["title"]});
            let action = serde_json::to_value(step.action).unwrap_or_default();
            let ranked = self
                .model
                .choose_target(page, action.as_str().unwrap_or("click"), named, choices)
                .await;
            record_model(self.model, self.audit);
            if let Ok(ranked) = ranked {
                let (top, p) = ranked[0].clone();
                if top != "none" && p >= CHOOSE_P {
                    return Ok(self.resolved(&snapshot, &option(&top), "model", Some(p)));
                }
                let candidates: Vec<Value> = ranked
                    .iter()
                    .filter(|(r, p)| r != "none" && *p >= 0.02)
                    .take(3)
                    .map(|(r, p)| candidate(&snapshot, &option(r), Some(*p)))
                    .collect();
                let (code, message) = if top == "none" {
                    (
                        "no_such_target",
                        format!("nothing on this page matches {named:?}"),
                    )
                } else {
                    (
                        "ambiguous_target",
                        format!("not sure which element is {named:?}; act on a ref"),
                    )
                };
                return Err(unresolved(code, message, candidates));
            }
        }
        // No model: descriptions resolve by exact name only.
        let candidates: Vec<Value> = exact
            .iter()
            .take(3)
            .map(|r| candidate(&snapshot, &option(r), None))
            .collect();
        let off = if self.model.is_on() {
            String::new()
        } else {
            format!(" ({}, so names must match exactly)", self.model.why_off())
        };
        Err(if exact.is_empty() {
            unresolved(
                "no_such_target",
                format!(
                    "no element that fits {} is named {named:?}{off}",
                    json!(step.action)
                ),
                candidates,
            )
        } else {
            unresolved(
                "ambiguous_target",
                format!("{} elements are named {named:?}; act on a ref", exact.len()),
                candidates,
            )
        })
    }

    fn resolved(&mut self, snapshot: &str, option: &Value, how: &str, p: Option<f64>) -> Target {
        let target = option["ref"].as_str().unwrap_or_default().to_string();
        let shown = json!({"ref": target, "role": option["role"], "name": option["name"]});
        self.trace.insert("target".into(), shown);
        self.trace.insert("resolved_by".into(), json!(how));
        self.record.insert("resolved_by".into(), json!(how));
        self.record.insert("ref".into(), json!(target));
        self.record.insert("role".into(), option["role"].clone());
        if let Some(p) = p {
            self.trace.insert("p".into(), json!(model::round(p)));
            self.record.insert("p".into(), json!(model::round(p)));
        }
        Target {
            snapshot: Some(snapshot.to_string()),
            target: Some(target),
        }
    }

    /// Sends the action. A click on a page the agent hasn't seen comes
    /// back for review first: one that can't be undone stops the list
    /// with `confirm_target`, however its target was named.
    async fn dispatch(&mut self, step: &Step, target: &Target) -> Result<Value, Failure> {
        let mut act = json!({
            "op": "act", "action": step.action, "quiet": true,
            "snapshot": target.snapshot, "target": target.target,
            "text": step.text, "at": step.at.map(|(x, y)| [x, y]),
        });
        let mut reply = call(self.session, act.clone(), self.audit).await;
        if let Err(failure) = &reply
            && failure.code == "review_target"
        {
            let review = failure.details.clone();
            let element = &review["target"];
            let what = describe(element);
            let title = review["title"].as_str().unwrap_or_default();
            let risky = match self.model.is_on() {
                true => {
                    let judged = self.model.risky(title, &what).await;
                    record_model(self.model, self.audit);
                    judged
                        .ok()
                        .map(|p| (p >= RISKY_P, json!({"by": "model", "p": model::round(p)})))
                }
                false => None,
            };
            let (risky, by) = risky.unwrap_or_else(|| {
                let words = format!(
                    "{} {}",
                    element["name"].as_str().unwrap_or_default(),
                    element["row"].as_str().unwrap_or_default()
                );
                (model::risky_by_words(&words), json!({"by": "words"}))
            });
            self.record.insert("review".into(), by.clone());
            if risky {
                self.trace.insert("dispatch".into(), json!("not_sent"));
                let mut failure = Failure::new(
                    Exit::Denied,
                    "confirm_target",
                    format!(
                        "{what} looks like it can't be undone, on a page you haven't seen; look, then click it by ref"
                    ),
                );
                failure.details = json!({
                    "target": {"ref": element["ref"], "snapshot": element["snapshot"],
                               "role": element["role"], "name": element["name"]},
                    "review": by,
                });
                return Err(failure);
            }
            act["reviewed"] = json!(true);
            reply = call(self.session, act, self.audit).await;
        }
        match reply {
            Ok((result, worker)) => {
                self.trace.insert("dispatch".into(), json!("dispatched"));
                self.record.insert("dispatch".into(), json!("dispatched"));
                if !self.trace.contains_key("target") {
                    if let Some(shown) = result.get("target") {
                        self.trace.insert("target".into(), shown.clone());
                    }
                    if let Some(how) = result.get("resolved_by") {
                        self.trace.insert("resolved_by".into(), how.clone());
                        self.record.insert("resolved_by".into(), how.clone());
                    }
                    if let Some(role) = worker.get("role") {
                        self.record.insert("role".into(), role.clone());
                    }
                }
                for key in ["changed", "events", "blocked_by_dialog"] {
                    if let Some(value) = result.get(key) {
                        self.trace.insert(key.into(), value.clone());
                    }
                }
                let (history, request) = self.history;
                history
                    .lock()
                    .expect("history lock")
                    .step(request, &self.trace);
                Ok(result)
            }
            Err(failure) => {
                // A refusal is made before any input; anything else may
                // have reached the page.
                let refused = matches!(failure.exit, Exit::Usage | Exit::Denied)
                    || matches!(
                        failure.code.as_str(),
                        "stale_snapshot"
                            | "covered_target"
                            | "no_such_target"
                            | "dialog_open"
                            | "no_history"
                            | "session_paused"
                    );
                let dispatch = if refused { "not_sent" } else { "unknown" };
                self.trace.insert("dispatch".into(), json!(dispatch));
                self.record.insert("dispatch".into(), json!(dispatch));
                Err(failure)
            }
        }
    }

    /// The step's expectations, typed checks first, within one deadline.
    async fn expect(&mut self, step: &Step, besides: Vec<String>) -> Result<(), Failure> {
        let expect = &step.expect;
        let deadline = Instant::now() + expect.within();
        let mut typed = Vec::new();
        if let Some(url) = &expect.url {
            typed.push(("url", json!({"kind": "url", "url": url})));
        }
        if let Some(text) = &expect.text {
            typed.push(("text", json!({"kind": "text", "text": text})));
        }
        if let Some(field) = &expect.field {
            let (target_ref, value) = field.split_once('=').unwrap_or_default();
            // A ref the agent wrote is from a snapshot it saw: the step's
            // --snapshot, or the latest one shown.
            let snapshot = step.snapshot.clone();
            typed.push((
                "field",
                json!({"kind": "field", "snapshot": snapshot, "target": target_ref, "value": value}),
            ));
        }
        if expect.download {
            typed.push(("download", json!({"kind": "download", "besides": besides})));
        }
        let mut verdicts = Vec::new();
        let mut events = Vec::new();
        for (kind, condition) in typed {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            let (waited, _) = call(
                self.session,
                json!({"op": "wait", "condition": condition, "within_ms": left}),
                self.audit,
            )
            .await?;
            events.extend(waited["events"].as_array().cloned().unwrap_or_default());
            let met = waited["met"] == true;
            let mut verdict = json!({"kind": kind, "result": if met { "met" } else { "not_met" }});
            if let Some(download) = waited.get("download") {
                verdict["download"] = download.clone();
            }
            verdicts.push(verdict);
            self.note(&verdicts, &events);
            if waited["stale"] == true {
                let mut failure = Failure::failed(
                    "stale_snapshot",
                    "the --expect-field ref is from a page that has changed; name a field the agent has seen on this page".into(),
                );
                failure.details = json!({"expect": verdicts});
                return Err(failure);
            }
            if !met {
                return Err(not_met(kind, &verdicts));
            }
        }
        if let Some(sentence) = &expect.sentence {
            let verdict = loop {
                let (seen, _) = call(self.session, json!({"op": "lines"}), self.audit).await?;
                let mut lines: Vec<String> = Vec::new();
                for line in seen["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(seen["fields"].as_array().into_iter().flatten())
                {
                    lines.push(line.as_str().unwrap_or_default().to_string());
                }
                let judged = self
                    .model
                    .judge(seen["page"].clone(), sentence, &lines)
                    .await;
                record_model(self.model, self.audit);
                let Ok(verdict) = judged else {
                    verdicts.push(json!({"kind": "sentence", "result": "unjudged"}));
                    self.note(&verdicts, &events);
                    return Err(assist_off(self.model));
                };
                if verdict.result == "met" || Instant::now() + JUDGE_EVERY > deadline {
                    break verdict;
                }
                tokio::time::sleep(JUDGE_EVERY).await;
            };
            let mut shown = verdict.to_json();
            shown["kind"] = json!("sentence");
            verdicts.push(shown);
            self.note(&verdicts, &events);
            if verdict.result != "met" {
                let code = if verdict.result == "unsure" {
                    "expect_unsure"
                } else {
                    "expect_not_met"
                };
                let mut failure = Failure::failed(
                    code,
                    format!(
                        "after step {}: {sentence:?} is {}",
                        self.trace["step"],
                        verdict.result.replace('_', " ")
                    ),
                );
                failure.details = json!({"expect": verdicts});
                return Err(failure);
            }
        }
        Ok(())
    }

    /// Puts the verdicts so far, and events seen while waiting, in the
    /// trace; the audit keeps only each verdict's kind and result.
    fn note(&mut self, verdicts: &[Value], events: &[Value]) {
        self.trace.insert("expect".into(), json!(verdicts));
        if !events.is_empty() {
            let mut all = self.trace["events"].as_array().cloned().unwrap_or_default();
            all.extend(events.iter().cloned());
            self.trace.insert("events".into(), json!(all));
        }
        let kept: Vec<Value> = verdicts
            .iter()
            .map(|v| json!({"kind": v["kind"], "result": v["result"]}))
            .collect();
        self.record.insert("expect".into(), json!(kept));
    }
}

fn not_met(kind: &str, verdicts: &[Value]) -> Failure {
    let mut failure = Failure::failed(
        "expect_not_met",
        format!("the {kind} expectation was not met in time"),
    );
    failure.details = json!({"expect": verdicts});
    failure
}

/// An element as one line: `button "Download" — in: October 2026 $412.00`.
fn describe(element: &Value) -> String {
    let mut line = format!(
        "{} {:?}",
        element["role"].as_str().unwrap_or("element"),
        element["name"].as_str().unwrap_or_default()
    );
    if let Some(value) = element["value"].as_str() {
        line.push_str(&format!(", value {value:?}"));
    }
    if let Some(checked) = element["checked"].as_bool() {
        line.push_str(if checked {
            ", checked"
        } else {
            ", not checked"
        });
    }
    if let Some(row) = element["row"].as_str() {
        line.push_str(&format!(" — in: {row}"));
    }
    line
}

fn candidate(snapshot: &str, option: &Value, p: Option<f64>) -> Value {
    let mut out = json!({"ref": option["ref"], "snapshot": snapshot,
                         "role": option["role"], "name": option["name"]});
    if let Some(row) = option.get("row") {
        out["row"] = row.clone();
    }
    if let Some(p) = p {
        out["p"] = json!(model::round(p));
    }
    out
}

fn unresolved(code: &str, message: String, candidates: Vec<Value>) -> Failure {
    let mut failure = Failure::failed(code, message);
    failure.details = json!({"candidates": candidates});
    failure
}

/// `find`: candidate refs for a role and exact name, or for a description.
/// It never acts.
pub async fn find(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let role = args["role"].as_str();
    let name = args["name"].as_str();
    let description = args["description"].as_str();
    let action = args["action"].as_str().unwrap_or("click");
    let mut session = entry.session.lock().await;
    audit.insert("engine".into(), session.ready["engine"].clone());
    let ask = match (description, role, name) {
        (Some(description), None, None) => {
            audit.insert(
                "description_length".into(),
                json!(description.chars().count()),
            );
            json!({"op": "resolve", "action": action, "name": description})
        }
        (None, Some(role), Some(name)) => {
            audit.insert("role".into(), json!(role));
            json!({"op": "resolve", "role": role, "name": name})
        }
        _ => {
            return Err(usage(
                "find takes a description, or --role R --name N".into(),
            ));
        }
    };
    let (found, _) = call(&mut session, ask, audit).await?;
    let snapshot = found["snapshot"].as_str().unwrap_or_default().to_string();
    let options = found["options"].as_array().cloned().unwrap_or_default();
    let option = |r: &str| {
        options
            .iter()
            .find(|o| o["ref"] == r)
            .cloned()
            .unwrap_or_default()
    };
    let exact: Vec<String> = found["exact"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_string))
        .collect();
    let mut out = json!({"snapshot": snapshot, "url": found["url"], "title": found["title"]});
    let named = |out: &mut Value| {
        out["resolved_by"] = json!("name");
        out["candidates"] = json!(
            exact
                .iter()
                .map(|r| candidate(&snapshot, &option(r), None))
                .collect::<Vec<_>>()
        );
    };
    let Some(description) = description else {
        named(&mut out);
        return Ok(out);
    };
    if exact.len() == 1 {
        named(&mut out);
        return Ok(out);
    }
    let mut model = browsers.model();
    if !model.is_on() {
        named(&mut out);
        out["assist"] = json!("off");
        return Ok(out);
    }
    let choices = options
        .iter()
        .map(|o| {
            (
                o["ref"].as_str().unwrap_or_default().to_string(),
                describe(o),
            )
        })
        .collect();
    let page = json!({"title": found["title"]});
    let ranked = model
        .choose_target(page, action, description, choices)
        .await;
    record_model(&model, audit);
    match ranked {
        Ok(ranked) => {
            out["resolved_by"] = json!("model");
            out["candidates"] = json!(
                ranked
                    .iter()
                    .filter(|(r, p)| r != "none" && *p >= 0.02)
                    .take(3)
                    .map(|(r, p)| candidate(&snapshot, &option(r), Some(*p)))
                    .collect::<Vec<_>>()
            );
        }
        Err(_) => {
            named(&mut out);
            out["assist"] = json!("off");
        }
    }
    Ok(out)
}

/// `wait`: for an element's state, for text, or for a sentence, with a
/// deadline. Not met in time is `timeout`.
pub async fn wait(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let within = args["within"].as_f64().unwrap_or(WITHIN);
    if !(within > 0.0 && within <= MAX_WITHIN) {
        return Err(usage(format!(
            "--within is between 0 and {MAX_WITHIN} seconds"
        )));
    }
    let condition = if let Some(target) = args["ref"].as_str() {
        json!({"kind": "ref", "snapshot": args["snapshot"], "target": target,
               "state": args["state"].as_str().unwrap_or("visible")})
    } else if let Some(text) = args["text"].as_str() {
        json!({"kind": "text", "text": text})
    } else if let Some(url) = args["url"].as_str() {
        json!({"kind": "url", "url": url})
    } else {
        Value::Null
    };
    let sentence = args["for"].as_str();
    let mut model = browsers.model();
    if let Some(sentence) = sentence {
        audit.insert("kind".into(), json!("sentence"));
        let step = Step {
            snapshot: None,
            action: Action::Scroll,
            target: None,
            text: None,
            at: None,
            expect: Expect {
                sentence: Some(sentence.to_string()),
                ..Expect::default()
            },
        };
        let checked = check_sentences(std::slice::from_ref(&step), &mut model).await;
        record_model(&model, audit);
        checked?;
    } else if condition.is_null() {
        return Err(usage(
            "wait takes --ref eN --state S, --text T, --url U or --for \"<sentence>\"".into(),
        ));
    } else {
        audit.insert("kind".into(), condition["kind"].clone());
    }
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let mut session = entry.session.lock().await;
    audit.insert("engine".into(), session.ready["engine"].clone());
    let deadline = Instant::now() + Duration::from_secs_f64(within);
    let out = if let Some(sentence) = sentence {
        let verdict = loop {
            let (seen, _) = call(&mut session, json!({"op": "lines"}), audit).await?;
            let lines: Vec<String> = seen["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(seen["fields"].as_array().into_iter().flatten())
                .map(|l| l.as_str().unwrap_or_default().to_string())
                .collect();
            let judged = model.judge(seen["page"].clone(), sentence, &lines).await;
            record_model(&model, audit);
            let verdict = judged.map_err(|_| assist_off(&model))?;
            if verdict.result == "met" || Instant::now() + JUDGE_EVERY > deadline {
                break verdict;
            }
            tokio::time::sleep(JUDGE_EVERY).await;
        };
        audit.insert("verdict".into(), json!(verdict.result));
        json!({"met": verdict.result == "met", "verdict": verdict.to_json()})
    } else {
        let ms = (within * 1000.0) as u64;
        let (waited, _) = call(
            &mut session,
            json!({"op": "wait", "condition": condition, "within_ms": ms}),
            audit,
        )
        .await?;
        audit.insert(
            "verdict".into(),
            json!(if waited["met"] == true {
                "met"
            } else {
                "not_met"
            }),
        );
        waited
    };
    let glance = call(&mut session, json!({"op": "glance"}), audit)
        .await
        .map(|(g, _)| g)
        .unwrap_or(Value::Null);
    let mut out = out;
    if let (Some(out), Some(glance)) = (out.as_object_mut(), glance.as_object()) {
        for (key, value) in glance {
            out.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    if out["met"] != true {
        let mut failure = Failure::failed(
            "timeout",
            format!("the condition did not hold within {within}s"),
        );
        failure.details = out;
        return Err(failure);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_checked_whole_before_anything_runs() {
        let step = json!({"action": "click", "target": "e1"});
        assert!(parse_steps(&json!({"steps": [step]})).is_ok());
        assert_eq!(
            parse_steps(&json!({"steps": []})).unwrap_err().exit,
            Exit::Usage
        );
        let eleven: Vec<Value> = (0..11).map(|_| step.clone()).collect();
        assert!(
            parse_steps(&json!({"steps": eleven}))
                .unwrap_err()
                .message
                .contains("at most 10")
        );
        let bad = json!({"action": "click", "target": "e1", "expect": {"field": "amount=3"}});
        assert!(parse_steps(&json!({"steps": [step, bad]})).is_err());
        let hover = json!({"action": "hover", "target": "e1"});
        assert!(parse_steps(&json!({"steps": [hover]})).is_err());
        // Every step's operands, before step 1 runs.
        for (bad, why) in [
            (json!({"action": "type", "target": "e1"}), "text to type"),
            (json!({"action": "type", "text": "x"}), "needs a target"),
            (json!({"action": "select", "target": "e1"}), "option"),
            (json!({"action": "press"}), "key"),
            (json!({"action": "scroll", "text": "left"}), "up or down"),
            (json!({"action": "back", "target": "e1"}), "no target"),
            (json!({"action": "click"}), "--at"),
            (
                json!({"action": "click", "target": "e1", "text": "x"}),
                "no text",
            ),
            (
                json!({"action": "type", "target": "e1", "text": "x", "at": [1, 2]}),
                "--at",
            ),
            (
                json!({"action": "click", "target": "Save", "snapshot": "o1"}),
                "--snapshot",
            ),
        ] {
            let message = parse_steps(&json!({"steps": [step, bad]}))
                .unwrap_err()
                .message;
            assert!(
                message.starts_with("step 2:") && message.contains(why),
                "{message}"
            );
        }
        for good in [
            json!({"action": "click", "at": [1, 2]}),
            json!({"action": "press", "text": "Enter"}),
            json!({"action": "scroll"}),
            json!({"action": "back"}),
            json!({"action": "type", "target": "amount", "text": ""}),
            json!({"action": "scroll", "snapshot": "o1", "expect": {"field": "e1=x"}}),
        ] {
            assert!(
                parse_steps(&json!({"steps": [good.clone()]})).is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn request_ids_are_the_clients_and_never_reused_in_a_session() {
        assert!(request_id(&json!({})).is_err());
        assert!(request_id(&json!({"request": "r 1"})).is_err());
        assert_eq!(request_id(&json!({"request": "r1a2"})).unwrap(), "r1a2");
        let mut history = History::default();
        history.start("r1", 2).unwrap();
        assert_eq!(
            history.start("r1", 1).unwrap_err().code,
            "duplicate_request"
        );
        let mut step = Map::new();
        step.insert("step".into(), json!(1));
        step.insert("dispatch".into(), json!("dispatched"));
        history.step("r1", &step);
        let kept = &history.requests["r1"];
        assert_eq!(kept["status"], "running");
        assert_eq!(kept["trace"][0]["dispatch"], "dispatched");
        assert_eq!(kept["trace"][1]["dispatch"], "not_sent");
        history.finish("r1", "done");
        assert_eq!(history.requests["r1"]["status"], "done");
    }

    #[test]
    fn an_element_reads_as_one_line_with_its_row() {
        let element =
            json!({"role": "button", "name": "Download", "row": "September 2026 $388.50 Paid"});
        assert_eq!(
            describe(&element),
            "button \"Download\" — in: September 2026 $388.50 Paid"
        );
    }
}
