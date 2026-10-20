//! The agent's side of `latch browser`: each command is one `browser`
//! request to the host, answered with one JSON result. A screenshot's or
//! download's bytes stream after its first answer and land in the client's
//! file; an upload's stream from the client's file after the host's
//! go-ahead. Both sides compare sha256.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::fs::File;

use super::plan::{self, Check, Literal, Op, Plan, Step};
use crate::cli::{
    ActAction, BrowserArgs, BrowserCommand, DialogAction, DownloadCommand, TabCommand,
};
use crate::dial::{self, ANSWER_MARGIN, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::state::StateDir;
use crate::transfer;
use crate::wire::Request;

pub async fn run(state: &StateDir, args: BrowserArgs) -> Result<Value, Failure> {
    let mut request = json!({
        "session": args.session,
        "ppid": std::os::unix::process::parent_id(),
        "turn": args.turn,
    });
    // Where bytes go: a screenshot or download into a file here, an
    // upload's out of one.
    let transfer = match &args.command {
        BrowserCommand::Screenshot { output }
        | BrowserCommand::Download {
            command: DownloadCommand::Get { output, .. },
        }
        | BrowserCommand::Do {
            output: Some(output),
            ..
        } => Some(Transfer::Into(output.clone())),
        BrowserCommand::Upload { file, .. } => Some(Transfer::From(open_upload(file).await?)),
        _ => None,
    };
    if let BrowserCommand::Eval { script_file } = &args.command {
        request["script"] = json!(read_script(script_file)?);
    }
    // How long the host may take: a plan and a wait each have their own
    // deadline.
    let mut answer_within = ANSWER_WITHIN;
    let plan = match &args.command {
        BrowserCommand::Do {
            steps,
            plan_file,
            confirm,
            history: None,
            within,
            ..
        } => Some(do_plan(
            steps,
            plan_file.as_deref(),
            confirm.is_some(),
            *within,
        )?),
        BrowserCommand::Act {
            snapshot,
            action,
            target,
            text,
            check,
        } => {
            let literal = act_literal(*action, snapshot.clone(), target.clone(), text.clone())?;
            let check = check
                .as_deref()
                .map(Check::parse)
                .transpose()
                .map_err(|why| Failure::usage("bad_arguments", why))?;
            let plan = Plan {
                steps: vec![Step {
                    literal: Some(literal),
                    check,
                    ..Step::default()
                }],
                ..Plan::default()
            };
            plan.validate()
                .map_err(|why| Failure::usage("bad_arguments", why))?;
            Some(plan)
        }
        BrowserCommand::Wait { within, .. } => {
            answer_within += Duration::from_secs_f64(within.unwrap_or(10.0)) + ANSWER_MARGIN;
            None
        }
        _ => None,
    };
    if let Some(plan) = plan {
        answer_within = Duration::from_secs_f64(plan.within()) + ANSWER_MARGIN;
        request["plan"] = json!(plan);
        request["request"] = json!(new_request_id());
    }
    fill(&mut request, args.command)?;
    let host = hosts::resolve(state, args.host).await?;
    if let Some(Transfer::From(upload)) = &transfer {
        request["name"] = json!(upload.name);
        request["size"] = json!(upload.size);
    }
    let request = Request {
        op: "browser".to_string(),
        args: request,
    };
    let id = request.args["request"].clone();
    match transfer {
        None => dial::call(state, &host, &request, answer_within)
            .await
            .map_err(|failure| with_request(failure, &id)),
        Some(transfer) => {
            let link = dial::connect(state, &host).await?;
            let result = match transfer {
                Transfer::Into(path) => receive(&link.connection, &request, &path).await,
                Transfer::From(mut upload) => send(&link.connection, &request, &mut upload).await,
            };
            link.close().await;
            result
        }
    }
}

enum Transfer {
    Into(std::path::PathBuf),
    From(Upload),
}

struct Upload {
    file: File,
    name: String,
    size: u64,
}

async fn open_upload(path: &Path) -> Result<Upload, Failure> {
    let local =
        |err: std::io::Error| Failure::usage("local_file", format!("{}: {err}", path.display()));
    let file = File::open(path).await.map_err(local)?;
    let metadata = file.metadata().await.map_err(local)?;
    if !metadata.is_file() {
        return Err(Failure::usage(
            "not_a_file",
            format!("{} is not a file", path.display()),
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Failure::usage("local_file", format!("{} has no file name", path.display()))
        })?
        .to_string();
    Ok(Upload {
        file,
        name,
        size: metadata.len(),
    })
}

/// The script, from its file or from stdin (`-`).
fn read_script(path: &Path) -> Result<String, Failure> {
    let bytes = if path == Path::new("-") {
        crate::input::read_all(std::time::Duration::from_secs(30))?
    } else {
        std::fs::read(path)
            .map_err(|err| Failure::usage("local_file", format!("{}: {err}", path.display())))?
    };
    String::from_utf8(bytes)
        .map_err(|_| Failure::usage("local_file", "the script is not UTF-8 text".into()))
}

/// An upload: the bytes, then the host's answer, given this side's sha256.
async fn send(
    connection: &iroh::endpoint::Connection,
    request: &Request,
    upload: &mut Upload,
) -> Result<Value, Failure> {
    let (mut result, sha256) =
        transfer::upload(connection, request, &mut upload.file, upload.size).await?;
    result["file"]["sha256"] = json!(sha256);
    Ok(result)
}

/// The request's command and arguments.
fn fill(request: &mut Value, command: BrowserCommand) -> Result<(), Failure> {
    let set = |request: &mut Value, key: &str, value: Value| {
        if !value.is_null() {
            request[key] = value;
        }
    };
    let name = match command {
        BrowserCommand::Open {
            profile,
            task,
            reserve_site,
            url,
        } => {
            // A new session: it has no id yet.
            if let Some(request) = request.as_object_mut() {
                request.remove("session");
            }
            set(request, "profile", json!(profile));
            set(request, "task", json!(task));
            set(request, "reserve_site", json!(reserve_site.then_some(true)));
            set(request, "url", json!(url));
            "open"
        }
        BrowserCommand::Observe { tab, scope } => {
            set(request, "tab", json!(tab));
            set(request, "scope", json!(scope));
            "observe"
        }
        BrowserCommand::Text => "text",
        BrowserCommand::Screenshot { .. } => "screenshot",
        // Its plan is in the request already.
        BrowserCommand::Act { .. } => "do",
        BrowserCommand::Do {
            history: Some(id),
            shot,
            ..
        } => {
            set(request, "request", json!(id));
            set(request, "shot", json!(shot));
            "do_history"
        }
        BrowserCommand::Do { confirm, .. } => {
            set(request, "confirm", json!(confirm));
            "do"
        }
        BrowserCommand::Find { role, name } => {
            if role.is_none() || name.is_none() {
                return Err(Failure::usage(
                    "bad_arguments",
                    "find takes --role R --name N".into(),
                ));
            }
            set(request, "role", json!(role));
            set(request, "name", json!(name));
            "find"
        }
        BrowserCommand::Wait {
            target,
            snapshot,
            state,
            text,
            url_path,
            within,
            ..
        } => {
            set(request, "ref", json!(target));
            set(request, "snapshot", json!(snapshot));
            set(request, "state", json!(state));
            set(request, "text", json!(text));
            set(request, "url_path", json!(url_path));
            set(request, "within", json!(within));
            "wait"
        }
        BrowserCommand::Tab { command } => {
            let (action, tab, url) = match command {
                TabCommand::New { url } => ("new", None, url),
                TabCommand::Switch { tab } => ("switch", Some(tab), None),
                TabCommand::Close { tab } => ("close", tab, None),
            };
            set(request, "action", json!(action));
            set(request, "tab", json!(tab));
            set(request, "url", json!(url));
            "tab"
        }
        BrowserCommand::Dialog { action, text } => {
            set(
                request,
                "accept",
                json!(matches!(action, DialogAction::Accept)),
            );
            set(request, "text", json!(text));
            "dialog"
        }
        BrowserCommand::Eval { .. } => "eval",
        BrowserCommand::Upload {
            snapshot, target, ..
        } => {
            set(request, "snapshot", json!(snapshot));
            set(request, "target", json!(target));
            "upload"
        }
        BrowserCommand::Downloads => "downloads",
        BrowserCommand::Download {
            command: DownloadCommand::Get { download, .. },
        } => {
            set(request, "download", json!(download));
            "download_get"
        }
        BrowserCommand::Login {
            snapshot,
            item,
            username,
            password,
            submit,
        } => {
            set(request, "snapshot", json!(snapshot));
            set(request, "item", json!(item));
            set(request, "username", json!(username));
            set(request, "password", json!(password));
            set(request, "submit", json!(submit));
            "login"
        }
        BrowserCommand::Fill {
            snapshot,
            item,
            fields,
            press,
        } => {
            let mut pairs = Vec::new();
            for field in fields {
                let (label, refs) = field.rsplit_once('=').ok_or_else(|| {
                    Failure::usage(
                        "bad_arguments",
                        format!("--field takes LABEL=eN, not {field:?}"),
                    )
                })?;
                let refs: Vec<&str> = refs.split(',').map(str::trim).collect();
                pairs.push(json!([label, refs]));
            }
            set(request, "snapshot", json!(snapshot));
            set(request, "item", json!(item));
            set(request, "fields", json!(pairs));
            set(request, "press", json!(press));
            // A fill that stops for a confirmation is kept as a request.
            set(request, "request", json!(new_request_id()));
            "fill"
        }
        BrowserCommand::FillCode {
            snapshot,
            item,
            field,
            submit,
        } => {
            set(request, "snapshot", json!(snapshot));
            set(request, "item", json!(item));
            set(request, "field", json!(field));
            set(request, "submit", json!(submit));
            "fill_code"
        }
        BrowserCommand::Sessions => "sessions",
        BrowserCommand::Close => "close",
    };
    if !matches!(name, "open" | "sessions") && request["session"].is_null() {
        return Err(Failure::usage(
            "no_session",
            "name the session with --session S (the id `open` answered) or LATCH_BROWSER_SESSION"
                .into(),
        ));
    }
    request["command"] = json!(name);
    Ok(())
}

/// A request id, made here so the client knows it even if the answer is
/// lost: `do --history` reads that request's steps.
fn new_request_id() -> String {
    format!("r{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff)
}

/// A failed plan names its request, so its history can be read; above all
/// when the outcome is unknown (exit 5).
fn with_request(mut failure: Failure, id: &Value) -> Failure {
    if id.is_null() {
        return failure;
    }
    let mut details = failure.details.as_object().cloned().unwrap_or_default();
    details.entry("request").or_insert_with(|| id.clone());
    if failure.exit == Exit::Unknown {
        failure.message = format!(
            "{}: read `latch browser do --history {}` first, and never resend the request",
            failure.message,
            id.as_str().unwrap_or_default()
        );
    }
    failure.details = Value::Object(details);
    failure
}

/// The plan command's plan: from `--plan-file`, or one short-form step per
/// argument, checked whole before it is sent.
pub(super) fn do_plan(
    steps: &[String],
    plan_file: Option<&Path>,
    confirm: bool,
    within: Option<f64>,
) -> Result<Plan, Failure> {
    let bad = |why: String| Failure::usage("bad_arguments", why);
    let mut plan = match plan_file {
        Some(file) => serde_json::from_str::<Plan>(&read_script(file)?)
            .map_err(|err| bad(format!("the plan file is malformed: {err}")))?,
        None => Plan {
            steps: steps
                .iter()
                .enumerate()
                .map(|(n, step)| {
                    plan::short_step(step).map_err(|why| bad(format!("step {}: {why}", n + 1)))
                })
                .collect::<Result<_, _>>()?,
            ..Plan::default()
        },
    };
    if within.is_some() {
        plan.within = within;
    }
    if plan.steps.is_empty() && !confirm {
        return Err(bad(
            "do takes steps, --plan-file, --confirm C or --history R".into(),
        ));
    }
    plan.validate().map_err(bad)?;
    Ok(plan)
}

/// `act`'s words as one literal step. A direction to scroll may come
/// without a ref: `act scroll down`.
pub(super) fn act_literal(
    action: Option<ActAction>,
    snapshot: Option<String>,
    target: Option<String>,
    text: Option<String>,
) -> Result<Literal, Failure> {
    let action = action.ok_or_else(|| {
        Failure::usage(
            "bad_arguments",
            "act needs an action: click, type, select, press, scroll or back".into(),
        )
    })?;
    let (target, text) = match (action, target, text) {
        (ActAction::Scroll, Some(word), None) if !plan::is_ref(&word) => (None, Some(word)),
        (_, Some(word), _) if !plan::is_ref(&word) => {
            return Err(Failure::usage(
                "bad_arguments",
                format!(
                    "act takes a ref (eN), not {word:?}: to name an element by its name or in words, write it as a plan step; see `latch browser do --help`"
                ),
            ));
        }
        words => (words.1, words.2),
    };
    let op = match action {
        ActAction::Click => Op::Click,
        ActAction::Type => Op::Type,
        ActAction::Select => Op::Select,
        ActAction::Press => Op::Press,
        ActAction::Scroll => Op::Scroll,
        ActAction::Back => Op::Back,
    };
    Ok(Literal {
        op,
        target,
        snapshot,
        text,
        ..Literal::click("e1")
    })
}

/// A screenshot or download: its details, then exactly that many bytes
/// into `path`, then the host's sha256, which must match.
async fn receive(
    connection: &iroh::endpoint::Connection,
    request: &Request,
    path: &Path,
) -> Result<Value, Failure> {
    let (mut meta, done, sha256) = transfer::download(connection, request, path).await?;
    meta["path"] = json!(path);
    meta["sha256"] = json!(sha256);
    meta["audit_id"] = done["audit_id"].clone();
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_plan_names_its_request_for_its_history() {
        let lost = Failure::new(Exit::Unknown, "outcome_unknown", "dropped".into());
        let named = with_request(lost, &json!("r00aa"));
        assert_eq!(named.details["request"], "r00aa");
        assert!(named.message.contains("do --history r00aa"));
        assert!(new_request_id().starts_with('r'));
        assert_ne!(new_request_id(), new_request_id());
    }

    #[test]
    fn act_is_one_literal_step_on_a_ref() {
        let literal = |a, t: Option<&str>, x: Option<&str>| {
            act_literal(Some(a), None, t.map(Into::into), x.map(Into::into))
        };
        let press = literal(ActAction::Press, Some("e4"), Some("Enter")).unwrap();
        assert_eq!(
            (press.op, press.text.as_deref()),
            (Op::Press, Some("Enter"))
        );
        let scroll = literal(ActAction::Scroll, Some("down"), None).unwrap();
        assert_eq!(
            (scroll.target, scroll.text.as_deref()),
            (None, Some("down"))
        );
        let named = literal(ActAction::Click, Some("Place order"), None).unwrap_err();
        assert_eq!(
            (named.exit, named.code.as_str()),
            (Exit::Usage, "bad_arguments")
        );
        assert!(named.message.contains("plan step"));
        assert!(act_literal(None, None, None, None).is_err());
    }

    #[tokio::test]
    async fn removed_forms_are_usage_errors() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        for argv in [
            &["act", "click", "--at", "10,20"][..],
            &["act", "click", "e1", "--expect", "the order is placed"],
            &["act", "--steps", "steps.txt"],
            &["act", "--history", "r1"],
            &["find", "the unpaid invoice"],
            &["wait", "--for", "the invoices are listed"],
        ] {
            assert!(
                Cli::try_parse_from(["latch", "browser"].iter().chain(argv)).is_err(),
                "{argv:?}"
            );
        }
        // A name or description where a ref goes: refused before any host is
        // looked up, pointing to the plan command.
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(Some(dir.path().to_path_buf())).unwrap();
        for argv in [
            &["act", "click", "Place order"][..],
            &["act", "type", "the amount field", "12.50"],
        ] {
            let cli = Cli::try_parse_from(["latch", "browser"].iter().chain(argv)).unwrap();
            let Command::Browser(args) = cli.command else {
                panic!("browser");
            };
            let failure = run(&state, args).await.unwrap_err();
            assert_eq!(failure.exit, Exit::Usage, "{argv:?}");
            assert!(
                failure.message.contains("latch browser do"),
                "{}",
                failure.message
            );
        }
    }

    #[test]
    fn the_short_form_is_checked_whole_before_it_is_sent() {
        let steps = [
            "click e7 -> path: /checkout".to_string(),
            "type e3".to_string(),
        ];
        let bad = do_plan(&steps, None, false, None).unwrap_err();
        assert!(bad.message.starts_with("step 2:"), "{}", bad.message);
        assert!(do_plan(&[], None, false, None).is_err());
        assert!(do_plan(&[], None, true, None).is_ok());
        let plan = do_plan(&steps[..1], None, false, Some(30.0)).unwrap();
        assert_eq!(plan.within(), 30.0);
        assert!(do_plan(&steps[..1], None, false, Some(301.0)).is_err());
    }
}
