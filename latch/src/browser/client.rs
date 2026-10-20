//! The agent's side of `latch browser`: each command is one `browser`
//! request to the host, answered with one JSON result. A screenshot's or
//! download's bytes stream after its first answer and land in the client's
//! file; an upload's stream from the client's file after the host's
//! go-ahead. Both sides compare sha256.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::fs::File;

use clap::Parser;

use crate::cli::{
    ActAction, BrowserArgs, BrowserCommand, DialogAction, DownloadCommand, StepArgs, StepLine,
    TabCommand,
};
use crate::dial::{self, ANSWER_MARGIN, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::state::StateDir;
use crate::wire::{self, Request};

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
        } => Some(Transfer::Into(output.clone())),
        BrowserCommand::Upload { file, .. } => Some(Transfer::From(open_upload(file).await?)),
        _ => None,
    };
    if let BrowserCommand::Eval { script_file } = &args.command {
        request["script"] = json!(read_script(script_file)?);
    }
    // How long the host may take: an act's expectations and a wait each
    // have their own deadline.
    let mut answer_within = ANSWER_WITHIN;
    match &args.command {
        BrowserCommand::Act {
            step,
            steps,
            history,
        } => {
            if let Some(id) = history {
                request["command"] = json!("act_history");
                request["request"] = json!(id);
            } else {
                let (list, parsed) = match steps {
                    Some(file) => (true, read_steps(&read_script(file)?)?),
                    None => (false, vec![step_json(step.clone())?]),
                };
                let waits: f64 = parsed
                    .iter()
                    .map(|s| s["expect"]["within"].as_f64().unwrap_or(10.0) + 15.0)
                    .sum();
                answer_within += Duration::from_secs_f64(waits);
                request["command"] = json!("act");
                request["list"] = json!(list);
                request["steps"] = json!(parsed);
                request["request"] = json!(new_request_id());
            }
        }
        BrowserCommand::Wait { within, .. } => {
            answer_within += Duration::from_secs_f64(within.unwrap_or(10.0)) + ANSWER_MARGIN;
        }
        _ => {}
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

/// An upload: wait for the host's go-ahead, send the bytes, and check the
/// host saw the same sha256.
async fn send(
    connection: &iroh::endpoint::Connection,
    request: &Request,
    upload: &mut Upload,
) -> Result<Value, Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let sha256 = wire::copy(&mut upload.file, &mut send, upload.size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let mut result = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
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
        // Its steps are in the request already.
        BrowserCommand::Act { history, .. } => {
            if history.is_some() {
                "act_history"
            } else {
                "act"
            }
        }
        BrowserCommand::Find {
            description,
            role,
            name,
            action,
        } => {
            set(request, "description", json!(description));
            set(request, "role", json!(role));
            set(request, "name", json!(name));
            set(request, "action", json!(action.map(ActAction::name)));
            "find"
        }
        BrowserCommand::Wait {
            target,
            snapshot,
            state,
            text,
            url,
            sentence,
            within,
        } => {
            set(request, "ref", json!(target));
            set(request, "snapshot", json!(snapshot));
            set(request, "state", json!(state));
            set(request, "text", json!(text));
            set(request, "url", json!(url));
            set(request, "for", json!(sentence));
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
/// lost: `act --history` reads that request's steps.
fn new_request_id() -> String {
    format!("r{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff)
}

/// A failed `act` names its request, so its history can be read; above
/// all when the outcome is unknown (exit 5).
fn with_request(mut failure: Failure, id: &Value) -> Failure {
    if id.is_null() {
        return failure;
    }
    let mut details = failure.details.as_object().cloned().unwrap_or_default();
    details.entry("request").or_insert_with(|| id.clone());
    if failure.exit == Exit::Unknown {
        failure.message = format!(
            "{}: read `latch browser act --history {}` first",
            failure.message,
            id.as_str().unwrap_or_default()
        );
    }
    failure.details = Value::Object(details);
    failure
}

/// One step as the host takes it.
fn step_json(step: StepArgs) -> Result<Value, Failure> {
    let action = step.action.ok_or_else(|| {
        Failure::usage(
            "bad_arguments",
            "act needs an action (click, type, select, press, scroll, back), --steps or --history"
                .into(),
        )
    })?;
    let (target, text) = act_words(action, step.target, step.text, step.at.is_some())?;
    let mut expect = serde_json::Map::new();
    let mut put = |key: &str, value: Value| {
        if !value.is_null() {
            expect.insert(key.into(), value);
        }
    };
    put("sentence", json!(step.expect));
    put("url", json!(step.expect_url));
    put("text", json!(step.expect_text));
    put("field", json!(step.expect_field));
    put("within", json!(step.within));
    if step.expect_download.is_some() {
        put("download", json!(true));
    }
    Ok(json!({
        "snapshot": step.snapshot,
        "action": action.name(),
        "target": target,
        "text": text,
        "at": step.at.map(|(x, y)| [x, y]),
        "expect": expect,
    }))
}

/// A step list: one step per line in `act`'s own form, quoted as a shell
/// would; blank lines and `#` comments are skipped. Every line is checked
/// before anything is sent.
pub(super) fn read_steps(text: &str) -> Result<Vec<Value>, Failure> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let at =
            |message: String| Failure::usage("bad_arguments", format!("line {}: {message}", n + 1));
        let words = split_words(line).map_err(at)?;
        if words.is_empty() {
            continue;
        }
        let parsed = StepLine::try_parse_from(&words).map_err(|err| {
            at(err
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("error: ")
                .to_string())
        })?;
        steps.push(step_json(parsed.step).map_err(|failure| at(failure.message))?);
    }
    if steps.is_empty() {
        return Err(Failure::usage(
            "bad_arguments",
            "the step list is empty".into(),
        ));
    }
    Ok(steps)
}

/// A line's words, as a POSIX shell splits them: 'single' and "double"
/// quotes, backslash escapes, and a `#` word starting a comment.
fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                words.extend(word.take());
            }
            '#' if word.is_none() => break,
            '\\' => {
                let next = chars.next().ok_or("the line ends with a backslash")?;
                word.get_or_insert_default().push(next);
            }
            '\'' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("a ' quote is not closed".into()),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => current.push(c),
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => return Err("a \" quote is not closed".into()),
                        },
                        Some(c) => current.push(c),
                        None => return Err("a \" quote is not closed".into()),
                    }
                }
            }
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

