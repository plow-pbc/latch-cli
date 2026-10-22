//! A call as people read it: a title for what was asked, a result for how
//! it went, and the details behind them, from what the host already holds
//! when the call ends (the request, its audit details and record id, the
//! answer, the gatekeeper's record). Titles say what was requested, never
//! a guess at why.

use std::time::Duration;

use serde_json::Value;

use super::{Detail, Outcome, State, Step};
use crate::wire::Response;

/// A call as it ended, for `Dashboard::end`.
pub struct Finished {
    pub title: String,
    pub outcome: Outcome,
    pub steps: Vec<Step>,
    pub detail: Detail,
    /// The browser session it belongs to (its id's hash), once known.
    pub session: Option<String>,
    /// The session's task label (`browser open --task`).
    pub label: Option<String>,
    /// The site the page was on.
    pub site: Option<String>,
}

/// A call `op` as requested, before it runs: its title and, for a browser
/// plan, its steps (none reached yet).
pub fn begun(op: &str, request: &Value) -> (String, Vec<Step>) {
    (
        title(op, request, &Value::Null),
        plan_steps(request, &Value::Null),
    )
}

/// The browser session a request names, as the audit records it.
pub fn session(op: &str, request: &Value) -> Option<String> {
    (op == "browser")
        .then(|| request["session"].as_str())
        .flatten()
        .map(crate::browser::host::hash_of)
}

/// The call `op` as it ended: `request` as the caller sent it, `audit` the
/// details its record keeps, `response` the answer, `gate` the
/// gatekeeper's record and `audit_id` the record's id.
pub fn finished(
    op: &str,
    request: &Value,
    audit: &Value,
    response: &Response,
    gate: Option<&Value>,
    audit_id: Option<&str>,
    took: Duration,
) -> Finished {
    let title = title(op, request, audit);
    let steps = plan_steps(request, audit);
    let (outcome, code, message) = match response {
        Response::Ok(_) => (done(op, audit, &steps), None, None),
        Response::Error {
            code,
            message,
            details,
            ..
        } => (
            failed(op, code, details, audit, &steps),
            Some(code.clone()),
            Some(message.clone()),
        ),
    };
    // A plan that stopped answers with the page as it ended, where it
    // stopped; else the page the audit names. Never its query.
    let ended = match response {
        Response::Error { details, .. } => details["page"]["url"].as_str(),
        Response::Ok(_) => None,
    };
    let page = match (
        ended.and_then(|url| url::Url::parse(url).ok()),
        audit["origin"].as_str(),
    ) {
        (Some(url), _) => Some(format!(
            "{}{}",
            url.origin().ascii_serialization(),
            url.path()
        )),
        (None, Some(origin)) => Some(format!(
            "{origin}{}",
            audit["path"].as_str().unwrap_or_default()
        )),
        _ => None,
    };
    let site = audit["origin"]
        .as_str()
        .and_then(|origin| url::Url::parse(origin).ok())
        .and_then(|url| url.host_str().map(str::to_string));
    Finished {
        detail: Detail {
            action: title.clone(),
            page,
            took_ms: Some(took.as_millis() as u64),
            code,
            message,
            audit_id: audit_id.map(str::to_string),
            // The browser judges its own steps: its record is in the audit's.
            gate: gate
                .or(audit.get("gate").filter(|gate| gate.is_object()))
                .cloned(),
            request: crate::redact::shown(op, audit),
        },
        title,
        outcome,
        steps,
        session: audit["session"].as_str().map(str::to_string),
        label: audit["task"].as_str().map(str::to_string),
        site,
    }
}

/// A call refused before it could run: its caller not let in.
pub fn refused(op: &str, code: &str, message: &str) -> Finished {
    let response = Response::Error {
        exit: crate::failure::Exit::Denied,
        code: code.to_string(),
        message: message.to_string(),
        details: Value::Null,
    };
    finished(
        op,
        &Value::Null,
        &Value::Null,
        &response,
        None,
        None,
        Duration::ZERO,
    )
}

