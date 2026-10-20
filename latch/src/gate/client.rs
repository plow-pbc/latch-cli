//! `latch rules …`: the owner's commands on a host's rules for agents.
//! With `--host` they go to that host over latch's connection, as the
//! `rules` operation; a failed call never falls back to a local file.
//! Without it they act on this machine's own host state, audited in its
//! log as the host would.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use super::owner;
use crate::audit::{self, AuditLog};
use crate::cli::{RulesArgs, RulesCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::Plow;
use crate::state::StateDir;
use crate::wire::{Request, Response};

pub async fn run(state: &StateDir, args: RulesArgs) -> Result<Value, Failure> {
    let target = Target {
        state,
        host: args.host,
    };
    let from = args.from;
    match args.command {
        RulesCommand::Set { text, file } => {
            let text = match (text, file) {
                (Some(text), None) => text,
                (None, Some(file)) => read_file(&file)?,
                _ => {
                    let message = "give the rules as TEXT or --file F (- reads stdin)";
                    return Err(Failure::usage("bad_arguments", message.into()));
                }
            };
            target
                .call(json!({"command": "set", "from": from, "text": text}))
                .await
        }
        RulesCommand::Edit => {
            let shown = target
                .call(json!({"command": "show", "from": from}))
                .await?;
            // The text being edited, and who it is for, as the host resolved it.
            let (current, from) = match from {
                None => (&shown["every_agent"], None),
                Some(_) => (
                    &shown["agents"][0],
                    shown["agents"][0]["agent_uid"].as_str(),
                ),
            };
            let text = edited(current["text"].as_str().unwrap_or_default())?;
            target
                .call(json!({"command": "edit", "from": from, "text": text,
                             "revision": current["revision"]}))
                .await
        }
        RulesCommand::Clear => target.call(json!({"command": "clear", "from": from})).await,
        RulesCommand::Show => target.call(json!({"command": "show", "from": from})).await,
    }
}

/// Where the commands go: the host named by `--host`, or this machine's
/// own host state.
struct Target<'a> {
    state: &'a StateDir,
    host: Option<String>,
}

impl Target<'_> {
    async fn call(&self, args: Value) -> Result<Value, Failure> {
        if let Some(host) = &self.host {
            let host = hosts::resolve(self.state, Some(host.clone())).await?;
            let request = Request {
                op: "rules".to_string(),
                args,
                chat: None,
            };
            return dial::call(self.state, &host, &request, ANSWER_WITHIN).await;
        }
        let config = self.state.require_login()?;
        let plow = Plow::from_state(self.state)?;
        // A log that can't be written to stops the command before it changes anything.
        let mut log = AuditLog::open(self.state.path.join("audit.ndjson"))?;
        let done = owner::run(
            &self.state.path,
            &plow,
            &config.peer.name,
            &config.peer.kind,
            &args,
        )
        .await;
        let decision = crate::serve::decision(&done.response);
        let mut record = audit::record(&config.peer.key, None, "rules", decision, &done.outcome);
        record.args = done.audit_args;
        let audit_id = log.append(record)?;
        match crate::serve::with_audit_id(done.response, audit_id) {
            Response::Ok(value) => Ok(value),
            Response::Error {
                exit,
                code,
                message,
                details,
            } => {
                let mut failure = Failure::new(exit, &code, message);
                failure.details = details;
                Err(failure)
            }
        }
    }
}

/// The rules in `file`, or on stdin for `-`.
fn read_file(file: &Path) -> Result<String, Failure> {
    let local =
        |err: std::io::Error| Failure::usage("local_file", format!("{}: {err}", file.display()));
    if file == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(local)?;
        return Ok(text);
    }
    std::fs::read_to_string(file).map_err(local)
}

/// `text` as the owner left it in `$EDITOR` (vi without one).
fn edited(text: &str) -> Result<String, Failure> {
    let failed = |message: String| Failure::new(Exit::Failed, "editor_failed", message);
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
        .map_err(|err| failed(format!("cannot read the edited rules: {err}")))
}
