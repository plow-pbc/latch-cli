//! A call as people read it: its kind and command, close to what was
//! typed; a title for what was asked; a result for how it went; and the
//! details behind them, from what the host already holds when the call
//! ends (the request, its audit details and record id, the answer, the
//! gatekeeper's record). Titles say what was requested, never a guess at
//! why.

use std::time::Duration;

use serde_json::Value;

use jiff::Timestamp;

use super::{Caller, Detail, Outcome, State, Step};
use crate::wire::Response;

/// A call as it ended, for `Dashboard::end`.
pub struct Finished {
    pub kind: String,
    pub command: String,
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

/// A call as requested, before it runs, for `Dashboard::begin`.
pub struct Begun {
    /// When it began.
    pub at: Timestamp,
    pub kind: String,
    pub command: String,
    pub title: String,
    /// A browser plan's steps, none reached yet.
    pub steps: Vec<Step>,
    /// The id a browser plan's client gave it.
    pub request: Option<String>,
}

/// A call `op` as requested, before it runs.
pub fn begun(op: &str, request: &Value) -> Begun {
    let (kind, command) = command(op, request, &Value::Null);
    Begun {
        at: Timestamp::now(),
        kind,
        command,
        title: title(op, request, &Value::Null),
        steps: plan_steps(request, &Value::Null),
        request: (op == "browser")
            .then(|| request["request"].as_str())
            .flatten()
            .map(str::to_string),
    }
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
    let (kind, command) = command(op, request, audit);
    let steps = plan_steps(request, audit);
    let (mut outcome, code, message) = match response {
        Response::Ok(_) => (done(op, audit, &steps), None, None),
        Response::Error {
            code,
            message,
            details,
            ..
        } => (
            failed(op, code, details, audit),
            Some(code.clone()),
            Some(message.clone()),
        ),
    };
    // A preview that worked says what it found, refusal or not.
    if op == "rules_try" && audit["verdict"].is_string() {
        outcome = preview_outcome(audit);
    }
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
        kind,
        command,
        title,
        outcome,
        steps,
        session: audit["session"].as_str().map(str::to_string),
        label: audit["task"].as_str().map(str::to_string),
        site,
    }
}