/// What was asked: `Run: cargo test`, `Read: notes.txt`, `Search
/// Messages`, a browser plan's steps. From the audit's details where they
/// hold it, else the request.
fn title(op: &str, request: &Value, audit: &Value) -> String {
    let pick = |key: &str| {
        audit[key]
            .as_str()
            .or_else(|| request[key].as_str())
            .unwrap_or_default()
            .to_string()
    };
    match op {
        "exec" => {
            let argv = if audit["argv"].is_array() {
                audit
            } else {
                request
            };
            let shown = crate::redact::shown("exec", argv);
            let words: Vec<String> =
                serde_json::from_value(shown["argv"].clone()).unwrap_or_default();
            format!("Run: {}", words.join(" "))
        }
        "file_get" => format!("Read: {}", pick_path(request, audit)),
        "file_put" => format!("Write: {}", pick_path(request, audit)),
        "messages" | "contacts" => {
            let action = request["action"].as_str().unwrap_or_default();
            match (op, action) {
                ("messages", "search") => "Search Messages".to_string(),
                ("messages", "thread") => "Read a Messages thread".to_string(),
                ("contacts", "search") => "Search Contacts".to_string(),
                (op, action) => format!("{}: {action}", capitalized(op)),
            }
        }
        // Only the grammar's own words, never a query or body.
        "google" => {
            let words: Vec<String> =
                serde_json::from_value(request["gog_args"].clone()).unwrap_or_default();
            format!("Google: {}", crate::google::policy::command_prefix(&words))
        }
        "browser" => browser_title(request, audit),
        "vault_link" => format!("Phone link: {}", pick("op")),
        "vault" | "rules" | "plugins" => {
            let command = pick("command");
            let name = request["name"]
                .as_str()
                .map(|name| format!(" {name}"))
                .unwrap_or_default();
            format!("{}: {command}{name}", capitalized(op))
        }
        op => capitalized(&op.replace('_', " ")),
    }
}

