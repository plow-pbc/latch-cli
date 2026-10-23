//! The `rules_try` operation: the owner previews the verdict a request
//! would get on this host right now, as one of their agents (`from`) or as
//! themselves, without running it. The request meets every check a real
//! one meets (`super::check`: the host's own refusals, then the
//! gatekeeper) and stops there. Owner devices only, like `rules`.
//!
//! A refusal is a verdict, not an error: `not_covered`, `owner_said_no`,
//! `no_intent`, or `host_refused` for a refusal the host makes without
//! asking the judge (`--deny-exec`, a connector that is off). Its record
//! is a `rules_try` one, with the judgment's details, never the request's.

use serde_json::{Map, Value, json};

use super::{Done, Host, Stop, check, exec_denied};
use crate::failure::{Exit, Failure};
use crate::gate;
use crate::plow::{Identity, Permissions};
use crate::wire::{Request, Response};

/// The preview's answer, and the judgment's details for its record.
pub(super) async fn run(
    host: &Host,
    identity: &Identity,
    local: bool,
    args: &Value,
) -> (Done, Option<Value>) {
    let mut audit = Map::new();
    let (result, judged) = preview(host, identity, local, args, &mut audit).await;
    // The record says what the preview found: its verdict, and whether
    // that was a refusal (its decision).
    if let Ok(answer) = &result {
        audit.insert("verdict".into(), answer["verdict"].clone());
        if let Some(code) = answer["refused"]["code"].as_str() {
            audit.insert("refused".into(), json!(code));
        }
    }
    let done = match result {
        Ok(answer) => Done {
            outcome: answer["verdict"].as_str().unwrap_or_default().to_string(),
            response: Response::Ok(answer),
            audit_args: Value::Object(audit),
        },
        Err(response) => Done {
            outcome: match &response {
                Response::Error { code, .. } => code.clone(),
                Response::Ok(_) => String::new(),
            },
            response,
            audit_args: Value::Object(audit),
        },
    };
    (done, judged)
}

