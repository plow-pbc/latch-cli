//! `latch rules …`: the owner's commands on a host's rules for agents.
//! With another `--host` they go over latch's connection, as the
//! `rules` operation; a failed call never falls back to a local file.
//! Without it, or naming this peer, they act locally, audited in its
//! log as the host would.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches};
use serde_json::{Value, json};

use super::owner;
use crate::audit::{self, AuditLog};
use crate::cli::{Cli, Command as Latch, FileCommand, HostArg, RulesArgs, RulesCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::Plow;
use crate::state::StateDir;
use crate::wire::Request;
use crate::{google, transfer};

pub async fn run(state: &StateDir, args: RulesArgs) -> Result<Value, Failure> {
    let scope = match args.command {
        RulesCommand::Try {
            host,
            agent,
            new_agent,
            draft,
            request,
        } => {
            let args = json!({"from": agent, "new_agent": new_agent});
            return preview(state, host, args, draft, request).await;
        }
        RulesCommand::Set { ref scope, .. }
        | RulesCommand::Edit { ref scope }
        | RulesCommand::Clear { ref scope }
        | RulesCommand::Show { ref scope } => scope,
    };
    let target = Target {
        state,
        host: scope.host.clone(),
    };
    // The operation still calls the agent `from`, as hosts of every
    // version read it.
    let from = scope.agent.clone();
    match args.command {
        RulesCommand::Set { text, file, .. } => {
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
        RulesCommand::Edit { .. } => {
            // It opens the person's editor: a program sets the rules instead.
            if !crate::term::interactive() {
                let message = "`latch rules edit` opens your editor, so it needs a terminal";
                let mut failure = Failure::usage("no_terminal", message.into());
                failure.next = Some("latch rules set TEXT".into());
                return Err(failure);
            }
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
            // From the editor on, what goes wrong is the person's to read.
            let text =
                edited(current["text"].as_str().unwrap_or_default()).map_err(Failure::told)?;
            target
                .call(json!({"command": "edit", "from": from, "text": text,
                             "revision": current["revision"]}))
                .await
                .map_err(Failure::told)
        }
        RulesCommand::Clear { .. } => target.call(json!({"command": "clear", "from": from})).await,
        RulesCommand::Show { .. } => target.call(json!({"command": "show", "from": from})).await,
        RulesCommand::Try { .. } => unreachable!("previewed above"),
    }
}

/// `latch rules try -- REQUEST`: REQUEST parsed and built exactly as
/// latch would send it, then previewed (the `rules_try` operation, with
/// `args` saying as whom and against which rules), which runs nothing. The
/// host is the one `--host` names, where it runs; or this machine's own,
/// judged here, running or not. Never `LATCH_HOST`, as for every rules
/// command, and a `--host` inside REQUEST is refused. Its `--plow-chat`
/// (or `LATCH_PLOW_CHAT`) means what it would on the real call. A
/// `--draft` is read before REQUEST is built, and only one of them may
/// read stdin.
async fn preview(
    state: &StateDir,
    host: Option<String>,
    mut args: Value,
    draft: Option<PathBuf>,
    words: Vec<String>,
) -> Result<Value, Failure> {
    // The parser's message, its usage line written as the request goes
    // after `latch rules try --`.
    let usage = |err: clap::Error| {
        let message = err.to_string().trim_end().to_string();
        let message = message.replacen("Usage: latch ", "Usage: latch rules try -- ", 1);
        Failure::usage("usage", message)
    };
    let matches = Cli::command()
        .try_get_matches_from(std::iter::once("latch".to_string()).chain(words))
        .map_err(usage)?;
    let parsed = Cli::from_arg_matches(&matches).map_err(usage)?;
    let mut innermost = &matches;
    while let Some((_, deeper)) = innermost.subcommand() {
        innermost = deeper;
    }
    if innermost.ids().any(|id| id == "host")
        && innermost.value_source("host") == Some(ValueSource::CommandLine)
    {
        let message = "The host goes on `latch rules try`, not on the request: \
                       latch rules try --host NAME -- <request>";
        return Err(Failure::usage("usage", message.to_string()));
    }
    if draft.as_deref() == Some(Path::new("-"))
        && matches!(&parsed.command, Latch::Exec(exec) if exec.stdin)
    {
        let message = "--draft - and exec --stdin both read stdin: give the draft as a file, \
                       --draft F";
        return Err(Failure::usage("usage", message.to_string()));
    }
    args["draft"] = json!(draft.map(|file| read_file(&file)).transpose()?);
    let (op, request, asked): (&str, Value, HostArg) = match parsed.command {
        Latch::Exec(args) => ("exec", crate::exec::request(&args)?, args.host),
        Latch::File(FileCommand::Get {
            remote_path, host, ..
        }) => ("file_get", transfer::get_args(&remote_path), host),
        Latch::File(FileCommand::Put {
            local_path,
            remote_path,
            host,
        }) => {
            let (_, args) = transfer::put_args(&local_path, &remote_path).await?;
            ("file_put", args, host)
        }
        Latch::Google(args) if !args.local_help() => {
            let prepared = google::client::prepare(&args).await?;
            let request = serde_json::to_value(&prepared.request).expect("request serializes");
            ("google", request, args.host)
        }
        #[cfg(target_os = "macos")]
        Latch::Messages(command) => (
            "messages",
            crate::messages::messages_request(&command),
            command.host(),
        ),
        #[cfg(target_os = "macos")]
        Latch::Contacts(command) => (
            "contacts",
            crate::messages::contacts_request(&command),
            command.host(),
        ),
        _ => {
            let name = matches.subcommand_name().unwrap_or_default();
            return Err(crate::serve::not_previewable(name));
        }
    };
    args["request"] = json!({"op": op, "args": request, "chat": asked.plow_chat});
    if hosts::local_target(state, host.as_deref())? {
        return crate::serve::preview_here(state, &args).await;
    }
    let peer = hosts::resolve(state, host).await?;
    let request = Request {
        op: "rules_try".to_string(),
        args,
        chat: None,
    };
    dial::call(state, &peer, &request, ANSWER_WITHIN).await
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
                chat: None,
            };
            return dial::call(self.state, &host, &request, ANSWER_WITHIN).await;
        }
        let config = self.state.require_login()?;
        let plow = Plow::from_state(self.state)?;
        // A log that can't be written to stops the command before it changes anything.
        let mut log = AuditLog::open(self.state.path.join(crate::audit::FILE))?;
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
        record.caller_name = Some(config.peer.name.clone());
        record.args = done.audit_args;
        let audit_id = log.append(record)?;
        crate::serve::with_audit_id(done.response, audit_id).into_result()
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