fn pick_path(request: &Value, audit: &Value) -> String {
    audit["path"]
        .as_str()
        .or_else(|| request["remote_path"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn browser_title(request: &Value, audit: &Value) -> String {
    let command = request["command"].as_str().unwrap_or_default();
    let steps = plan_steps(request, audit);
    match command {
        "open" | "goto" => {
            let url = request["url"].as_str().unwrap_or_default();
            let place = url::Url::parse(url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| url.to_string());
            format!("{} {place}", capitalized(command))
                .trim()
                .to_string()
        }
        "do" | "act" if !steps.is_empty() => {
            let titles: Vec<&str> = steps.iter().map(|step| step.title.as_str()).collect();
            match titles.as_slice() {
                [one] => one.to_string(),
                more => format!("Plan: {}", more.join("; ")),
            }
        }
        command => capitalized(command),
    }
}

/// A browser plan's steps in order: each one's words as the caller sent
/// them, and how far it got, from the audit's step outcomes.
fn plan_steps(request: &Value, audit: &Value) -> Vec<Step> {
    let mut titles: Vec<String> = Vec::new();
    if let Some(confirm) = request["confirm"].as_str() {
        titles.push(format!("Confirm {confirm}"));
    }
    for step in request["plan"]["steps"].as_array().into_iter().flatten() {
        titles.push(step_words(step));
    }
    let outcomes: Vec<&Value> = audit["step_outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    titles
        .into_iter()
        .enumerate()
        .map(|(n, title)| Step {
            title,
            outcome: outcomes.get(n).and_then(|outcome| step_outcome(outcome)),
        })
        .collect()
}

/// A plan step's words: its goal, or its literal action.
fn step_words(step: &Value) -> String {
    if let Some(goal) = step["goal"].as_str() {
        return goal.to_string();
    }
    let literal = &step["literal"];
    let mut words = vec![literal["op"].as_str().unwrap_or_default().to_string()];
    for key in ["ref", "item"] {
        if let Some(word) = literal[key].as_str() {
            words.push(word.to_string());
        }
    }
    // Never the text it types: it may be a code or a secret, which the
    // audit keeps only as a length and a hash.
    if let Some(check) = step["check"]["value"].as_str() {
        // A sentence is its own words; a typed check says its kind.
        match step["check"]["kind"].as_str() {
            Some("sentence") | None => words.push(format!("-> {check}")),
            Some(kind) => words.push(format!("-> {kind}: {check}")),
        }
    }
    words.join(" ")
}

pub(super) fn step_outcome(outcome: &Value) -> Option<Outcome> {
    match outcome["outcome"].as_str()? {
        "not_run" => None,
        "stopped" => {
            let code = outcome["code"].as_str().unwrap_or_default();
            Some(
                refusal(code, &Value::Null)
                    .unwrap_or_else(|| Outcome::new(State::Stopped, Some(stop_words(code)))),
            )
        }
        _ => Some(Outcome::new(State::Done, None)),
    }
}

/// How a call that was answered went.
fn done(op: &str, audit: &Value, steps: &[Step]) -> Outcome {
    match op {
        "exec" => {
            if audit["timed_out"] == true {
                return Outcome::new(State::Failed, Some("timed out".into()));
            }
            if let Some(signal) = audit["signal"].as_str() {
                return Outcome::new(State::Failed, Some(format!("killed by {signal}")));
            }
            match audit["exit_code"].as_i64() {
                Some(0) => Outcome::new(State::Done, Some("exit 0".into())),
                Some(code) => Outcome::new(State::Failed, Some(format!("exit {code}"))),
                None => Outcome::new(State::Done, None),
            }
        }
        "file_get" | "file_put" => {
            Outcome::new(State::Done, audit["size"].as_u64().map(human_size))
        }
        "messages" | "contacts" => Outcome::new(
            State::Done,
            audit["result"]["count"]
                .as_u64()
                .map(|count| format!("{count} {}", plural(count, "match", "matches"))),
        ),
        "browser" if steps.len() > 1 => {
            Outcome::new(State::Done, Some(format!("{} steps", steps.len())))
        }
        _ => Outcome::new(State::Done, None),
    }
}

/// How a call that was not answered went: refused, stopped or failed.
fn failed(op: &str, code: &str, details: &Value, audit: &Value, steps: &[Step]) -> Outcome {
    if let Some(refused) = refusal(code, details) {
        return refused;
    }
    // A plan that stopped part way: where, and how much ran first; the
    // step itself says why.
    if op == "browser"
        && let Some(at) = audit["stop"]["step"].as_u64()
        && steps.len() > 1
    {
        let ran = steps
            .iter()
            .filter(|step| {
                step.outcome
                    .as_ref()
                    .is_some_and(|o| o.state == State::Done)
            })
            .count();
        let state = steps
            .get(at as usize - 1)
            .and_then(|step| step.outcome.as_ref())
            .map_or(State::Stopped, |outcome| outcome.state);
        return Outcome::new(state, Some(format!("step {at}; {ran} ran")));
    }
    if op == "browser" && (STOPS.iter().any(|(stop, _)| *stop == code) || !audit["stop"].is_null())
    {
        return Outcome::new(State::Stopped, Some(stop_words(code)));
    }
    Outcome::new(State::Failed, Some(code.replace('_', " ")))
}

/// A refusal: the gatekeeper's, or the host owner's standing settings. No
/// answer from the judge is said apart from a no.
fn refusal(code: &str, details: &Value) -> Option<Outcome> {
    let why = match code {
        "gatekeeper_denied" => match details["reason"].as_str() {
            Some("owner_said_no") => "owner said no",
            _ => "outside your instructions",
        },
        "no_intent" => "no instructions from the owner",
        "gatekeeper_unavailable" => "judge unavailable",
        "exec_denied" => "exec is off for this agent",
        "not_enabled" => "connector not enabled",
        "caller_denied" => "caller not allowed",
        "owner_only" => "the owner's only",
        _ => return None,
    };
    Some(Outcome::new(State::Refused, Some(why.to_string())))
}

/// What a browser safety stop means, in words.
const STOPS: &[(&str, &str)] = &[
    ("covered_target", "target hidden, blocked or moving"),
    ("stale_snapshot", "page changed; observe again"),
    ("ambiguous_target", "target unclear"),
    ("repeat", "repeated action on unchanged page"),
    ("confirm_needed", "needs your confirmation"),
    ("model_unavailable", "decision model unavailable"),
    ("check_not_met", "check did not hold"),
    ("wrong_target", "target is not what the step names"),
];

fn stop_words(code: &str) -> String {
    STOPS
        .iter()
        .find(|(stop, _)| *stop == code)
        .map_or_else(|| code.replace('_', " "), |(_, words)| words.to_string())
}

pub(super) fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn plural<'a>(count: u64, one: &'a str, more: &'a str) -> &'a str {
    if count == 1 { one } else { more }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.0),
    }
}