async fn preview(
    host: &Host,
    identity: &Identity,
    local: bool,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> (Result<Value, Response>, Option<Value>) {
    let refused = |failure: Failure| (Err(Response::error(&failure)), None);
    if let Err(failure) = gate::owner::owner_only(&identity.kind) {
        return refused(failure);
    }
    let Ok(mut request) = serde_json::from_value::<Request>(args["request"].clone()) else {
        let message = "The request to preview is not a latch request.".to_string();
        return refused(Failure::usage("bad_request", message));
    };
    audit.insert("op".into(), json!(request.op));
    if gate::gated(&request.op, &request.args).is_none() {
        return refused(not_previewable(&request.op));
    }

    // Judged as the agent `from` names, or as the owner who asks.
    let agent;
    let (judged_as, judged_local, shown_as) = match args["from"].as_str() {
        Some(from) => match gate::owner::agent(&host.plow, from).await {
            Ok((uid, name)) => {
                audit.insert("from".into(), json!(uid));
                let shown = json!({"agent_uid": uid, "agent_name": name});
                agent = Identity {
                    key: uid.clone(),
                    kind: "agent".into(),
                    name: name.unwrap_or_else(|| uid.clone()),
                    agent_uid: Some(uid),
                    permissions: Permissions { call: true },
                };
                (&agent, false, shown)
            }
            Err(failure) => return refused(failure),
        },
        None => (identity, local, json!("owner")),
    };

    let chat = request.chat.clone();
    let checked = check(host, judged_as, judged_local, chat.as_deref(), &mut request).await;
    let judged = checked
        .judgment
        .as_ref()
        .map(|judgment| judgment.gate.clone());
    // An exec the host refuses outright is refused before its paths are
    // checked; they are resolved here so its answer shows them as a judged
    // one's does.
    let exec_off = matches!(&checked.stop, Some(Stop::Host(done)) if done.outcome == "exec_denied");
    if exec_off && let Ok(prepared) = crate::exec::normalize(&host.workers, &request.args) {
        request.args = serde_json::to_value(&prepared).expect("prepared exec serializes");
    }
    // The answer shows the action as the judge was shown it; the record
    // keeps it as the audit keeps the real call's.
    let gated = gate::gated(&request.op, &request.args);
    let action = gated
        .as_ref()
        .map(|gated| judged_action(&request.op, gated));
    if let Some(gated) = gated {
        audit.insert("action".into(), gated.audit);
    }
    let answer = |verdict: &str| {
        // One shape whatever the verdict: no judgment is a null `gate`,
        // and a judgment with no rules has null `rules`.
        let mut gate = judged.clone().unwrap_or(Value::Null);
        if gate.is_object() && gate.get("rules").is_none() {
            gate["rules"] = Value::Null;
        }
        json!({
            "verdict": verdict,
            "host": host.name,
            "as": shown_as,
            "request": {"op": request.op, "action": action},
            "gate": gate,
        })
    };
    let host_refused = |code: &str, message: &str| {
        let mut answer = answer("host_refused");
        answer["refused"] = json!({"code": code, "message": message});
        answer
    };

    // A judgment is there only if nothing stopped the request before the
    // judge; the judge's own no is its verdict.
    let result = match (checked.judgment, checked.stop) {
        (Some(judgment), _) => match judgment.verdict {
            Ok(()) => Ok(answer("allow")),
            // A refusal the judge makes is the preview's verdict; one it
            // cannot make (no chat chosen, an outage) is an error.
            Err(failure) if failure.exit == Exit::Denied => {
                let verdict = failure.details["reason"].as_str().unwrap_or(&failure.code);
                Ok(answer(verdict))
            }
            Err(failure) => Err(Response::error(&failure)),
        },
        (None, Some(Stop::Host(_))) if exec_off => {
            let uid = judged_as.agent_uid.as_deref().unwrap_or_default();
            let off = exec_turned_off(host.deny_exec.every_agent, uid);
            Ok(host_refused(&off.code, &off.message))
        }
        (None, Some(Stop::Host(done))) => match done.response {
            Response::Error { code, message, .. } => Ok(host_refused(&code, &message)),
            ok => Err(ok),
        },
        (None, Some(Stop::Refused(done))) => Err(done.response),
        (None, None) => Err(Response::error(&not_previewable(&request.op))),
    };
    (result, judged)
}

/// What the judge was shown, masked as the audit keeps the same request:
/// an exec's stdin as its size and sha256, a messages or contacts plan
/// without a contacts page's position; secret values were never in it.
fn judged_action(op: &str, gated: &gate::Gated) -> Value {
    let mut action: Value = serde_json::from_str(gated.action.get()).expect("an action is JSON");
    if let Some(stdin) = action["stdin"].as_str() {
        action["stdin"] = crate::exec::stdin_audit(stdin.as_bytes());
    }
    if action.get("arguments").is_some() {
        action["arguments"] = crate::messages::host::kept_plan(op, &action["arguments"]);
    }
    action
}

/// `exec_denied` as the owner who turned exec off reads it.
fn exec_turned_off(every_agent: bool, uid: &str) -> Failure {
    let (whom, flag) = match every_agent {
        true => ("every agent", "--deny-exec".to_string()),
        false => ("this agent", format!("--deny-exec={uid}")),
    };
    let message = format!(
        "You turned exec off for {whom} on this host when you started it with \
         `latch serve {flag}`. Start it again without that flag to turn exec back on."
    );
    let mut failure = exec_denied();
    failure.message = message;
    failure
}

/// A request the gatekeeper does not judge, or browser steps, which it
/// judges against the page they act on.
pub(crate) fn not_previewable(op: &str) -> Failure {
    let instead = "Preview an exec, file, google, messages or contacts request instead.";
    let message = match op {
        "browser" => format!(
            "Browser steps can't be previewed: they are judged against the page they act on. {instead}"
        ),
        op => format!(
            "`{op}` is not judged by the gatekeeper, so there is nothing to preview. {instead}"
        ),
    };
    Failure::usage("not_previewable", message)
}
