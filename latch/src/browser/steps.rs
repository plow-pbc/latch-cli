//! The host's step executor: the plan command (`do`), `act` (a plan of one
//! literal step), instruction steps, confirmations, `find` and `wait`.
//!
//! **Before anything is sent** the whole plan is checked: its shape, every
//! ref pinned to the snapshot it is read in, and every sentence check
//! (refused as `vague_condition`, or `model_unavailable` with no model).
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
//! **Each instruction step**, a goal in words, runs pass by pass in
//! `goal.rs`, its actions going through the same risk decision, dispatch
//! and journal.
//!
//! **A confirmation** is used once, in the session and document it was
//! issued in, and only while its element and the lines around it are
//! unchanged: otherwise `confirm_stale`. It is carried out by
//! `do --confirm C` only, and never needs a new risk decision; a click sent
//! any other way, on the same ref too, is judged again.
//!
//! The decision model (`model.rs`) runs here, never in the worker, and the
//! worker is not held while it answers.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::core::{Action, fingerprint};
use super::host::{Browsers, Entry, paused, take_audit};
use super::journal::{self, Journal};
use super::judged::{self, Judged};
use super::model::{self, CHECKABLE_P, Model, RISKY_P};
use super::plan::{Check, CheckKind, Literal, Op, Plan, Step};
use super::session::Session;
use crate::failure::{Exit, Failure};
use crate::gate::Gate;
use crate::state::to_hex;

mod goal;

/// How long a step's check may wait after its action.
const CHECK_WITHIN: Duration = Duration::from_secs(10);
/// How often a sentence is judged again while its check waits.
const JUDGE_EVERY: Duration = Duration::from_millis(1000);
/// How often a typed condition is checked while it waits.
const WAIT_POLL: Duration = Duration::from_millis(250);
/// `wait`'s default and largest deadline, in seconds.
const WAIT_WITHIN: f64 = 10.0;
const MAX_WAIT: f64 = 120.0;

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

/// The fill a confirmation in this session would release, while it is
/// still unused: what the gatekeeper judges before it is carried out.
pub(super) fn fill_to_confirm(entry: &Entry, id: &str) -> Option<Value> {
    let all = entry.confirmations.lock().expect("confirmations lock");
    all.iter()
        .find(|c| c.id == id && !c.used)
        .and_then(|c| match &c.kind {
            Confirms::Fill(args) => Some(args.clone()),
            _ => None,
        })
}

/// A risk the pass that chose a click or Enter answered, and the context
/// the pass saw it in, as a review reads it: `target`, the element clicked
/// (role, name and row) or what Enter submits (its form's submit control,
/// else the form); `lines_sha256`, the lines around it; and the page's
/// `title`.
pub(super) struct Passed {
    pub(super) p: f64,
    pub(super) seen: Value,
}

impl Passed {
    /// Whether a fresh review finds the very context the pass saw.
    fn holds(&self, reviewed: &Value, enter: bool) -> bool {
        let now = if enter {
            &reviewed["acts_on"]
        } else {
            &reviewed["element"]
        };
        let then = &self.seen["target"];
        let known = |v: &Value| !v.is_null();
        known(then)
            && known(now)
            && ["role", "name", "row"]
                .iter()
                .all(|key| now[*key] == then[*key])
            && self.seen["lines_sha256"] == json!(lines_sha256(reviewed))
            && known(&self.seen["title"])
            && reviewed["title"] == self.seen["title"]
    }
}

/// The SHA-256 of the lines a review read, one per line of text, as the
/// page's reader hashes them for a pass.
fn lines_sha256(reviewed: &Value) -> String {
    to_hex(&Sha256::digest(
        strings(&reviewed["lines"]).join("\n").as_bytes(),
    ))
}

/// Waits until `deadline` for `condition`, checking it a few times a
/// second: between checks the browser is free for other sessions' calls.
/// A check can also end the wait early: the condition's element is from a
/// page that is gone, or a dialog blocks the page.
async fn poll(
    entry: &Entry,
    condition: &Value,
    deadline: Instant,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    loop {
        let check = json!({"op": "wait", "condition": condition, "within_ms": 0});
        let (waited, _) = call(entry, check, audit).await?;
        let ended =
            waited["met"] == true || waited["stale"] == true || waited["blocked_by_dialog"] == true;
        let left = deadline.saturating_duration_since(Instant::now());
        if ended || left.is_zero() {
            return Ok(waited);
        }
        tokio::time::sleep(left.min(WAIT_POLL)).await;
    }
}