/// `act`'s words as target and text. A key to press, or a direction to
/// scroll, may come without a target: `act press Enter`, `act scroll down`.
fn act_words(
    action: ActAction,
    target: Option<String>,
    text: Option<String>,
    at: bool,
) -> Result<(Option<String>, Option<String>), Failure> {
    let is_ref = |word: &str| word.starts_with('e') && word[1..].parse::<u32>().is_ok();
    let words = match (action, target, text) {
        (ActAction::Press | ActAction::Scroll, Some(word), None)
            if !is_ref(&word) && word != "page" =>
        {
            (None, Some(word))
        }
        (ActAction::Back, None, None) => (None, None),
        (ActAction::Back, ..) => {
            return Err(Failure::usage(
                "bad_arguments",
                "back takes no target".into(),
            ));
        }
        (ActAction::Click, None, None) if at => (None, None),
        (_, None, _) if !matches!(action, ActAction::Scroll) => {
            return Err(Failure::usage(
                "bad_arguments",
                "name the target: a ref such as e7, its exact name, or a description".into(),
            ));
        }
        (_, target, text) => (target, text),
    };
    if at && (action != ActAction::Click || words.0.is_some()) {
        return Err(Failure::usage(
            "bad_arguments",
            "--at is for `act click --at X,Y`, without a ref".into(),
        ));
    }
    Ok(words)
}

