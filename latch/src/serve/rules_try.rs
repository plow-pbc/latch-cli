//! The `rules_try` operation: the owner previews the verdict a request
//! would get on this host right now, as one of their agents (`from`) or as
//! themselves, without running it. The request meets every check a real
//! one meets (`super::check`: the host's own refusals, then the
//! gatekeeper) and stops there. Owner devices only, like `rules`.
//!
//! An agent is previewed by its rules layer alone (`Caller::RulesLayer`):
//! the built-in policy and the rules, with no chat. A `draft` stands in
//! for the saved rules for every agent, and is never saved. With
//! `new_agent`, the request is judged as a new agent's, with no rules of
//! its own, whatever the host's connectors and `--deny-exec` would refuse.
//! On this machine the preview is judged in the previewing process
//! (`preview_here`), so it needs no running host.
//!
//! A `fill` in place of the request previews a vault login fill
//! (`previewed_fill`): the judgment the agent's fill would get, built from
//! the item's kind and title as given. Only the judge is asked: no browser
//! is started and the vault is never opened. The site's own judgment and
//! the host's checks on the fill are not previewed.
//!
//! A refusal is a verdict, not an error: `not_covered`, `owner_said_no`,
//! or `host_refused` for a refusal the host makes without asking the judge
//! (`--deny-exec`, a connector that is off, a path it never lets a worker
//! use). A preview runs nothing, so it is not audited and not shown on the
//! dashboard.

use serde_json::{Value, json};

use super::{Binary, Checks, DenyExec, Stop, check, exec_denied};
use crate::failure::{Exit, Failure, code};
use crate::gate::rules::RuleFiles;
use crate::gate::{self, Caller, Gate, Gatekeeper};
use crate::plow::{Identity, Permissions, Plow};
use crate::state::StateDir;
use crate::wire::{Request, Response};

/// A preview on this machine, judged here as its host would judge it: with
/// its login, its rules, its connectors and the `--deny-exec` its host runs
/// or last ran with. The same whether or not its host is running.
pub(crate) async fn preview_here(state: &StateDir, args: &Value) -> Result<Value, Failure> {
    let config = state.require_login()?;
    let plow = Plow::from_state(state)?;
    let provider = Gatekeeper::provider_for_host(&state.path)
        .map_err(|why| Failure::new(code::not_logged_in, why))?;
    let gatekeeper = Gatekeeper::new(
        plow.clone(),
        provider,
        Box::new(RuleFiles::new(&state.path)?),
    );
    let state_dir = state
        .path
        .canonicalize()
        .map_err(|err| Failure::new(code::state_dir, format!("{}: {err}", state.path.display())))?;
    let workers = super::workers(state_dir)?;
    let binary = Binary::at(&workers.program)?;
    let home = std::env::home_dir().unwrap_or_default();
    let plugins = crate::plugins::Plugins::load(state, &home)?;
    let deny_exec = crate::managed::saved_deny_exec(&state.path).unwrap_or_default();
    let checks = Checks {
        name: &config.peer.name,
        plow: &plow,
        workers: &workers,
        gatekeeper: &gatekeeper,
        deny_exec: &deny_exec,
        binary: &binary,
        plugins: Some(&plugins),
    };
    // Who asks, as a host's own socket takes its caller: this login.
    let identity = Identity {
        key: config.peer.key.clone(),
        kind: config.peer.kind.clone(),
        name: config.peer.name.clone(),
        agent_uid: None,
        permissions: Permissions { call: true },
    };
    run(&checks, &identity, true, args).await.into_result()
}

/// The preview's answer: what it found, or why it could not preview.
pub(super) async fn run(
    host: &Checks<'_>,
    identity: &Identity,
    local: bool,
    args: &Value,
) -> Response {
    match preview(host, identity, local, args).await {
        Ok(answer) => Response::Ok(answer),
        Err(response) => response,
    }
}

