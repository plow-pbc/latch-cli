//! `latch rules …` and `latch agents map`: the owner's commands on a
//! host's device rule and agent map. With another `--host` they go over
//! latch's connection, as the `rules` operation; a failed call never falls
//! back to a local file. Without it, or naming this peer, they act
//! locally, audited in its log as the host would. `rules try` judges on
//! this machine only.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::swarm::SwarmReader;
use super::{Gatekeeper, owner};
use crate::audit::{self, AuditLog};
use crate::cli::{RulesArgs, RulesCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Failure, code};
use crate::hosts;
use crate::plow::Plow;
use crate::state::StateDir;
use crate::wire::Request;

pub async fn run(state: &StateDir, args: RulesArgs) -> Result<Value, Failure> {
    let scope = match &args.command {
        RulesCommand::Set { scope, .. }
        | RulesCommand::Edit { scope }
        | RulesCommand::Clear { scope, .. }
        | RulesCommand::Show { scope } => scope,
        RulesCommand::Try { draft, actions } => return preview(state, draft, actions).await,
    };
    // `--host default` read once: an edit's save goes where its show went.
    let target = Target {
        state,
        host: hosts::pinned(state, scope.host.clone())?,
    };
    match args.command {
        RulesCommand::Set {
            text,
            file,
            revision,
            ..
        } => {
            let text = match (text, file) {
                (Some(text), None) => text,
                (None, Some(file)) => read_file(&file)?,
                _ => {
                    let message = "give the device rule as TEXT or --file F (- reads stdin)";
                    return Err(Failure::new(code::bad_arguments, message.into()));
                }
            };
            let args = match revision {
                None => json!({"command": "set", "text": text}),
                Some(revision) => {
                    json!({"command": "edit", "text": text, "revision": expected(&revision)})
                }
            };
            target.call(args).await
        }
        RulesCommand::Edit { .. } => {
            // It opens the person's editor: a program sets the rule instead.
            if !crate::term::interactive() {
                let message = "`latch rules edit` opens your editor, so it needs a terminal";
                let mut failure = Failure::new(code::no_terminal, message.into());
                failure.next = Some("latch rules set TEXT".into());
                return Err(failure);
            }
            let shown = target.call(json!({"command": "show"})).await?;
            let current = &shown["device_rule"];
            // From the editor on, what goes wrong is the person's to read.
            let text =
                edited(current["text"].as_str().unwrap_or_default()).map_err(Failure::told)?;
            target
                .call(json!({"command": "edit", "text": text, "revision": current["revision"]}))
                .await
                .map_err(Failure::told)
        }
        RulesCommand::Clear { revision, .. } => {
            let args = match revision {
                None => json!({"command": "clear"}),
                Some(revision) => {
                    json!({"command": "clear_at_revision", "revision": expected(&revision)})
                }
            };
            target.call(args).await
        }
        RulesCommand::Show { .. } => target.call(json!({"command": "show"})).await,
        RulesCommand::Try { .. } => unreachable!("previewed above"),
    }
}

/// `latch rules try`: whether the draft denies each action, judged on
/// this machine as the owner's own call.
async fn preview(state: &StateDir, draft: &Path, actions: &[String]) -> Result<Value, Failure> {
    let rule = read_file(draft)?;
    let named = |action: &str, mut failure: Failure| {
        failure.message = format!("{action}: {}", failure.message);
        failure
    };
    let raws = actions
        .iter()
        .map(|action| {
            RawValue::from_string(action.clone()).map_err(|err| {
                named(
                    action,
                    Failure::new(code::bad_arguments, format!("not a JSON action: {err}")),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let provider = Gatekeeper::provider_for_host(&state.path)
        .map_err(|why| Failure::new(code::not_logged_in, why))?;
    // The owner's judgment reads no thread.
    let keeper = Gatekeeper::new(
        provider,
        Box::new(SwarmReader::new(&state.path)),
        &state.path,
    );
    let mut verdicts = Vec::new();
    for raw in &raws {
        let denied = keeper
            .preview(&rule, raw)
            .await
            .map_err(|failure| named(raw.get(), failure))?;
        verdicts.push(json!({"denied": denied}));
    }
    Ok(json!({"verdicts": verdicts}))
}

/// `latch agents map AGENT SWARM_ID`: maps an agent to its Swarm account
/// on the host `--host` names, or this machine's own.
pub async fn map_agent(
    state: &StateDir,
    host: Option<String>,
    agent: &str,
    swarm_id: &str,
) -> Result<Value, Failure> {
    let target = Target {
        state,
        host: hosts::pinned(state, host)?,
    };
    target
        .call(json!({"command": "map_agent", "agent": agent, "swarm_id": swarm_id}))
        .await
}

/// Where the commands go: the host named by `--host`, or this machine's
/// own host state.
struct Target<'a> {
    state: &'a StateDir,
    host: Option<String>,
}

impl Target<'_> {
    async fn call(&self, args: Value) -> Result<Value, Failure> {
        if !hosts::local_target(self.state, self.host.as_deref())? {
            let host = hosts::resolve(self.state, self.host.clone()).await?;
            let request = Request {
                op: "rules".to_string(),
                args,
                intent: None,
            };
            return dial::call(self.state, &host, &request, ANSWER_WITHIN).await;
        }
        let config = self.state.require_login()?;
        let plow = Plow::from_state(self.state)?;
        // A log that can't be written to stops the command before it changes anything.
        let mut log = AuditLog::open(self.state.path.join(crate::audit::FILE))?;
        let (done, snapshot) = owner::changing(
            &self.state.path,
            &plow,
            &config.peer.name,
            &config.peer.kind,
            &args,
        )
        .await;
        let decision = crate::serve::decision(&done.response);
        let mut record = audit::record(&config.peer.key, None, "rules", decision, &done.outcome);
        record.caller_name = Some(config.peer.name.clone());
        record.args = done.audit_args;
        // A change the log doesn't hold is put back.
        let audit_id = log.append(record).inspect_err(|_| owner::undo(snapshot))?;
        crate::serve::with_audit_id(done.response, audit_id).into_result()
    }
}

/// `--revision R` as the host compares it: `none` is no rule.
fn expected(revision: &str) -> Value {
    match revision {
        "none" => Value::Null,
        revision => json!(revision),
    }
}

/// The text in `file`, or on stdin for `-`.
fn read_file(file: &Path) -> Result<String, Failure> {
    let local =
        |err: std::io::Error| Failure::new(code::local_file, format!("{}: {err}", file.display()));
    if file == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(local)?;
        return Ok(text);
    }
    std::fs::read_to_string(file).map_err(local)
}

/// `text` as the owner left it in `$EDITOR` (vi without one).
fn edited(text: &str) -> Result<String, Failure> {
    let failed = |message: String| Failure::new(code::editor_failed, message);
    let file = tempfile::Builder::new()
        .prefix("latch-rules-")
        .suffix(".txt")
        .tempfile()
        .map_err(|err| failed(format!("cannot make a file to edit: {err}")))?;
    std::fs::write(file.path(), text)
        .map_err(|err| failed(format!("cannot make a file to edit: {err}")))?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    // Through the shell, so an EDITOR with arguments (`code -w`) works.
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("latch")
        .arg(file.path())
        .status()
        .map_err(|err| failed(format!("cannot start {editor}: {err}")))?;
    if !status.success() {
        return Err(failed(format!(
            "{editor} ended with {status}; nothing was saved"
        )));
    }
    std::fs::read_to_string(file.path())
        .map_err(|err| failed(format!("cannot read the edited rule: {err}")))
}