/// A call the audit log recorded, as it ended, by whom and when: what a
/// host's board starts with. `host` is this host's own key, the caller
/// of the owner's calls on its socket. The log keeps no request, so a call
/// is what its audit details say of it; a refusal's code says why.
pub fn recorded(
    record: &crate::audit::Record,
    host: &str,
) -> Option<(Caller, Timestamp, Finished)> {
    let at = record
        .started
        .as_ref()
        .unwrap_or(&record.time)
        .parse()
        .ok()?;
    let key: String = record.caller.chars().take(8).collect();
    let caller = if record.caller == host {
        Caller {
            label: "you (local)".to_string(),
            detail: "on this host's own socket".to_string(),
        }
    } else if let Some(agent) = &record.agent {
        Caller {
            label: format!("agent {key}"),
            detail: format!("agent {agent}, key {}", record.caller),
        }
    } else {
        Caller {
            label: "you (device)".to_string(),
            detail: format!("key {}", record.caller),
        }
    };
    let response = match record.outcome.as_str() {
        "ok" => Response::Ok(Value::Null),
        code => Response::Error {
            exit: crate::failure::Exit::Failed,
            code: code.to_string(),
            message: String::new(),
            details: Value::Null,
        },
    };
    let mut finished = finished(
        &record.op,
        &Value::Null,
        &record.args,
        &response,
        record.gate.as_ref(),
        Some(&record.hash),
        Duration::ZERO,
    );
    finished.detail.took_ms = None;
    finished.detail.message = None;
    Some((caller, at, finished))
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

/// What kind of call `op` is (`exec`, `browser`, `google`, …) and its
/// command, close to what was typed after `latch <kind>`: from the request
/// as sent, else what the audit kept of it. Never a query, a search or the
/// text a step types, and credential-looking words hidden.
fn command(op: &str, request: &Value, audit: &Value) -> (String, String) {
    let pick = |key: &str| -> Value {
        if request[key].is_null() {
            audit[key].clone()
        } else {
            request[key].clone()
        }
    };
    let words = |value: Value| -> Vec<String> { serde_json::from_value(value).unwrap_or_default() };
    let mut line: Vec<String> = Vec::new();
    let kind = match op {
        "rules_try" => {
            let (previewed, asked, kept) = previewed(request, audit);
            line.push("try".into());
            if let Some(from) = request["from"].as_str().or(audit["from"].as_str()) {
                line.extend(["--from".into(), quoted(from)]);
            }
            let (kind, words) = command(&previewed, &asked, &kept);
            line.extend(["--".into(), kind, words]);
            "rules"
        }
        "exec" => {
            let writes = words(pick("writes"));
            // The folder it runs in by default (its first write folder,
            // else home) goes unsaid, as it is when typed.
            if let Some(dir) = pick("workdir").as_str() {
                let home = std::env::home_dir();
                let real = home.as_ref().and_then(|home| home.canonicalize().ok());
                let default = match writes.first() {
                    Some(first) => [Some(std::path::PathBuf::from(first)), None],
                    None => [home, real],
                };
                if !default
                    .iter()
                    .flatten()
                    .any(|path| path == std::path::Path::new(dir))
                {
                    line.extend(["--workdir".into(), quoted(dir)]);
                }
            }
            for path in writes {
                line.extend(["--write".into(), quoted(&path)]);
            }
            if pick("network") == true {
                line.push("--network".into());
            }
            // A secret by its name only: the request holds its value, the
            // audit its length; neither is shown.
            let secrets = pick("secret_env");
            let names: Vec<&str> = match &secrets {
                Value::Object(secrets) => secrets.keys().map(String::as_str).collect(),
                Value::Array(secrets) => secrets
                    .iter()
                    .filter_map(|secret| secret["name"].as_str())
                    .collect(),
                _ => Vec::new(),
            };
            for name in names {
                line.extend(["--secret-env".into(), format!("{}=•••", quoted(name))]);
            }
            if let Some(timeout) = pick("timeout").as_u64()
                && timeout != 60
            {
                line.extend(["--timeout".into(), timeout.to_string()]);
            }
            if !pick("stdin").is_null() {
                line.push("-i".into());
            }
            let argv = crate::redact::argv(&words(pick("argv")));
            line.extend(argv.iter().map(|word| quoted(word)));
            "exec"
        }
        "file_get" | "file_put" => {
            let verb = if op == "file_get" { "get" } else { "put" };
            line.extend([verb.into(), quoted(&pick_path(request, audit))]);
            "file"
        }
        "messages" | "contacts" => {
            let action = pick("action");
            let action = action.as_str().unwrap_or_default();
            line.push(action.trim_start_matches(&format!("{op}.")).to_string());
            op
        }
        "google" => {
            let args = words(request["gog_args"].clone());
            for account in words(request["account"].clone()) {
                line.extend(["--account".into(), quoted(&account)]);
            }
            let prefix = match audit["command"].as_str() {
                Some(prefix) => prefix.to_string(),
                None => crate::google::policy::command_prefix(&args),
            };
            line.push(prefix.replace('.', " "));
            "google"
        }
        "browser" => {
            let command = pick("command");
            let command = command.as_str().unwrap_or_default();
            line.push(command.to_string());
            match command {
                "open" | "goto" => {
                    let url = pick("url");
                    let place = match url.as_str().and_then(|url| url::Url::parse(url).ok()) {
                        Some(url) => {
                            format!("{}{}", url.origin().ascii_serialization(), url.path())
                        }
                        None => format!(
                            "{}{}",
                            audit["origin"].as_str().unwrap_or_default(),
                            audit["path"].as_str().unwrap_or_default()
                        ),
                    };
                    line.push(place);
                    if let Some(task) = pick("task").as_str() {
                        line.extend(["--task".into(), quoted(task)]);
                    }
                }
                "do" | "act" => {
                    if let Some(confirm) = pick("confirm").as_str() {
                        line.extend(["--confirm".into(), quoted(confirm)]);
                    }
                    let plan = pick("plan");
                    for step in plan["steps"].as_array().into_iter().flatten() {
                        line.push(quoted(&step_words(step)));
                    }
                }
                _ => {}
            }
            "browser"
        }
        "vault" | "rules" | "plugins" => {
            for key in ["command", "name"] {
                if let Some(word) = pick(key).as_str() {
                    line.push(word.to_string());
                }
            }
            op
        }
        "vault_link" => {
            line.push(pick("op").as_str().unwrap_or_default().to_string());
            "phone link"
        }
        op => return (op.replace('_', " "), String::new()),
    };
    let line: Vec<String> = line.into_iter().filter(|word| !word.is_empty()).collect();
    (kind.to_string(), line.join(" "))
}

/// `word` as a shell reads it back.
fn quoted(word: &str) -> String {
    crate::login::shell_quoted(word)
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
        "rules_try" => {
            let (previewed, asked, kept) = previewed(request, audit);
            format!("Preview: {}", title(&previewed, &asked, &kept))
        }
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
            match audit["command"].as_str() {
                Some(command) if words.is_empty() => format!("Google: {command}"),
                _ => format!("Google: {}", crate::google::policy::command_prefix(&words)),
            }
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

/// A preview's request as a call is read: its operation, and what the
/// record kept of it (its paths resolved) or, before there is a record,
/// the request as the owner sent it.
fn previewed(request: &Value, audit: &Value) -> (String, Value, Value) {
    let op = request["request"]["op"]
        .as_str()
        .or(audit["op"].as_str())
        .unwrap_or_default();
    let kept = audit["action"].clone();
    let asked = match kept.is_null() {
        true => request["request"]["args"].clone(),
        false => Value::Null,
    };
    (op.to_string(), asked, kept)
}

/// What a preview found: the request would be allowed, or refused and why,
/// in the words a refused call's row uses.
fn preview_outcome(audit: &Value) -> Outcome {
    let verdict = audit["verdict"].as_str().unwrap_or_default();
    if verdict == "allow" {
        return Outcome::new(State::Done, Some("would be allowed".into()));
    }
    let code = match verdict {
        "owner_said_no" | "not_covered" => "gatekeeper_denied",
        "host_refused" => audit["refused"].as_str().unwrap_or_default(),
        verdict => verdict,
    };
    let why = refusal(code, &serde_json::json!({"reason": verdict}))
        .and_then(|refused| refused.text)
        .unwrap_or_else(|| verdict.replace('_', " "));
    Outcome::new(State::Done, Some(format!("would be refused: {why}")))
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
fn failed(op: &str, code: &str, details: &Value, audit: &Value) -> Outcome {
    // A plan that stopped part way, a refused fill too: where, and how much
    // ran first, as its audit keeps them; the step itself says why.
    if op == "browser"
        && let Some(at) = audit["stop"]["step"].as_u64()
        && let Some(steps) = audit["step_outcomes"].as_array()
        && steps.len() > 1
    {
        let ran = steps
            .iter()
            .filter_map(step_outcome)
            .filter(|outcome| outcome.state == State::Done)
            .count();
        return Outcome::new(State::Stopped, Some(format!("step {at}; {ran} ran")));
    }
    if let Some(refused) = refusal(code, details) {
        return refused;
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
