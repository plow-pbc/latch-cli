//! The host's step executor: the plan command (`do`), `act` (a plan of one
//! literal step), confirmations, `find` and `wait`.
//!
//! **Before anything is sent** the whole plan is checked: its shape, every
//! ref pinned to the snapshot it is read in, every sentence check (refused
//! as `vague_condition`, or `model_unavailable` with no model), and the
//! host's step policy (`step_not_allowed`).
//!
//! **Each literal step** then runs in order: a click, or an Enter in a
//! field, first gets the decision model's risk decision on exactly what it
//! would act on; one that can't be undone stops with `confirm_needed` and a
//! confirmation instead of being sent, and with no model it stops with
//! `model_unavailable`. A fill that releases a card or identity secret
//! always stops for a confirmation. Every dispatch is journaled before it is
//! sent and once its result is known (`journal.rs`). The step's check then
//! decides its outcome: `checked` or `model_assessed`, or `dispatched` when
//! it has none. The plan stops at the first step that does not pass, and
//! nothing is ever replayed.
//!
//! **A confirmation** is used once, in the session and document it was
//! issued in, and only while its element and the lines around it are
//! unchanged: otherwise `confirm_stale`. It is carried out by
//! `do --confirm C`, or by `act click` on its own ref, and never needs a new
//! risk decision.
//!
//! The decision model (`model.rs`) runs here, never in the worker, and the
//! worker is not held while it answers.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::core::{Action, fingerprint};
use super::host::{Browsers, Entry, paused, take_audit};
use super::journal::{self, Journal};
use super::model::{self, CHECKABLE_P, Model, RISKY_P};
use super::plan::{Check, CheckKind, Literal, Op, Plan};
use crate::failure::{Exit, Failure};
use crate::state::to_hex;

/// How long a step's check may wait after its action.
const CHECK_WITHIN: Duration = Duration::from_secs(10);
/// How often a sentence is judged again while its check waits.
const JUDGE_EVERY: Duration = Duration::from_millis(1000);
/// `wait`'s default and largest deadline, in seconds.
const WAIT_WITHIN: f64 = 10.0;
const MAX_WAIT: f64 = 120.0;
/// Where the host's step policy is set, in the state directory.
pub const POLICY_FILE: &str = "browser/policy.json";

/// An issued confirmation: what it lets through, and what it is bound to.
pub struct Confirmation {
    id: String,
    kind: Confirms,
    snapshot: String,
    target: String,
    document: String,
    uid: Value,
    /// The reviewed element, what it acts on, the page title and the lines
    /// around it, hashed: any change makes the confirmation stale.
    fingerprint: String,
    used: bool,
}

enum Confirms {
    Click,
    /// An Enter in a field: `press` Enter, or `type` text with a line
    /// break; the op and its text as written.
    Keys(Op, String),
    /// A fill releasing a secret field, with its arguments.
    Fill(Value),
}

/// Which literal steps a session may run (a host setting).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Policy {
    /// Refs from any page latch returned to the session.
    Any,
    /// Only refs returned in the session's latest stop.
    Stop,
    /// None.
    None,
}

impl Policy {
    fn name(self) -> &'static str {
        match self {
            Policy::Any => "any",
            Policy::Stop => "stop",
            Policy::None => "none",
        }
    }
}

/// The host's step policy: `{"literal_steps": "any" | "stop" | "none"}` in
/// `browser/policy.json`, `any` without one.
fn policy(state_dir: &Path) -> Result<Policy, Failure> {
    let bad = |why: String| Failure::failed("bad_policy", format!("{POLICY_FILE}: {why}"));
    let text = match std::fs::read_to_string(state_dir.join(POLICY_FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::Any),
        Err(err) => return Err(bad(err.to_string())),
    };
    let set: Value = serde_json::from_str(&text).map_err(|err| bad(err.to_string()))?;
    match set["literal_steps"].as_str() {
        None | Some("any") => Ok(Policy::Any),
        Some("stop") => Ok(Policy::Stop),
        Some("none") => Ok(Policy::None),
        Some(other) => Err(bad(format!(
            "literal_steps is any, stop or none, not {other:?}"
        ))),
    }
}

/// One worker call; its audit fills in what the request's audit lacks
/// (tab, origin, path), and is returned for the action's own record. The
/// session is held only for the call.
async fn call(
    entry: &Entry,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Map<String, Value>), Failure> {
    let reply = entry.session.lock().await.call(call).await;
    let (outcome, worker) = match reply {
        Ok(reply) => {
            let worker = reply.audit.as_object().cloned().unwrap_or_default();
            (Ok(reply.result), worker)
        }
        Err(mut failure) => {
            let mut worker = Map::new();
            take_audit(&mut failure, &mut worker);
            (Err(failure), worker)
        }
    };
    for key in ["tab", "origin", "path"] {
        if let Some(value) = worker.get(key) {
            audit.entry(key).or_insert_with(|| value.clone());
        }
    }
    outcome.map(|result| (result, worker))
}