/// A screenshot or download: its details, then exactly that many bytes
/// into `path`, then the host's sha256, which must match.
async fn receive(
    connection: &iroh::endpoint::Connection,
    request: &Request,
    path: &Path,
) -> Result<Value, Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let mut meta = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let size = meta["size"].as_u64().ok_or_else(|| {
        Failure::new(
            Exit::Unknown,
            "bad_response",
            "the host sent no size".into(),
        )
    })?;
    let mut file = File::create(path)
        .await
        .map_err(|err| Failure::usage("local_file", format!("{}: {err}", path.display())))?;
    let Ok(sha256) = wire::copy(&mut recv, &mut file, size).await else {
        let _ = tokio::fs::remove_file(path).await;
        return Err(dial::lost(connection, true));
    };
    let done = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    if done["sha256"] != sha256.as_str() {
        let _ = tokio::fs::remove_file(path).await;
        return Err(Failure::failed(
            "checksum_mismatch",
            format!(
                "the host sent sha256 {}, this side got {sha256}",
                done["sha256"]
            ),
        ));
    }
    meta["path"] = json!(path);
    meta["sha256"] = json!(sha256);
    meta["audit_id"] = done["audit_id"].clone();
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(
        action: ActAction,
        target: Option<&str>,
        text: Option<&str>,
    ) -> Result<(Option<String>, Option<String>), Failure> {
        act_words(action, target.map(Into::into), text.map(Into::into), false)
    }

    #[test]
    fn step_lines_split_as_a_shell_would() {
        assert_eq!(
            split_words(r#"type "amount" '$388.50' --expect "it's \"done\"" # note"#).unwrap(),
            ["type", "amount", "$388.50", "--expect", r#"it's "done""#]
        );
        assert_eq!(
            split_words("  # only a comment").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            split_words(r"click New\ expense").unwrap(),
            ["click", "New expense"]
        );
        assert!(split_words("click \"open").is_err());
    }

    #[test]
    fn a_step_list_is_checked_line_by_line_before_it_is_sent() {
        let steps = read_steps(
            "--snapshot o1 click e7 --expect-download complete\n\n\
             click \"New expense\" --expect \"a form for a new expense is shown\"\n\
             type amount 388.50 --expect-field e4=388.50 --within 5\n",
        )
        .unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["snapshot"], "o1");
        assert_eq!(steps[0]["expect"], json!({"download": true}));
        assert_eq!(steps[1]["target"], "New expense");
        assert_eq!(
            steps[1]["expect"]["sentence"],
            "a form for a new expense is shown"
        );
        assert_eq!(steps[2]["text"], "388.50");
        assert_eq!(steps[2]["expect"]["within"], 5.0);
        let bad = read_steps("click e1\nhover e2\n").unwrap_err();
        assert!(bad.message.starts_with("line 2:"), "{}", bad.message);
        assert!(read_steps("\n# nothing\n").is_err());
        assert!(read_steps("click e1 --expect-download started").is_err());
    }

    #[test]
    fn a_lost_act_names_its_request_for_its_history() {
        let lost = Failure::new(Exit::Unknown, "outcome_unknown", "dropped".into());
        let named = with_request(lost, &json!("r00aa"));
        assert_eq!(named.details["request"], "r00aa");
        assert!(named.message.contains("act --history r00aa"));
        assert!(new_request_id().starts_with('r'));
        assert_ne!(new_request_id(), new_request_id());
    }

    #[test]
    fn act_reads_a_key_or_direction_without_a_target() {
        assert_eq!(
            words(ActAction::Press, Some("Enter"), None).unwrap(),
            (None, Some("Enter".into()))
        );
        assert_eq!(
            words(ActAction::Press, Some("e4"), Some("Enter")).unwrap(),
            (Some("e4".into()), Some("Enter".into()))
        );
        assert_eq!(
            words(ActAction::Scroll, Some("down"), None).unwrap(),
            (None, Some("down".into()))
        );
        assert_eq!(words(ActAction::Scroll, None, None).unwrap(), (None, None));
        assert_eq!(words(ActAction::Back, None, None).unwrap(), (None, None));
        assert!(words(ActAction::Click, None, None).is_err());
        assert!(words(ActAction::Back, Some("e1"), None).is_err());
        assert!(act_words(ActAction::Click, None, None, true).is_ok());
        assert!(act_words(ActAction::Type, Some("e1".into()), None, true).is_err());
    }
}