/// One worker call for the agent; its audit fills in what the request's
/// audit lacks (tab, origin, path), and is returned for the action's own
/// record. The session is held only for the call, and refused while the
/// owner has it (`handoff`): a plan under way sends nothing more.
pub(super) async fn call(
    entry: &Entry,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Map<String, Value>), Failure> {
    let mut session = entry.agent_session().await?;
    send(&mut session, call, audit).await
}

/// A handoff's own worker call, made while the owner has the session.
pub(super) async fn owner_call(
    entry: &Entry,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Map<String, Value>), Failure> {
    let mut session = entry.session.lock().await;
    send(&mut session, call, audit).await
}

async fn send(
    session: &mut Session,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<(Value, Map<String, Value>), Failure> {
    let reply = session.call(call).await;
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
    let mut failure = Failure::failed(
        "model_unavailable",
        format!(
            "{what} needs the decision model, and {}; nothing was sent",
            model.why_off()
        ),
    );
    if let Some(reason) = model.reason() {
        failure.details = json!({"reason": reason});
    }
    failure
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
/// Where a run stopped, for the journal and the audit: the step, the code,
/// and the failure's reason when it gives one.
fn stopped_at(step: usize, failure: &Failure) -> Value {
    let mut stop = json!({"step": step, "code": failure.code});
    if let Some(reason) = failure.details.get("reason") {
        stop["reason"] = reason.clone();
    }
    stop
}

fn record_model(model: &Model, audit: &mut Map<String, Value>) {
    if !model.calls.is_empty() {
        audit.insert("model_calls".into(), json!(model.calls));
    }
}

/// One step of a run: a confirmation to carry out, a literal action, or
/// a goal in words.
enum Work {
    Confirm(String),
    Literal(Literal, Option<Check>),
    Goal(Step),
}

/// `do` and `act`: a plan's steps, or a confirmation and the steps after
/// it.
pub async fn run(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    caller: &str,
    gate: Option<Gate<'_>>,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let request = request_id(args)?;
    audit.insert("request".into(), json!(request));
    let mut plan: Plan = serde_json::from_value(args["plan"].clone())
        .map_err(|err| usage(format!("the plan is malformed: {err}")))?;
    plan.validate().map_err(usage)?;
    plan.resolve_facts();
    let confirm = args["confirm"].as_str().map(str::to_string);
    if plan.steps.is_empty() && confirm.is_none() {
        return Err(usage("a plan needs a step".into()));
    }
    audit.insert("steps".into(), json!(plan.steps.len()));
    audit.insert(
        "goals".into(),
        json!(plan.steps.iter().filter(|s| s.goal.is_some()).count()),
    );
    let first = usize::from(confirm.is_some()) + 1;
    let mut model = browsers.model();
    if let Some(n) = plan.steps.iter().position(|s| s.goal.is_some())
        && !model.is_on()
    {
        let mut failure =
            model_unavailable(&model, &format!("step {} (a goal in words)", n + first));
        failure.details = json!({"step": n + first});
        return Err(failure);
    }
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
    if entry.runtime.filling.load(Ordering::SeqCst) {
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
        work.push(Work::Confirm(id.clone()));
    }
    for (n, step) in plan.steps.iter().enumerate() {
        let Some(mut literal) = step.literal.clone() else {
            work.push(Work::Goal(step.clone()));
            continue;
        };
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

    let summaries: Vec<Value> = work
        .iter()
        .enumerate()
        .map(|(n, step)| match step {
            Work::Confirm(id) => json!({"step": n + 1, "confirm": id}),
            Work::Literal(literal, check) => {
                let mut summary = json!({"step": n + 1, "op": literal.op, "ref": literal.target});
                if let Some(check) = check {
                    summary["check"] = json!(check.kind);
                }
                summary
            }
            Work::Goal(step) => {
                let mut summary = json!({"step": n + 1, "goal": step.goal});
                if let Some(check) = &step.check {
                    summary["check"] = json!(check.kind);
                }
                summary
            }
        })
        .collect();
    let context = goal::Context {
        plan: work
            .iter()
            .map(|step| match step {
                Work::Confirm(id) => format!("carry out confirmation {id}"),
                Work::Literal(literal, _) => match &literal.target {
                    Some(target) => format!(
                        "{} {target}",
                        json!(literal.op).as_str().unwrap_or_default()
                    ),
                    None => json!(literal.op).as_str().unwrap_or_default().to_string(),
                },
                Work::Goal(step) => step.goal.clone().unwrap_or_default(),
            })
            .collect(),
        items: plan.items.clone(),
    };
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
        session,
        request: &request,
        model,
        journal,
        audit,
        deadline: Instant::now() + Duration::from_secs_f64(plan.within()),
        steps: Vec::new(),
        trail: Vec::new(),
        records: Vec::new(),
        actions: 0,
        dispatched: HashSet::new(),
        choice: None,
        gate,
        judgment: None,
    };
    let mut stop = None;
    for (n, step) in work.into_iter().enumerate() {
        let step_no = n + 1;
        let mut shown = json!({"step": step_no, "actions": 0});
        let before = run.actions;
        let outcome = match step {
            Work::Confirm(id) => {
                shown["confirm"] = json!(id);
                run.confirmed(step_no, &id).await
            }
            Work::Literal(literal, check) => {
                shown["op"] = json!(literal.op);
                run.literal(step_no, &literal, check.as_ref()).await
            }
            Work::Goal(step) => {
                shown["goal"] = json!(step.goal);
                run.goal(step_no, n, &step, &context).await
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
                if let Some(reason) = failure.details.get("reason") {
                    shown["reason"] = reason.clone();
                }
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
        None if run.steps.iter().all(|s| {
            matches!(
                s["outcome"].as_str(),
                Some("checked" | "model_assessed" | "already_held")
            )
        }) =>
        {
            "reached"
        }
        None => "dispatched",
    };
    let mut end = json!({"t": "end", "status": status});
    if let Some((step, failure)) = &stop {
        end["stop"] = stopped_at(*step, failure);
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
    // Each step's kind and outcome, for the metrics: never its words.
    let outcomes: Vec<Value> = steps
        .iter()
        .map(|s| {
            let kind = if s.get("goal").is_some() {
                "goal"
            } else if s.get("confirm").is_some() {
                "confirm"
            } else {
                "literal"
            };
            let mut outcome = json!({"step": s["step"], "kind": kind, "outcome": s["outcome"],
                                     "actions": s["actions"]});
            if let Some(code) = s.get("code") {
                outcome["code"] = code.clone();
            }
            outcome
        })
        .collect();
    audit.insert("step_outcomes".into(), json!(outcomes));
    // Every answer ends with the page as it is now.
    let page = call(entry, json!({"op": "glance"}), audit)
        .await
        .map(|(page, _)| page)
        .unwrap_or(Value::Null);
    let mut out =
        json!({"request": request, "status": status, "steps": steps, "trail": trail, "page": page});
    // `--output F`: the page as the plan ended, kept as its `final` shot (a
    // stop for a confirmation keeps its own, below).
    let confirming = stop
        .as_ref()
        .is_some_and(|(_, f)| f.code == "confirm_needed");
    if args["output"] == true
        && !confirming
        && let Some((handle, meta)) =
            keep_shot(browsers, entry, session, &request, "final", "shot").await
    {
        audit.insert("screenshot".into(), meta);
        out["screenshot"] = handle;
    }
    // A final step only the model judged is for the agent to review.
    if steps.last().is_some_and(|s| {
        s["outcome"] == "model_assessed"
            || s["outcome"] == "already_held" && !s["evidence"]["p"].is_null()
    }) {
        out["review"] = json!(true);
    }
    let Some((step, mut failure)) = stop else {
        return Ok(out);
    };
    rebase(&mut failure.details, &page);
    audit.insert("stop".into(), stopped_at(step, &failure));
    let mut shown = json!({"step": step, "code": failure.code, "message": failure.message});
    for (key, value) in failure.details.as_object().into_iter().flatten() {
        shown[key] = value.clone();
    }
    if failure.code == "confirm_needed"
        && let Some((handle, meta)) =
            keep_shot(browsers, entry, session, &request, "final", "screenshot").await
    {
        audit.insert("screenshot".into(), meta);
        shown["confirmation"]["screenshot"] = handle.clone();
        out["screenshot"] = handle;
    }
    out["stop"] = shown;
    failure.details = out;
    Err(failure)
}

/// A screenshot of the page now, kept with request `request` as shot
/// `name` (`final`, or an action's number), taken by worker call `op`: its
/// handle, which `do --history R --shot <name>` fetches, and what the audit
/// records of it. None if the page could not be shot, or (`shot`) a value
/// latch filled from the vault is on it.
async fn keep_shot(
    browsers: &Browsers,
    entry: &Entry,
    session: &str,
    request: &str,
    name: &str,
    op: &str,
) -> Option<(Value, Value)> {
    let reply = entry
        .agent_session()
        .await
        .ok()?
        .call(json!({"op": op}))
        .await
        .ok()?;
    let png = reply.bytes?;
    journal::save_shot(&browsers.state_dir(), session, request, name, &png).ok()?;
    let meta = json!({"shot": name, "size": png.len(), "sha256": to_hex(&Sha256::digest(&png))});
    let handle = match name.parse::<usize>() {
        Ok(n) => json!({"request": request, "shot": n}),
        Err(_) => json!({"request": request, "shot": name}),
    };
    Some((handle, meta))
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
    let mut failure = confirm_fill(browsers, entry, args, audit).await;
    let stop = json!({"step": 1, "code": failure.code});
    let journaled = journal
        .write(json!({"t": "step", "step": 1, "op": "fill", "outcome": "stopped", "code": failure.code}))
        .and_then(|()| journal.write(json!({"t": "end", "status": "stopped", "stop": stop})));
    if let Err(lost) = journaled {
        return lost;
    }
    if failure.code == "confirm_needed"
        && let Some((handle, meta)) =
            keep_shot(browsers, entry, session, &request, "final", "screenshot").await
    {
        audit.insert("screenshot".into(), meta);
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
    session: &'a str,
    request: &'a str,
    model: Model,
    journal: Journal,
    audit: &'a mut Map<String, Value>,
    deadline: Instant,
    steps: Vec<Value>,
    trail: Vec<Value>,
    /// The audit's record of each action.
    records: Vec<Value>,
    actions: usize,
    /// Every action an instruction step dispatched, with the page state it
    /// was dispatched on.
    dispatched: HashSet<(String, String)>,
    /// How the model chose the action being dispatched, for its record.
    choice: Option<Value>,
    /// The agent's gate: every vault fill a plan makes is judged, as a
    /// `fill` command is.
    gate: Option<Gate<'a>>,
    /// The gatekeeper's judgment of the fill being made, for its record.
    judgment: Option<Value>,
}

fn stopped(code: &str, message: String) -> Failure {
    Failure::failed(code, message)
}

impl<'a> Run<'a> {
    /// Judges a vault fill the plan is about to make, as `fill` is
    /// judged; a refusal is recorded and stops the plan, nothing sent.
    async fn judge_fill(&mut self, n: usize, args: &Value) -> Result<Option<Judged<'a>>, Failure> {
        let Some(gate) = self.gate else {
            return Ok(None);
        };
        let site = judged::site(self.browsers, self.entry).await?;
        let action = judged::fill(self.browsers, args, site, false);
        let (judged, record) = judged::judgment(gate, action).await;
        match judged {
            Ok(judged) => {
                self.judgment = Some(record);
                Ok(Some(judged))
            }
            Err(failure) => {
                self.records
                    .push(json!({"step": n, "op": "fill", "item": args["item"],
                                         "gate": record, "dispatch": "refused"}));
                Err(failure)
            }
        }
    }
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
        match literal.op {
            Op::Fill => self.fill(n, literal).await?,
            Op::Click => self.risky(n, literal, false, None).await?,
            _ if presses_enter(literal) => self.risky(n, literal, true, None).await?,
            _ => {
                self.dispatch(n, literal, None, None, None).await?;
            }
        }
        self.check(n, literal, check, besides).await
    }

    /// A click or Enter: the model's risk decision on what it would act on,
    /// then the action, made only if that is still what the page shows, or a
    /// confirmation in its place.
    /// The risk decision on a click or Enter, then the action or its
    /// confirmation. `passed` is the risk the pass that chose the action
    /// already answered, low enough to stand for the decision, with what
    /// the pass saw the action act on: while the review finds that
    /// unchanged, no call of its own is made.
    async fn risky(
        &mut self,
        n: usize,
        literal: &Literal,
        enter: bool,
        passed: Option<Passed>,
    ) -> Result<(), Failure> {
        let reviewed = self.review(literal, enter).await?;
        if !self.model.is_on() {
            return Err(model_unavailable(&self.model, "a click or Enter"));
        }
        let what = action_line(&reviewed, enter, literal.op == Op::Type);
        let changed = passed.as_ref().is_some_and(|p| !p.holds(&reviewed, enter));
        if changed && let Some(choice) = self.choice.as_mut() {
            choice["risk_from"] = json!("own_call");
            choice["pass_context"] = json!("changed");
        }
        let p = match passed.filter(|_| !changed) {
            Some(passed) => passed.p,
            None => {
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
                judged.map_err(|_| model_unavailable(&self.model, "a click or Enter"))?
            }
        };
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
        let kind = match enter {
            true => Confirms::Keys(literal.op, literal.text.clone().unwrap_or_default()),
            false => Confirms::Click,
        };
        let id = self.issue(kind, &reviewed);
        let mut record = json!({"step": n, "op": literal.op, "ref": literal.target,
                                "risk_p": model::round(p), "confirmation": id, "dispatch": "refused"});
        for (key, value) in self.choice.iter().flat_map(|c| c.as_object()).flatten() {
            record[key] = value.clone();
        }
        self.records.push(record);
        let mut failure = Failure::new(
            Exit::Denied,
            "confirm_needed",
            format!(
                "{what} looks like it can't be undone; nothing was sent. If it is what the task needs, confirm it with `latch browser do --confirm {id}`"
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
    /// was issued for, with no new risk decision.
    async fn confirmed(&mut self, n: usize, id: &str) -> Result<Done, Failure> {
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
        let guard = Some((print.as_str(), enter));
        let sent = match kind {
            Confirms::Click => self.dispatch(n, &literal, None, Some(id), guard).await,
            Confirms::Keys(op, text) => {
                let literal = Literal {
                    op,
                    text: Some(text),
                    ..literal.clone()
                };
                self.dispatch(n, &literal, None, Some(id), guard).await
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
        self.check(n, &literal, None, Vec::new()).await
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
    ) -> Result<(), Failure> {
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
        let mut shown = json!({"step": n, "action": action, "op": literal.op, "chosen": "ref"});
        if let Some(choice) = &self.choice {
            for (key, value) in choice.as_object().into_iter().flatten() {
                record[key] = value.clone();
                shown[key] = value.clone();
            }
        }
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
                Ok(popup_of(&result))
            }
            Err(failure) => Err(failure),
        };
        // A page the action opened from the session's tab is where the plan
        // goes on: the session now acts in it, and nothing comes to the
        // front.
        let outcome = match outcome {
            Ok(Some(tab)) => {
                let switched = call(
                    self.entry,
                    json!({"op": "tab", "action": "switch", "tab": tab}),
                    self.audit,
                )
                .await;
                if switched.is_ok() {
                    shown["followed"] = json!(tab);
                    record["followed"] = json!(tab);
                }
                Ok(())
            }
            Ok(None) => Ok(()),
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
        if result == "dispatched" {
            self.shoot(action, &mut record, &mut shown).await;
        }
        self.records.push(record);
        self.trail.push(shown);
        outcome
    }

    /// A screenshot after action `action`, for the trail: its handle in the
    /// action's trail entry, its size and sha256 in its audit record, or a
    /// marker where a vault value on the page keeps it from being taken.
    async fn shoot(&mut self, action: usize, record: &mut Value, shown: &mut Value) {
        let name = action.to_string();
        match keep_shot(
            self.browsers,
            self.entry,
            self.session,
            self.request,
            &name,
            "shot",
        )
        .await
        {
            Some((handle, meta)) => {
                shown["screenshot"] = handle;
                record["screenshot"] = meta;
            }
            None => shown["screenshot"] = json!({"suppressed": true}),
        }
    }

    /// The journal's record of an action about to be sent.
    fn begin(&mut self, n: usize, op: Op, target: Value) -> Result<usize, Failure> {
        self.begin_named(n, json!(op), target)
    }

    fn begin_named(&mut self, n: usize, op: Value, target: Value) -> Result<usize, Failure> {
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
        let mut args = super::fill::resolve(self.browsers, self.entry, &fill_args(literal)).await?;
        let judged = self.judge_fill(n, &args).await?;
        let filled = match super::fill::releases_secret(self.browsers, &args) {
            Ok(Some(title)) => {
                args["item_title"] = json!(title);
                let failure = confirm_fill(self.browsers, self.entry, &args, self.audit).await;
                self.records.push(json!({"step": n, "op": "fill", "item": args["item"],
                                         "confirmation": failure.details["confirmation"]["id"], "dispatch": "refused",
                                         "gate": self.judgment.take()}));
                Err(failure)
            }
            Ok(None) => self.fill_now(n, args, None).await,
            Err(failure) => Err(failure),
        };
        if let Some(judged) = judged {
            judged.ended(&filled);
        }
        filled
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
        let mut outcome = super::fill::run(self.browsers, self.entry, &args, &mut fill_audit).await;
        // A confirmed fill whose fields or their frames went from the page
        // is no longer the fill that was confirmed.
        if confirmation.is_some()
            && let Err(failure) = &mut outcome
            && matches!(failure.code.as_str(), "stale_snapshot" | "fill_refused")
        {
            failure.code = "context_changed".into();
        }
        let result = match &outcome {
            Ok(_) => "dispatched",
            Err(failure) if refused(failure) => "refused",
            Err(_) => "unknown",
        };
        self.end(action, result, &Value::Null)?;
        let mut record = json!({"step": n, "action": action, "op": "fill", "chosen": "ref",
                                "dispatch": if result == "dispatched" { "sent" } else { result }});
        if let Some(judgment) = self.judgment.take() {
            record["gate"] = judgment;
        }
        for (key, value) in fill_audit {
            record[key] = value;
        }
        for (key, value) in self
            .choice
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            record[key] = value.clone();
        }
        if let Some(id) = confirmation {
            record["confirmation"] = json!(id);
        }
        self.records.push(record);
        // No screenshot: the page may still show what was filled.
        let mut shown = json!({"step": n, "action": action, "op": "fill", "item": args["item"],
                               "chosen": "ref", "result": result,
                               "screenshot": {"suppressed": true}});
        for (key, value) in self
            .choice
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            shown[key] = value.clone();
        }
        if let Some(id) = args["suggestion"].as_str() {
            shown["suggestion"] = json!(id);
        }
        if let Some(id) = confirmation {
            shown["confirmation"] = json!(id);
        }
        if let Ok(filled) = &outcome {
            shown["fields"] = filled["fields"].clone();
            shown["stages"] = filled["stages"].clone();
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
        poll(self.entry, &condition, Instant::now() + within, self.audit).await
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

/// A stop's refs (its `candidates`, its `field`), read in latch's own
/// snapshot of the page, as refs in the page the answer ends with: the
/// element of the same role and name. One the page no longer shows loses
/// its ref.
fn rebase(details: &mut Value, page: &Value) {
    let elements = page["elements"].as_array().cloned().unwrap_or_default();
    let fix = |candidate: &mut Value| {
        let same = |e: &&Value| e["role"] == candidate["role"] && e["name"] == candidate["name"];
        let found = elements
            .iter()
            .filter(same)
            .find(|e| e["ref"] == candidate["ref"])
            .or_else(|| elements.iter().find(same));
        candidate["ref"] = found.map_or(Value::Null, |e| e["ref"].clone());
    };
    if let Some(candidates) = details.get_mut("candidates").and_then(Value::as_array_mut) {
        candidates.iter_mut().for_each(fix);
    }
    if let Some(field) = details.get_mut("field").filter(|f| f.is_object()) {
        fix(field);
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

/// The tab an action's page opened, from its events: a new tab with an
/// opener (the driver reports one only to the session whose tab opened it).
fn popup_of(result: &Value) -> Option<String> {
    result["events"]
        .as_array()?
        .iter()
        .find(|event| event["event"] == "new_tab" && event["opener"].is_string())
        .and_then(|event| event["tab"].as_str())
        .map(str::to_string)
}

/// A literal fill as `fill.rs` takes its arguments.
fn fill_args(literal: &Literal) -> Value {
    let fields: Vec<Value> = literal
        .fields
        .iter()
        .map(|(label, refs)| json!([label, refs]))
        .collect();
    json!({"item": literal.item, "fields": fields, "press": literal.press,
           "snapshot": literal.snapshot, "suggested": literal.suggestion})
}

/// A fill that releases a secret field stops here, before any value is
/// released, with a confirmation bound to its press button and the lines
/// around it, its exact mapping, the site of each frame a value would go
/// into, and the item's revision. Its confirmation fills, presses and
/// cleans up as one step.
pub async fn confirm_fill(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Failure {
    let press = args["press"].as_str().unwrap_or_default();
    let (mapping, frames) = match super::fill::preview(browsers, entry, args).await {
        Ok(previewed) => previewed,
        Err(failure) => return failure,
    };
    let revision = browsers
        .vault
        .list_if_present()
        .into_iter()
        .find(|meta| args["item"] == meta.item.as_str())
        .map(|meta| meta.revision);
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
    // Its values go only where they were confirmed to go.
    fill["confirmed"] = json!({"frames": frames, "revision": revision});
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
    shown["mapping"] = json!(mapping);
    shown["frames"] = json!(frames);
    if let Some(id) = args["suggestion"].as_str() {
        shown["suggestion"] = json!(id);
    }
    let mut failure = Failure::new(
        Exit::Denied,
        "confirm_needed",
        format!(
            "this fill releases a secret field into {}, and presses {:?}; nothing was released. If that mapping is what the task needs, confirm it with `latch browser do --confirm {id}`",
            frames.join(", "),
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
    if entry.runtime.filling.load(Ordering::SeqCst) {
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
    if entry.runtime.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let deadline = Instant::now() + Duration::from_secs_f64(within);
    let mut waited = poll(entry, &condition, deadline, audit).await?;
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
    fn a_passs_risk_holds_only_on_the_very_context_it_judged() {
        let reviewed = json!({"element": {"role": "button", "name": "Continue"},
                              "acts_on": null, "title": "Review your order",
                              "lines": ["Nothing is charged yet.", "Continue"]});
        let passed = Passed {
            p: 0.03,
            seen: json!({"target": {"role": "button", "name": "Continue", "row": null},
                         "lines_sha256": lines_sha256(&reviewed),
                         "title": "Review your order"}),
        };
        assert!(passed.holds(&reviewed, false));
        let changed = |key: &str, value: Value| {
            let mut now = reviewed.clone();
            now[key] = value;
            passed.holds(&now, false)
        };
        assert!(!changed(
            "element",
            json!({"role": "button", "name": "Place order"})
        ));
        assert!(!changed(
            "lines",
            json!(["Continuing places your order.", "Continue"])
        ));
        assert!(!changed("title", json!("Confirm and pay")));
        // An Enter is judged on what it submits, which this review lacks.
        assert!(!passed.holds(&reviewed, true));
    }

    #[test]
    fn request_ids_are_the_clients_own() {
        assert!(request_id(&json!({})).is_err());
        assert!(request_id(&json!({"request": "r 1"})).is_err());
        assert_eq!(request_id(&json!({"request": "r1a2"})).unwrap(), "r1a2");
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
