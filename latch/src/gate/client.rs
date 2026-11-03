//! `latch rules …`: the owner's commands on the account's rules
//! (`account.rs`), and `rules try`, a preview of one host's judgment.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches};
use serde_json::{Value, json};

use super::account;
use crate::cli::{Cli, Command as Latch, FileCommand, HostArg, RulesArgs, RulesCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Failure, code};
use crate::hosts;
use crate::state::StateDir;
use crate::wire::Request;
use crate::{google, transfer};

pub async fn run(state: &StateDir, args: RulesArgs) -> Result<Value, Failure> {
    match args.command {
        RulesCommand::Try {
            host,
            agent,
            new_agent,
            draft,
            fill_login,
            url,
            fields,
            request,
        } => {
            let mut args = json!({"from": agent, "new_agent": new_agent});
            if let (Some(title), Some(url)) = (fill_login, url) {
                let fields = match fields.is_empty() {
                    true => vec!["username".to_string(), "password".to_string()],
                    false => fields,
                };
                args["fill"] = json!({"url": url, "item": {"kind": "login", "title": title},
                                      "fields": fields});
                args["draft"] = json!(draft.map(|file| read_file(&file)).transpose()?);
                return previewed(state, host, args).await;
            }
            preview(state, host, args, draft, request).await
        }
        RulesCommand::List { agent, host } => account::list(state, agent, host).await,
        RulesCommand::Add {
            text,
            file,
            preset,
            covers,
            if_empty,
        } => {
            let text = match (text, file, preset) {
                (Some(text), None, None) => text,
                (None, Some(file), None) => read_file(&file)?,
                (None, None, Some(preset)) => preset.text().to_string(),
                _ => {
                    let message =
                        "give the rule as TEXT, --file F (- reads stdin) or --preset NAME";
                    return Err(Failure::new(code::bad_arguments, message.into()));
                }
            };
            account::add(state, text, covers.agents, covers.hosts, if_empty).await
        }
        RulesCommand::Edit {
            id,
            text,
            file,
            covers,
        } => {
            let text = match (text, file) {
                (Some(text), _) => Some(text),
                (None, Some(file)) => Some(read_file(&file)?),
                (None, None) => None,
            };
            account::edit(state, id, text, covers.agents, covers.hosts).await
        }
        RulesCommand::Remove { id } => account::remove(state, id).await,
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
        Failure::new(code::usage, message)
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
        let message = "the host goes on `latch rules try`, not on the request";
        let mut failure = Failure::new(code::usage, message.to_string());
        failure.next = Some("latch rules try --host NAME -- <request>".into());
        return Err(failure);
    }
    if draft.as_deref() == Some(Path::new("-"))
        && matches!(&parsed.command, Some(Latch::Exec(exec)) if exec.stdin)
    {
        let message = "--draft - and exec --stdin both read stdin";
        let mut failure = Failure::new(code::usage, message.to_string());
        failure.next = Some("give the draft as a file: --draft F".into());
        return Err(failure);
    }
    args["draft"] = json!(draft.map(|file| read_file(&file)).transpose()?);
    let (op, request, asked): (&str, Value, HostArg) = match parsed.command {
        Some(Latch::Exec(args)) => ("exec", crate::exec::request(&args)?, args.host),
        Some(Latch::File(FileCommand::Get {
            remote_path, host, ..
        })) => ("file_get", transfer::get_args(&remote_path), host),
        Some(Latch::File(FileCommand::Put {
            local_path,
            remote_path,
            host,
        })) => {
            let (_, args) = transfer::put_args(&local_path, &remote_path).await?;
            ("file_put", args, host)
        }
        Some(Latch::Google(args)) if !args.local_help() => {
            let prepared = google::client::prepare(&args).await?;
            let request = serde_json::to_value(&prepared.request).expect("request serializes");
            ("google", request, args.host)
        }
        Some(Latch::Messages(command)) => (
            "messages",
            crate::messages::messages_request(&command),
            command.host(),
        ),
        Some(Latch::Contacts(command)) => (
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
    previewed(state, host, args).await
}

/// The `rules_try` operation with `args`, on this machine's own host or the
/// one `--host` names.
async fn previewed(state: &StateDir, host: Option<String>, args: Value) -> Result<Value, Failure> {
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

/// The rules in `file`, or on stdin for `-`.
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