async fn preview(
    host: &Checks<'_>,
    identity: &Identity,
    local: bool,
    args: &Value,
) -> Result<Value, Response> {
    let refused = |failure: Failure| Err(Response::error(&failure));
    if let Err(failure) = gate::owner::owner_only(&identity.kind, "the rules") {
        return refused(failure);
    }
    let fill = match &args["fill"] {
        Value::Null => None,
        fill => match login_fill(fill) {
            Ok(fill) => Some(fill),
            Err(failure) => return refused(failure),
        },
    };
    let request = match fill {
        Some(_) => None,
        None => {
            let Ok(request) = serde_json::from_value::<Request>(args["request"].clone()) else {
                let message = "The request to preview is not a latch request.".to_string();
                return refused(Failure::new(code::bad_request, message));
            };
            if gate::gated(&request.op, &request.args).is_none() {
                return refused(not_previewable(&request.op));
            }
            Some(request)
        }
    };

    // A blank draft is no rules for every agent, as `rules clear` leaves.
    let draft = args["draft"]
        .as_str()
        .map(|text| Some(text.to_string()).filter(|text| !text.trim().is_empty()));
    let new_agent = args["new_agent"] == true;

    // Judged as the agent `from` names, as a new agent, or as the owner
    // who asks.
    let agent;
    let (judged_as, judged_local, shown_as) = match (args["from"].as_str(), new_agent) {
        (Some(_), true) => {
            let message = "Judge as one agent (from) or as a new agent, not both.".to_string();
            return refused(Failure::new(code::bad_request, message));
        }
        (Some(from), false) => match gate::owner::agent(host.plow, from).await {
            Ok((uid, name)) => {
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
        (None, true) => (identity, local, json!("new_agent")),
        (None, false) => (identity, local, json!("owner")),
    };

    let chat = request.as_ref().and_then(|request| request.chat.clone());
    let none_denied = DenyExec::default();
    let rules_alone;
    let host = match new_agent {
        false => host,
        true => {
            rules_alone = Checks {
                deny_exec: &none_denied,
                plugins: None,
                ..*host
            };
            &rules_alone
        }
    };
    // An agent, named or new, is previewed by its rules layer alone: what
    // it may do whatever its chat says.
    let gate = match (new_agent || args["from"].is_string(), &chat) {
        (false, _) => host.gate(judged_as, judged_local, chat.as_deref()),
        (true, Some(_)) => Err(Failure::new(
            code::plow_chat_not_supported,
            "A preview judges an agent by its rules alone, with no Plow chat; omit --plow-chat and LATCH_PLOW_CHAT".into(),
        )),
        (true, None) => Ok(Gate {
            keeper: host.gatekeeper,
            caller: Caller::RulesLayer {
                agent: judged_as.agent_uid.as_deref(),
            },
        }),
    };
    let drafted;
    let gate = match (gate, draft) {
        (Ok(gate), Some(draft)) => match gate.keeper.drafted(gate.caller, draft) {
            Ok(keeper) => {
                drafted = keeper;
                Ok(Gate {
                    keeper: &drafted,
                    caller: gate.caller,
                })
            }
            Err(failure) => return refused(failure),
        },
        (gate, _) => gate,
    };
    let shape = |op: &str, action: Option<Value>, judged: Option<Value>, verdict: &str| {
        answer_shape(host.name, &shown_as, op, action, judged, verdict)
    };
    // A fill is only judged: no host operation runs, so there is nothing
    // to check before the judge.
    let Some(mut request) = request else {
        let fill = fill.expect("a fill when there is no request");
        let judgment = match gate {
            Ok(gate) => gate.judge(&fill).await,
            Err(failure) => return refused(failure),
        };
        let action = serde_json::to_value(&fill).expect("an action serializes");
        let verdict = verdict(judgment.verdict)?;
        return Ok(shape(
            "browser",
            Some(action),
            Some(judgment.gate),
            &verdict,
        ));
    };
    let checked = check(host, judged_as, gate, chat.as_deref(), &mut request).await;
    let judged = checked
        .judgment
        .as_ref()
        .map(|judgment| judgment.gate.clone());
    // An exec the host refuses outright is refused before its paths are
    // checked; they are resolved here so its answer shows them as a judged
    // one's does.
    let exec_off = matches!(&checked.stop, Some(Stop::Host(done)) if done.outcome == "exec_denied");
    if exec_off && let Ok(prepared) = crate::exec::normalize(host.workers, &request.args) {
        request.args = serde_json::to_value(&prepared).expect("prepared exec serializes");
    }
    // The action as the judge was shown it.
    let action =
        gate::gated(&request.op, &request.args).map(|gated| judged_action(&request.op, &gated));
    let answer = |verdict: &str| shape(&request.op, action.clone(), judged.clone(), verdict);
    let host_refused = |code: &str, message: &str| {
        let mut answer = answer("host_refused");
        answer["refused"] = json!({"code": code, "message": message});
        answer
    };

    // A judgment is there only if nothing stopped the request before the
    // judge; the judge's own no is its verdict.
    match (checked.judgment, checked.stop) {
        (Some(judgment), _) => Ok(answer(&verdict(judgment.verdict)?)),
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
    }
}

/// The preview's answer, one shape whatever the verdict: no judgment is a
/// null `gate`, and a judgment with no rules has null `rules`.
fn answer_shape(
    host: &str,
    shown_as: &Value,
    op: &str,
    action: Option<Value>,
    judged: Option<Value>,
    verdict: &str,
) -> Value {
    let mut gate = judged.unwrap_or(Value::Null);
    if gate.is_object() && gate.get("rules").is_none() {
        gate["rules"] = Value::Null;
    }
    json!({
        "verdict": verdict,
        "host": host,
        "as": shown_as,
        "request": {"op": op, "action": action},
        "gate": gate,
    })
}

/// A judgment as the preview's verdict: a refusal the judge makes is one;
/// one it cannot make (no chat chosen, an outage) is an error.
fn verdict(judged: Result<(), Failure>) -> Result<String, Response> {
    match judged {
        Ok(()) => Ok("allow".into()),
        Err(failure) if failure.exit == Exit::Denied => Ok(failure.details["reason"]
            .as_str()
            .unwrap_or(&failure.code)
            .to_string()),
        Err(failure) => Err(Response::error(&failure)),
    }
}

/// The vault fill a `fill` previews: a login, on a URL with a site, into
/// fields named by their labels.
fn login_fill(fill: &Value) -> Result<impl serde::Serialize + use<>, Failure> {
    let bad = |message: &str| Err(Failure::new(code::bad_request, message.to_string()));
    let url = fill["url"].as_str().unwrap_or_default();
    if crate::vault::sites::site_of(url).is_none() {
        return bad(
            "The fill's URL has no site to fill on: give a web address like https://www.instacart.com/login.",
        );
    }
    if fill["item"]["kind"] != "login" {
        return bad("Only a login fill can be previewed.");
    }
    let Some(title) = fill["item"]["title"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
    else {
        return bad("The fill names no login: give its title.");
    };
    let fields: Option<Vec<String>> = fill["fields"].as_array().and_then(|fields| {
        fields
            .iter()
            .map(|field| {
                field
                    .as_str()
                    .filter(|f| !f.trim().is_empty())
                    .map(str::to_string)
            })
            .collect()
    });
    let Some(fields) = fields.filter(|fields| !fields.is_empty()) else {
        return bad("The fill names no fields: give each field's label, like username.");
    };
    Ok(crate::browser::previewed_fill(url, "login", title, fields))
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
        "browser" => "Browser steps can't be previewed: they are judged against the page they \
                      act on. Preview a login fill with --fill-login TITLE --url URL, or an \
                      exec, file, google, messages or contacts request instead."
            .to_string(),
        op => format!(
            "`{op}` is not judged by the gatekeeper, so there is nothing to preview. {instead}"
        ),
    };
    Failure::new(code::not_previewable, message)
}