fn usage(message: String) -> Failure {
    Failure::usage("bad_arguments", message)
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

fn model_unavailable(model: &Model, what: &str) -> Failure {
    Failure::failed(
        "model_unavailable",
        format!(
            "{what} needs the decision model, and {}; nothing was sent",
            model.why_off()
        ),
    )
}

/// Refuses sentence checks before anything runs: all of them when there is
/// no model, and any that names a task rather than something a page shows.
async fn check_sentences(sentences: &[(usize, String)], model: &mut Model) -> Result<(), Failure> {
    if sentences.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = sentences.iter().map(|(_, s)| s.clone()).collect();
    let checkable = model
        .checkable(&texts)
        .await
        .map_err(|_| model_unavailable(model, "a sentence check"))?;
    for ((n, sentence), p) in sentences.iter().zip(checkable) {
        if p < CHECKABLE_P {
            let mut failure = Failure::usage(
                "vague_condition",
                format!(
                    "step {n}: {sentence:?} names a task, not something a page shows; say what the page will show, or use a typed check"
                ),
            );
            failure.details = json!({"step": n, "p": model::round(p)});
            return Err(failure);
        }
    }
    Ok(())
}

/// The model's calls so far, for the audit: never their content.
fn record_model(model: &Model, audit: &mut Map<String, Value>) {
    if !model.calls.is_empty() {
        audit.insert("model_calls".into(), json!(model.calls));
    }
}

/// One step of a run: a confirmation to carry out, or a literal action.
enum Work {
    /// Confirmation C, on the ref `act` named (with the act's check), or
    /// its own.
    Confirm(String, Option<(String, String)>, Option<Check>),
    Literal(Literal, Option<Check>),
}

/// `do` and `act`: a plan's literal steps, or a confirmation and the steps
/// after it.
pub async fn run(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let request = request_id(args)?;
    audit.insert("request".into(), json!(request));
    let via_act = args["via"] == "act";
    audit.insert("via".into(), json!(if via_act { "act" } else { "do" }));
    let plan: Plan = serde_json::from_value(args["plan"].clone())
        .map_err(|err| usage(format!("the plan is malformed: {err}")))?;
    plan.validate().map_err(usage)?;
    let confirm = args["confirm"].as_str().map(str::to_string);
    if plan.steps.is_empty() && confirm.is_none() {
        return Err(usage("a plan needs a step".into()));
    }
    audit.insert("steps".into(), json!(plan.steps.len()));
    let first = usize::from(confirm.is_some()) + 1;
    if let Some(n) = plan.steps.iter().position(|s| s.goal.is_some()) {
        let mut failure = Failure::new(
            Exit::Denied,
            "step_not_allowed",
            format!(
                "step {}: this host runs literal steps only; instruction steps (a goal in words) are not available on it yet",
                n + first
            ),
        );
        failure.details = json!({"step": n + first});
        return Err(failure);
    }
    let mut model = browsers.model();
    let sentences: Vec<(usize, String)> = plan
        .steps
        .iter()
        .enumerate()
        .filter_map(|(n, s)| {
            s.check
                .as_ref()
                .filter(|c| c.kind == CheckKind::Sentence)
                .map(|c| (n + first, c.value.clone()))
        })
        .collect();
    let checked = check_sentences(&sentences, &mut model).await;
    record_model(&model, audit);
    checked?;
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let _serial = entry.serial.lock().await;
    audit.insert(
        "engine".into(),
        entry.session.lock().await.ready["engine"].clone(),
    );

    // Every ref pinned to the snapshot it is read in, before anything runs.
    let mut work = Vec::new();
    if let Some(id) = &confirm {
        work.push(Work::Confirm(id.clone(), None, None));
    }
    for (n, step) in plan.steps.iter().enumerate() {
        let mut literal = step.literal.clone().expect("goals were refused");
        let at = |failure: Failure| Failure {
            message: format!("step {}: {}", n + first, failure.message),
            ..failure
        };
        let mut refs: Vec<String> = literal.target.iter().cloned().collect();
        refs.extend(literal.fields.values().flatten().cloned());
        refs.extend(literal.press.iter().cloned());
        for target in refs {
            let (pinned, _) = call(
                entry,
                json!({"op": "pin", "snapshot": literal.snapshot, "target": target}),
                audit,
            )
            .await
            .map_err(at)?;
            literal.snapshot = pinned["snapshot"].as_str().map(str::to_string);
        }
        work.push(Work::Literal(literal, step.check.clone()));
    }

    // `act click` on a confirmation's own ref carries it out.
    let issued = !entry
        .confirmations
        .lock()
        .expect("confirmations lock")
        .is_empty();
    if via_act
        && issued
        && let [Work::Literal(literal, check)] = work.as_slice()
        && literal.op == Op::Click
    {
        let check = check.clone();
        let target = literal.target.clone().unwrap_or_default();
        let snapshot = literal.snapshot.clone().unwrap_or_default();
        let (reviewed, _) = call(
            entry,
            json!({"op": "review", "snapshot": snapshot, "target": target}),
            audit,
        )
        .await?;
        let found = entry
            .confirmations
            .lock()
            .expect("confirmations lock")
            .iter()
            .rev()
            .find(|c| {
                matches!(c.kind, Confirms::Click)
                    && c.document == reviewed["document"]
                    && c.uid == reviewed["uid"]
            })
            .map(|c| c.id.clone());
        if let Some(id) = found {
            work = vec![Work::Confirm(id, Some((snapshot, target)), check)];
        }
    }

    // The step policy, before anything is sent. Confirmations always pass.
    let policy = policy(&browsers.state_dir())?;
    audit.insert("policy".into(), json!(policy.name()));
    let last_stop = entry.last_stop.lock().expect("last stop lock").clone();
    for (n, step) in work.iter().enumerate() {
        let Work::Literal(literal, _) = step else {
            continue;
        };
        let allowed = match policy {
            Policy::Any => true,
            Policy::None => false,
            Policy::Stop => {
                literal.target.is_none() && literal.op != Op::Fill
                    || literal.snapshot.is_some() && literal.snapshot == last_stop
            }
        };
        if !allowed {
            audit.insert(
                "step_not_allowed".into(),
                json!({"step": n + 1, "policy": policy.name()}),
            );
            let why = match policy {
                Policy::None => "this host allows no literal steps".to_string(),
                _ => "this host allows literal steps only on refs from the latest stop".to_string(),
            };
            let mut failure = Failure::new(
                Exit::Denied,
                "step_not_allowed",
                format!("step {}: {why}; nothing was sent", n + 1),
            );
            failure.details = json!({"step": n + 1, "policy": policy.name()});
            return Err(failure);
        }
    }

    let summaries: Vec<Value> = work
        .iter()
        .enumerate()
        .map(|(n, step)| match step {
            Work::Confirm(id, ..) => json!({"step": n + 1, "confirm": id}),
            Work::Literal(literal, check) => {
                let mut summary = json!({"step": n + 1, "op": literal.op, "ref": literal.target});
                if let Some(check) = check {
                    summary["check"] = json!(check.kind);
                }
                summary
            }
        })
        .collect();
    let journal = Journal::start(
        &browsers.state_dir(),
        session,
        caller,
        &request,
        json!(summaries),
    )?;
    let running = format!("{session}/{request}");
    browsers.running(&running, true);
    let mut run = Run {
        browsers,
        entry,
        model,
        journal,
        audit,
        deadline: Instant::now() + Duration::from_secs_f64(plan.within()),
        steps: Vec::new(),
        trail: Vec::new(),
        records: Vec::new(),
        actions: 0,
    };
    let mut stop = None;
    for (n, step) in work.into_iter().enumerate() {
        let step_no = n + 1;
        let mut shown = json!({"step": step_no, "actions": 0});
        let before = run.actions;
        let outcome = match step {
            Work::Confirm(id, act, check) => {
                shown["confirm"] = json!(id);
                run.confirmed(step_no, &id, act, check.as_ref()).await
            }
            Work::Literal(literal, check) => {
                shown["op"] = json!(literal.op);
                run.literal(step_no, &literal, check.as_ref(), n + 1 == summaries.len())
                    .await
            }
        };
        shown["actions"] = json!(run.actions - before);
        match outcome {
            Ok(Done { outcome, evidence }) => {
                shown["outcome"] = json!(outcome);
                if let Some(evidence) = evidence {
                    shown["evidence"] = evidence;
                }
                run.note_step(&shown)?;
                run.steps.push(shown);
            }
            Err(failure) => {
                shown["outcome"] = json!("stopped");
                shown["code"] = json!(failure.code);
                if let Some(evidence) = failure.details.get("evidence") {
                    shown["evidence"] = evidence.clone();
                }
                run.note_step(&shown)?;
                run.steps.push(shown);
                stop = Some((step_no, failure));
                break;
            }
        }
    }
    for summary in summaries.iter().skip(run.steps.len()) {
        let mut shown = summary.clone();
        shown["outcome"] = json!("not_run");
        run.steps.push(shown);
    }
    let status = match &stop {
        Some(_) => "stopped",
        None if run
            .steps
            .iter()
            .all(|s| s["outcome"] == "checked" || s["outcome"] == "model_assessed") =>
        {
            "reached"
        }
        None => "dispatched",
    };
    let mut end = json!({"t": "end", "status": status});
    if let Some((step, failure)) = &stop {
        end["stop"] = json!({"step": step, "code": failure.code});
    }
    let journaled = run.journal.write(end);
    browsers.running(&running, false);
    journaled?;
    let Run {
        model,
        steps,
        trail,
        records,
        audit,
        ..
    } = run;
    record_model(&model, audit);
    audit.insert("status".into(), json!(status));
    audit.insert("step_records".into(), json!(records));
    // Every answer ends with the page as it is now.
    let page = call(entry, json!({"op": "glance"}), audit)
        .await
        .map(|(page, _)| page)
        .unwrap_or(Value::Null);
    let mut out =
        json!({"request": request, "status": status, "steps": steps, "trail": trail, "page": page});
    if steps
        .last()
        .is_some_and(|s| s["outcome"] == "model_assessed")
    {
        out["review"] = json!(true);
    }
    let Some((step, mut failure)) = stop else {
        return Ok(out);
    };
    audit.insert("stop".into(), json!({"step": step, "code": failure.code}));
    *entry.last_stop.lock().expect("last stop lock") =
        page["snapshot"].as_str().map(str::to_string);
    let mut shown = json!({"step": step, "code": failure.code, "message": failure.message});
    for (key, value) in failure.details.as_object().into_iter().flatten() {
        shown[key] = value.clone();
    }
    if failure.code == "confirm_needed"
        && let Some(handle) = keep_shot(browsers, entry, session, &request, audit).await
    {
        shown["confirmation"]["screenshot"] = handle.clone();
        out["screenshot"] = handle;
    }
    out["stop"] = shown;
    failure.details = out;
    Err(failure)
}

/// A screenshot of the page now, kept with request `request` as its
/// `final` shot; its handle, which `do --history R --shot final` fetches,
/// or none if the page could not be shot.
async fn keep_shot(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    request: &str,
    audit: &mut Map<String, Value>,
) -> Option<Value> {
    let reply = entry
        .session
        .lock()
        .await
        .call(json!({"op": "screenshot"}))
        .await
        .ok()?;
    let png = reply.bytes?;
    journal::save_shot(&browsers.state_dir(), session, request, "final", &png).ok()?;
    audit.insert(
        "screenshot".into(),
        json!({"shot": "final", "size": png.len(), "sha256": to_hex(&Sha256::digest(&png))}),
    );
    Some(json!({"request": request, "shot": "final"}))
}

/// A standalone `fill` that releases a secret field: journaled as a request
/// of one step that stopped for a confirmation, with a screenshot of the
/// page.
pub async fn stop_fill(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Failure {
    let request = match request_id(args) {
        Ok(request) => request,
        Err(failure) => return failure,
    };
    audit.insert("request".into(), json!(request));
    let started = Journal::start(
        &browsers.state_dir(),
        session,
        caller,
        &request,
        json!([{"step": 1, "op": "fill"}]),
    );
    let mut journal = match started {
        Ok(journal) => journal,
        Err(failure) => return failure,
    };
    let mut failure = confirm_fill(entry, args, audit).await;
    let stop = json!({"step": 1, "code": failure.code});
    let journaled = journal
        .write(json!({"t": "step", "step": 1, "op": "fill", "outcome": "stopped", "code": failure.code}))
        .and_then(|()| journal.write(json!({"t": "end", "status": "stopped", "stop": stop})));
    if let Err(lost) = journaled {
        return lost;
    }
    if failure.code == "confirm_needed"
        && let Some(handle) = keep_shot(browsers, entry, session, &request, audit).await
    {
        failure.details["confirmation"]["screenshot"] = handle.clone();
        failure.details["screenshot"] = handle;
    }
    failure.details["request"] = json!(request);
    failure
}

/// A step that passed: its outcome, and the evidence for it.
struct Done {
    outcome: &'static str,
    evidence: Option<Value>,
}

struct Run<'a> {
    browsers: &'a Browsers,
    entry: &'a Entry,
    model: Model,
    journal: Journal,
    audit: &'a mut Map<String, Value>,
    deadline: Instant,
    steps: Vec<Value>,
    trail: Vec<Value>,
    /// The audit's record of each action.
    records: Vec<Value>,
    actions: usize,
}

fn stopped(code: &str, message: String) -> Failure {
    Failure::failed(code, message)
}

impl Run<'_> {
    fn note_step(&mut self, shown: &Value) -> Result<(), Failure> {
        let mut record = shown.clone();
        record["t"] = json!("step");
        // The journal keeps what the answer shows; the audit, kinds only.
        self.journal.write(record)
    }

    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    async fn literal(
        &mut self,
        n: usize,
        literal: &Literal,
        check: Option<&Check>,
        last: bool,
    ) -> Result<Done, Failure> {
        if self.left().is_zero() {
            return Err(stopped(
                "deadline",
                format!("the plan's time ran out before step {n}"),
            ));
        }
        if let Some(check) = check
            && check.require_change
            && self
                .holds(check, literal, Vec::new(), Duration::ZERO)
                .await?
        {
            return Err(stopped(
                "check_not_met",
                format!(
                    "step {n}'s check already holds before the step, and it must change; nothing was sent"
                ),
            ));
        }
        let besides = match check {
            Some(c) if c.kind == CheckKind::Download => self.downloads().await?,
            _ => Vec::new(),
        };
        let acted = match literal.op {
            Op::Fill => {
                self.fill(n, literal).await?;
                Value::Null
            }
            Op::Click => self.risky(n, literal, false).await?,
            _ if presses_enter(literal) => self.risky(n, literal, true).await?,
            _ => self.dispatch(n, literal, None, None, None).await?,
        };
        let done = self.check(n, literal, check, besides).await?;
        if !last {
            wall(&acted)?;
        }
        Ok(done)
    }

    /// A click or Enter: the model's risk decision on what it would act on,
    /// then the action, made only if that is still what the page shows, or a
    /// confirmation in its place.
    async fn risky(&mut self, n: usize, literal: &Literal, enter: bool) -> Result<Value, Failure> {
        let reviewed = self.review(literal, enter).await?;
        if !self.model.is_on() {
            return Err(model_unavailable(&self.model, "a click or Enter"));
        }
        let what = action_line(&reviewed, enter, literal.op == Op::Type);
        let lines = strings(&reviewed["lines"]);
        let judged = self
            .model
            .risky(
                reviewed["title"].as_str().unwrap_or_default(),
                &what,
                &lines,
            )
            .await;
        record_model(&self.model, self.audit);
        let p = judged.map_err(|_| model_unavailable(&self.model, "a click or Enter"))?;
        if p < RISKY_P {
            let guard = fingerprint(&reviewed);
            return match self
                .dispatch(n, literal, Some(p), None, Some((&guard, enter)))
                .await
            {
                Err(failure) if failure.code == "context_changed" => Err(stopped(
                    "stale_snapshot",
                    format!(
                        "{what} was judged on a page that changed while latch judged it; nothing was sent. Look at the page again"
                    ),
                )),
                acted => acted,
            };
        }
        let is_click = !enter;
        let kind = match enter {
            true => Confirms::Keys(literal.op, literal.text.clone().unwrap_or_default()),
            false => Confirms::Click,
        };
        let id = self.issue(kind, &reviewed);
        self.records.push(json!({"step": n, "op": literal.op, "ref": literal.target,
                                 "risk_p": model::round(p), "confirmation": id, "dispatch": "refused"}));
        let mut failure = Failure::new(
            Exit::Denied,
            "confirm_needed",
            format!(
                "{what} looks like it can't be undone; nothing was sent. If it is what the task needs, confirm it with `latch browser do --confirm {id}`{}",
                if is_click {
                    format!(
                        " or `latch browser act click {}`",
                        literal.target.as_deref().unwrap_or_default()
                    )
                } else {
                    String::new()
                }
            ),
        );
        let mut shown = reviewed_json(&id, &reviewed);
        shown["op"] = json!(literal.op);
        shown["risk_p"] = json!(model::round(p));
        failure.details = json!({"confirmation": shown});
        Err(failure)
    }

    async fn review(&mut self, literal: &Literal, enter: bool) -> Result<Value, Failure> {
        let (reviewed, _) = call(
            self.entry,
            json!({"op": "review", "snapshot": literal.snapshot, "target": literal.target, "enter": enter}),
            self.audit,
        )
        .await?;
        Ok(reviewed)
    }

    /// A new confirmation, bound to what `reviewed` shows.
    fn issue(&mut self, kind: Confirms, reviewed: &Value) -> String {
        let id = format!("c{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff);
        let confirmation = Confirmation {
            id: id.clone(),
            kind,
            snapshot: reviewed["snapshot"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            target: reviewed["ref"].as_str().unwrap_or_default().to_string(),
            document: reviewed["document"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            uid: reviewed["uid"].clone(),
            fingerprint: fingerprint(reviewed),
            used: false,
        };
        self.entry
            .confirmations
            .lock()
            .expect("confirmations lock")
            .push(confirmation);
        id
    }

    /// Carries out confirmation `id`: once, on the element and context it
    /// was issued for, with no new risk decision; then `check`, if any.
    async fn confirmed(
        &mut self,
        n: usize,
        id: &str,
        act: Option<(String, String)>,
        check: Option<&Check>,
    ) -> Result<Done, Failure> {
        // A check that must change is checked before the confirmation is
        // spent, as for any step.
        if let Some(check) = check
            && check.require_change
        {
            let before = Literal {
                snapshot: act.as_ref().map(|(snapshot, _)| snapshot.clone()),
                ..Literal::click("e1")
            };
            if self
                .holds(check, &before, Vec::new(), Duration::ZERO)
                .await?
            {
                return Err(stopped(
                    "check_not_met",
                    format!(
                        "step {n}'s check already holds before the step, and it must change; nothing was sent, and confirmation {id} is still unused"
                    ),
                ));
            }
        }
        let taken = {
            let mut all = self.entry.confirmations.lock().expect("confirmations lock");
            all.iter_mut().find(|c| c.id == id).map(|c| {
                let was_used = c.used;
                c.used = true;
                let kind = match &c.kind {
                    Confirms::Click => Confirms::Click,
                    Confirms::Keys(op, text) => Confirms::Keys(*op, text.clone()),
                    Confirms::Fill(args) => Confirms::Fill(args.clone()),
                };
                (
                    was_used,
                    kind,
                    c.snapshot.clone(),
                    c.target.clone(),
                    c.document.clone(),
                    c.uid.clone(),
                    c.fingerprint.clone(),
                )
            })
        };
        let stale = |why: &str| {
            let mut failure = Failure::new(
                Exit::Denied,
                "confirm_stale",
                format!("confirmation {id} {why}; nothing was sent. Look at the page again"),
            );
            failure.details = json!({"confirmation": id});
            failure
        };
        let Some((used, kind, snapshot, target, document, uid, print)) = taken else {
            return Err(stale("does not exist in this session"));
        };
        self.audit.insert("confirmation".into(), json!(id));
        if used {
            return Err(stale("was already used"));
        }
        let (snapshot, target) = act.unwrap_or((snapshot, target));
        let enter = matches!(kind, Confirms::Keys(..));
        let press = match &kind {
            Confirms::Fill(args) => args["press"].as_str().unwrap_or_default().to_string(),
            _ => target.clone(),
        };
        let reviewed = call(
            self.entry,
            json!({"op": "review", "snapshot": snapshot, "target": press, "enter": enter}),
            self.audit,
        )
        .await;
        let reviewed = match reviewed {
            Ok((reviewed, _)) => reviewed,
            Err(failure) if failure.code == "stale_snapshot" => {
                return Err(stale("is for a page that has changed"));
            }
            Err(failure) => return Err(failure),
        };
        if reviewed["document"] != document.as_str() || reviewed["uid"] != uid {
            return Err(stale("is for an element no longer on the page"));
        }
        if fingerprint(&reviewed) != print {
            return Err(stale(
                "no longer matches the page: the element or the lines around it changed",
            ));
        }
        let literal = Literal {
            snapshot: Some(snapshot),
            ..Literal::click(&target)
        };
        let besides = match check {
            Some(c) if c.kind == CheckKind::Download => self.downloads().await?,
            _ => Vec::new(),
        };
        let guard = Some((print.as_str(), enter));
        let sent = match kind {
            Confirms::Click => self
                .dispatch(n, &literal, None, Some(id), guard)
                .await
                .map(|_| ()),
            Confirms::Keys(op, text) => {
                let literal = Literal {
                    op,
                    text: Some(text),
                    ..literal.clone()
                };
                self.dispatch(n, &literal, None, Some(id), guard)
                    .await
                    .map(|_| ())
            }
            Confirms::Fill(args) => self.fill_now(n, args, Some(id)).await,
        };
        match sent {
            Err(failure) if failure.code == "context_changed" => {
                return Err(stale(
                    "no longer matches the page: the element or the lines around it changed",
                ));
            }
            sent => sent?,
        }
        self.check(n, &literal, check, besides).await
    }

    /// One action, journaled before it is sent and once its result is known.
    /// With `guard` (a reviewed context's fingerprint, and whether it was
    /// reviewed as an Enter), the worker makes it only while the page still
    /// shows that context.
    async fn dispatch(
        &mut self,
        n: usize,
        literal: &Literal,
        risk_p: Option<f64>,
        confirmation: Option<&str>,
        guard: Option<(&str, bool)>,
    ) -> Result<Value, Failure> {
        let action = self.begin(n, literal.op, json!({"ref": literal.target}))?;
        let mut act = json!({
            "op": "act", "action": action_of(literal.op), "quiet": true,
            "snapshot": literal.snapshot, "target": literal.target, "text": literal.text,
        });
        if let Some((fingerprint, enter)) = guard {
            act["guard"] = json!({"fingerprint": fingerprint, "enter": enter});
        }
        let reply = call(self.entry, act, self.audit).await;
        let mut record = json!({"step": n, "action": action, "op": literal.op, "chosen": "ref"});
        if let Some(target) = &literal.target {
            record["ref"] = json!(target);
        }
        if literal.op == Op::Type
            && let Some(text) = &literal.text
        {
            record["text_length"] = json!(text.chars().count());
            record["text_sha256"] = json!(to_hex(&Sha256::digest(text.as_bytes())));
        }
        if let Some(p) = risk_p {
            record["risk_p"] = json!(model::round(p));
        }
        if let Some(id) = confirmation {
            record["confirmation"] = json!(id);
        }
        let mut shown = json!({"step": n, "action": action, "op": literal.op, "chosen": "ref"});
        if let Some(p) = risk_p {
            shown["risk_p"] = json!(model::round(p));
        }
        if let Some(id) = confirmation {
            shown["confirmation"] = json!(id);
        }
        let outcome = match reply {
            Ok((result, worker)) => {
                for key in ["role", "origin", "path"] {
                    if let Some(value) = worker.get(key) {
                        record[key] = value.clone();
                    }
                }
                shown["url"] = result["page"]["url"].clone();
                shown["title"] = result["page"]["title"].clone();
                shown["target"] = result["target"].clone();
                if let Some(changed) = result.get("changed") {
                    shown["changed"] = changed.clone();
                }
                if let Some(events) = result.get("events") {
                    shown["events"] = events.clone();
                }
                Ok(result)
            }
            Err(failure) => Err(failure),
        };
        let result = match &outcome {
            Ok(_) => "dispatched",
            Err(failure) if refused(failure) => "refused",
            Err(_) => "unknown",
        };
        self.end(action, result, &shown["target"])?;
        record["dispatch"] = json!(match result {
            "dispatched" => "sent",
            other => other,
        });
        shown["result"] = json!(result);
        self.records.push(record);
        self.trail.push(shown);
        outcome
    }

    /// The journal's record of an action about to be sent.
    fn begin(&mut self, n: usize, op: Op, target: Value) -> Result<usize, Failure> {
        self.actions += 1;
        let action = self.actions;
        self.journal.write(
            json!({"t": "sending", "step": n, "action": action, "op": op, "target": target}),
        )?;
        Ok(action)
    }

    fn end(&mut self, action: usize, result: &str, target: &Value) -> Result<(), Failure> {
        let mut record = json!({"t": "sent", "action": action, "result": result});
        if !target.is_null() {
            record["target"] = target.clone();
        }
        self.journal.write(record)
    }

    /// A literal fill: one that releases a secret field stops for a
    /// confirmation first; any other fills now.
    async fn fill(&mut self, n: usize, literal: &Literal) -> Result<(), Failure> {
        let mut args = fill_args(literal);
        if let Some(title) = super::fill::releases_secret(self.browsers, &args)? {
            args["item_title"] = json!(title);
            let failure = confirm_fill(self.entry, &args, self.audit).await;
            self.records.push(json!({"step": n, "op": "fill", "item": args["item"],
                                     "confirmation": failure.details["confirmation"]["id"], "dispatch": "refused"}));
            return Err(failure);
        }
        self.fill_now(n, args, None).await
    }

    /// A vault fill as one action: pause, origin match, set, press, clean
    /// up (`fill.rs`). The trail records the item and the fields filled,
    /// never a value.
    async fn fill_now(
        &mut self,
        n: usize,
        args: Value,
        confirmation: Option<&str>,
    ) -> Result<(), Failure> {
        let action = self.begin(
            n,
            Op::Fill,
            json!({"item": args["item"], "press": args["press"]}),
        )?;
        let mut fill_audit = Map::new();
        let outcome =
            super::fill::run(self.browsers, self.entry, "fill", &args, &mut fill_audit).await;
        let result = match &outcome {
            Ok(_) => "dispatched",
            Err(failure) if refused(failure) => "refused",
            Err(_) => "unknown",
        };
        self.end(action, result, &Value::Null)?;
        let mut record = json!({"step": n, "action": action, "op": "fill", "chosen": "ref",
                                "dispatch": if result == "dispatched" { "sent" } else { result }});
        for (key, value) in fill_audit {
            record[key] = value;
        }
        if let Some(id) = confirmation {
            record["confirmation"] = json!(id);
        }
        self.records.push(record);
        let mut shown = json!({"step": n, "action": action, "op": "fill", "item": args["item"],
                               "chosen": "ref", "result": result});
        if let Some(id) = confirmation {
            shown["confirmation"] = json!(id);
        }
        if let Ok(filled) = &outcome {
            shown["filled"] = filled["filled"].clone();
            shown["submitted"] = filled["submitted"].clone();
        }
        self.trail.push(shown);
        outcome.map(|_| ())
    }

    async fn downloads(&mut self) -> Result<Vec<String>, Failure> {
        let (listed, _) = call(self.entry, json!({"op": "downloads"}), self.audit).await?;
        Ok(listed["downloads"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["download"].as_str().map(str::to_string))
            .collect())
    }

    /// Whether a typed check holds within `within`.
    async fn holds(
        &mut self,
        check: &Check,
        literal: &Literal,
        besides: Vec<String>,
        within: Duration,
    ) -> Result<bool, Failure> {
        let waited = self.wait_for(check, literal, besides, within).await?;
        Ok(waited["met"] == true)
    }

    async fn wait_for(
        &mut self,
        check: &Check,
        literal: &Literal,
        besides: Vec<String>,
        within: Duration,
    ) -> Result<Value, Failure> {
        let condition = condition(check, literal.snapshot.as_deref(), besides);
        let (waited, _) = call(
            self.entry,
            json!({"op": "wait", "condition": condition, "within_ms": within.as_millis() as u64}),
            self.audit,
        )
        .await?;
        Ok(waited)
    }

    /// The step's check, within 10 s of its action.
    async fn check(
        &mut self,
        n: usize,
        literal: &Literal,
        check: Option<&Check>,
        besides: Vec<String>,
    ) -> Result<Done, Failure> {
        let Some(check) = check else {
            return Ok(Done {
                outcome: "dispatched",
                evidence: None,
            });
        };
        let within = CHECK_WITHIN.min(self.left());
        if check.kind != CheckKind::Sentence {
            let waited = self.wait_for(check, literal, besides, within).await?;
            let mut evidence = json!({"check": check.kind, "value": check.value});
            if let Some(download) = waited.get("download") {
                evidence["download"] = download.clone();
            }
            if waited["stale"] == true {
                let mut failure = stopped(
                    "stale_snapshot",
                    format!("step {n}'s field check names a ref from a page that has changed"),
                );
                failure.details = json!({"evidence": evidence});
                return Err(failure);
            }
            if waited["met"] != true {
                let mut failure = stopped(
                    "check_not_met",
                    format!("step {n}'s check did not hold within {}s", within.as_secs()),
                );
                failure.details = json!({"evidence": evidence});
                return Err(failure);
            }
            return Ok(Done {
                outcome: "checked",
                evidence: Some(evidence),
            });
        }
        let deadline = Instant::now() + within;
        let verdict = loop {
            let (seen, _) = call(self.entry, json!({"op": "lines"}), self.audit).await?;
            let lines: Vec<String> = strings(&seen["lines"])
                .into_iter()
                .chain(strings(&seen["fields"]))
                .collect();
            let judged = self
                .model
                .judge(seen["page"].clone(), &check.value, &lines)
                .await;
            record_model(&self.model, self.audit);
            let verdict = judged.map_err(|_| model_unavailable(&self.model, "a sentence check"))?;
            if verdict.result == "met" || Instant::now() + JUDGE_EVERY > deadline {
                break verdict;
            }
            tokio::time::sleep(JUDGE_EVERY).await;
        };
        let mut evidence = verdict.to_json();
        evidence["check"] = json!("sentence");
        if verdict.result != "met" {
            let mut failure = stopped(
                "check_not_met",
                format!(
                    "step {n}'s check {:?} was judged {}",
                    check.value,
                    verdict.result.replace('_', " ")
                ),
            );
            failure.details = json!({"evidence": evidence});
            return Err(failure);
        }
        Ok(Done {
            outcome: "model_assessed",
            evidence: Some(evidence),
        })
    }
}

/// A step that led to a sign-in form or a CAPTCHA stops the plan there.
fn wall(acted: &Value) -> Result<(), Failure> {
    match acted["wall"].as_str() {
        Some("login") => Err(stopped(
            "needs_login",
            "this step led to a sign-in form; the rest of the plan did not run: sign in with `login`, then go on".into(),
        )),
        Some("captcha") => {
            let mut failure = stopped(
                "needs_owner",
                "this step led to a CAPTCHA; the rest of the plan did not run: hand off to the owner".into(),
            );
            failure.details = json!({"reason": "captcha"});
            Err(failure)
        }
        _ => Ok(()),
    }
}

/// A refusal is made before any input; anything else may have reached the
/// page.
fn refused(failure: &Failure) -> bool {
    matches!(failure.exit, Exit::Usage | Exit::Denied)
        || matches!(
            failure.code.as_str(),
            "stale_snapshot"
                | "covered_target"
                | "no_such_target"
                | "dialog_open"
                | "no_history"
                | "session_paused"
                | "origin_mismatch"
                | "no_such_item"
                | "context_changed"
        )
}

/// Whether a step presses Enter in a field: `press` Enter, or `type` text
/// with a line break, which the engine types as Enter.
fn presses_enter(literal: &Literal) -> bool {
    let text = literal.text.as_deref().unwrap_or_default();
    match literal.op {
        Op::Press => text.eq_ignore_ascii_case("enter"),
        Op::Type => text.contains(['\n', '\r']),
        _ => false,
    }
}

fn action_of(op: Op) -> Action {
    match op {
        Op::Click => Action::Click,
        Op::Type => Action::Type,
        Op::Select => Action::Select,
        Op::Press => Action::Press,
        Op::Scroll => Action::Scroll,
        Op::Back => Action::Back,
        Op::Fill => unreachable!("a fill is no act"),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| line.as_str().map(str::to_string))
        .collect()
}

/// A check as the worker's typed condition.
fn condition(check: &Check, snapshot: Option<&str>, besides: Vec<String>) -> Value {
    let value = check.value.as_str();
    match check.kind {
        CheckKind::Origin => json!({"kind": "origin", "origin": value}),
        CheckKind::Path => json!({"kind": "path", "path": value}),
        CheckKind::Query => {
            let (name, value) = value.split_once('=').unwrap_or_default();
            json!({"kind": "query", "name": name, "value": value})
        }
        CheckKind::Text => json!({"kind": "text", "text": value}),
        CheckKind::Field => {
            let (target, value) = value.split_once('=').unwrap_or_default();
            json!({"kind": "field", "snapshot": snapshot, "target": target, "value": value})
        }
        CheckKind::Download => json!({"kind": "download", "besides": besides}),
        CheckKind::Sentence => unreachable!("judged by the model"),
    }
}

/// What a click or Enter would do, in one line, for the risk question.
fn action_line(reviewed: &Value, enter: bool, typing: bool) -> String {
    let element = &reviewed["element"];
    let named = |e: &Value| {
        format!(
            "{} {:?}",
            e["role"].as_str().unwrap_or("element"),
            e["name"].as_str().unwrap_or_default()
        )
    };
    let mut line = if enter {
        let mut line = if typing {
            format!(
                "type text with a line break, which presses Enter, in {}",
                named(element)
            )
        } else {
            format!("press Enter in {}", named(element))
        };
        match reviewed["acts_on"].as_object() {
            Some(_) if reviewed["acts_on"]["role"] == "form" => {
                line.push_str(", which submits its form")
            }
            Some(_) => line.push_str(&format!(
                ", which submits its form with {}",
                named(&reviewed["acts_on"])
            )),
            None => {}
        }
        line
    } else {
        format!("click {}", named(element))
    };
    if let Some(row) = element["row"].as_str() {
        line.push_str(&format!(" — in: {row}"));
    }
    line
}

/// A confirmation as the agent sees it: its id, the target and the context
/// it was reviewed in.
fn reviewed_json(id: &str, reviewed: &Value) -> Value {
    let mut target = json!({"ref": reviewed["ref"], "snapshot": reviewed["snapshot"]});
    for key in ["role", "name", "row"] {
        if !reviewed["element"][key].is_null() {
            target[key] = reviewed["element"][key].clone();
        }
    }
    let mut shown =
        json!({"id": id, "target": target, "title": reviewed["title"], "lines": reviewed["lines"]});
    if !reviewed["acts_on"].is_null() {
        shown["acts_on"] = reviewed["acts_on"].clone();
    }
    shown
}

/// A literal fill as `fill.rs` takes its arguments.
fn fill_args(literal: &Literal) -> Value {
    let fields: Vec<Value> = literal
        .fields
        .iter()
        .map(|(label, refs)| json!([label, refs]))
        .collect();
    json!({"item": literal.item, "fields": fields, "press": literal.press, "snapshot": literal.snapshot})
}

/// A fill that releases a secret field stops here, before any value is
/// released, with a confirmation bound to its press button and the lines
/// around it. Its confirmation fills, presses and cleans up as one step.
pub async fn confirm_fill(entry: &Entry, args: &Value, audit: &mut Map<String, Value>) -> Failure {
    let press = args["press"].as_str().unwrap_or_default();
    let reviewed = match call(
        entry,
        json!({"op": "review", "snapshot": args["snapshot"], "target": press}),
        audit,
    )
    .await
    {
        Ok((reviewed, _)) => reviewed,
        Err(failure) => return failure,
    };
    let id = format!("c{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff);
    let mut fill = args.clone();
    fill["snapshot"] = reviewed["snapshot"].clone();
    // Its press is made only while the page still shows what was confirmed.
    fill["guard"] = json!({"fingerprint": fingerprint(&reviewed)});
    entry
        .confirmations
        .lock()
        .expect("confirmations lock")
        .push(Confirmation {
            id: id.clone(),
            kind: Confirms::Fill(fill),
            snapshot: reviewed["snapshot"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            target: press.to_string(),
            document: reviewed["document"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            uid: reviewed["uid"].clone(),
            fingerprint: fingerprint(&reviewed),
            used: false,
        });
    audit.insert("confirmation".into(), json!(id));
    let mut shown = reviewed_json(&id, &reviewed);
    shown["op"] = json!("fill");
    shown["item"] = args["item"].clone();
    if let Some(title) = args["item_title"].as_str() {
        shown["item_title"] = json!(title);
    }
    let mut failure = Failure::new(
        Exit::Denied,
        "confirm_needed",
        format!(
            "this fill releases a secret field and presses {:?}; nothing was released. If that is what the task needs, confirm it with `latch browser do --confirm {id}`",
            reviewed["element"]["name"].as_str().unwrap_or(press)
        ),
    );
    failure.details = json!({"confirmation": shown});
    failure
}

/// `do --history R --shot N`: one of request R's screenshots, for the
/// caller that sent it.
pub fn shot(
    browsers: &Browsers,
    session: &str,
    caller: &str,
    request: &str,
    shot: &str,
) -> Result<Vec<u8>, Failure> {
    journal::shot(&browsers.state_dir(), session, caller, request, shot)
}

/// `do --history R`: request R's steps, actions and outcomes, from its
/// journal, for the caller that sent it.
pub fn history(
    browsers: &Browsers,
    session: &str,
    caller: &str,
    request: &str,
) -> Result<Value, Failure> {
    let running = browsers.is_running(&format!("{session}/{request}"));
    journal::history(&browsers.state_dir(), session, caller, request, running)
}

/// `find --role R --name N`: candidate refs, by role and exact name. No
/// model; it never acts.
pub async fn find(
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let (Some(role), Some(name)) = (args["role"].as_str(), args["name"].as_str()) else {
        return Err(usage("find takes --role R --name N".into()));
    };
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    audit.insert("role".into(), json!(role));
    let (found, _) = call(
        entry,
        json!({"op": "resolve", "role": role, "name": name}),
        audit,
    )
    .await?;
    Ok(found)
}

/// `wait`: for an element's state, for text, or for a path, with a
/// deadline. Not met in time is `timeout`.
pub async fn wait(
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let within = args["within"].as_f64().unwrap_or(WAIT_WITHIN);
    if !(within > 0.0 && within <= MAX_WAIT) {
        return Err(usage(format!(
            "--within is between 0 and {MAX_WAIT} seconds"
        )));
    }
    let condition = if let Some(target) = args["ref"].as_str() {
        json!({"kind": "ref", "snapshot": args["snapshot"], "target": target,
               "state": args["state"].as_str().unwrap_or("visible")})
    } else if let Some(text) = args["text"].as_str() {
        json!({"kind": "text", "text": text})
    } else if let Some(path) = args["url_path"].as_str() {
        json!({"kind": "path", "path": path})
    } else {
        return Err(usage(
            "wait takes --ref eN --state S, --text T or --url-path P".into(),
        ));
    };
    audit.insert("kind".into(), condition["kind"].clone());
    if entry.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let ms = (within * 1000.0) as u64;
    let (mut waited, _) = call(
        entry,
        json!({"op": "wait", "condition": condition, "within_ms": ms}),
        audit,
    )
    .await?;
    let met = waited["met"] == true;
    audit.insert("verdict".into(), json!(if met { "met" } else { "not_met" }));
    if let Ok((glance, _)) = call(entry, json!({"op": "glance"}), audit).await {
        waited["page"] = glance;
    }
    if !met {
        let mut failure = Failure::failed(
            "timeout",
            format!("the condition did not hold within {within}s"),
        );
        failure.details = waited;
        return Err(failure);
    }
    Ok(waited)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_the_clients_own() {
        assert!(request_id(&json!({})).is_err());
        assert!(request_id(&json!({"request": "r 1"})).is_err());
        assert_eq!(request_id(&json!({"request": "r1a2"})).unwrap(), "r1a2");
    }

    #[test]
    fn the_step_policy_is_a_host_setting() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(policy(dir.path()).unwrap(), Policy::Any);
        std::fs::create_dir_all(dir.path().join("browser")).unwrap();
        let file = dir.path().join(POLICY_FILE);
        std::fs::write(&file, r#"{"literal_steps": "stop"}"#).unwrap();
        assert_eq!(policy(dir.path()).unwrap(), Policy::Stop);
        std::fs::write(&file, r#"{"literal_steps": "some"}"#).unwrap();
        assert_eq!(policy(dir.path()).unwrap_err().code, "bad_policy");
    }

    #[test]
    fn a_confirmation_is_bound_to_the_element_and_the_lines_around_it() {
        let reviewed = json!({"document": "f/1", "uid": 4, "title": "Checkout",
            "element": {"role": "button", "name": "Place order"}, "lines": ["Total $10.00", "Place order"]});
        let mut repriced = reviewed.clone();
        repriced["lines"][0] = json!("Total $100.00");
        assert_ne!(fingerprint(&reviewed), fingerprint(&repriced));
        assert_eq!(
            action_line(&reviewed, false, false),
            "click button \"Place order\""
        );
        let enter = json!({"element": {"role": "textbox", "name": "Gift note"},
                           "acts_on": {"role": "button", "name": "Place order"}});
        assert_eq!(
            action_line(&enter, true, false),
            "press Enter in textbox \"Gift note\", which submits its form with button \"Place order\""
        );
    }
}
