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

use super::{Checked, Done, Host, check, exec_denied, host_upgraded};
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
    let action = gate::gated(&request.op, &request.args).map(|gated| gated.audit);
    if let Some(action) = &action {
        audit.insert("action".into(), action.clone());
    }
    let answer = |verdict: &str| {
        let mut answer = json!({
            "verdict": verdict,
            "host": host.name,
            "as": shown_as,
            "request": {"op": request.op, "action": action},
        });
        if let Some(judged) = &judged {
            answer["gate"] = judged.clone();
        }
        answer
    };
    let host_refused = |failure: &Failure| {
        let mut answer = answer("host_refused");
        answer["refused"] = json!({"code": failure.code, "message": failure.message});
        answer
    };

    // In the order a real request meets them (`serve_request`).
    let Checked {
        gate,
        denied_exec,
        upgraded,
        exec_failure,
        running,
        unplanned,
        unplaced,
        judgment,
        refused: refusal,
        ..
    } = checked;
    let result = if upgraded {
        Err(Response::error(&host_upgraded()))
    } else if let Some(failure) = exec_failure {
        Err(Response::error(&failure))
    } else if let Some(Err(failure)) = &running {
        Ok(host_refused(failure))
    } else if let Some(done) = unplanned {
        Err(done.response)
    } else if let Some(judgment) = judgment {
        match judgment.verdict {
            Ok(()) => Ok(answer("allow")),
            // A refusal the judge makes is the preview's verdict; one it
            // cannot make (no chat chosen, an outage) is an error.
            Err(failure) if failure.exit == Exit::Denied => {
                let verdict = failure.details["reason"].as_str().unwrap_or(&failure.code);
                Ok(answer(verdict))
            }
            Err(failure) => Err(Response::error(&failure)),
        }
    } else if let Err(failure) = gate {
        Err(Response::error(&failure))
    } else if let Some(done) = refusal {
        Err(done.response)
    } else if let Some(done) = unplaced {
        Err(done.response)
    } else if denied_exec {
        Ok(host_refused(&exec_denied()))
    } else {
        Err(Response::error(&not_previewable(&request.op)))
    };
    (result, judged)
}

/// A request the gatekeeper does not judge, or browser steps, which it
/// judges against the page they act on.
fn not_previewable(op: &str) -> Failure {
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
