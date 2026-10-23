//! The host's `messages` and `contacts` operations: run the pinned reader in
//! a worker that may read the Messages and address book folders and nothing
//! else of the owner's, and hand back its answer. The audit keeps the
//! action, its arguments and the answer's counts, never a row.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::failure::{Failure, code};
use crate::serve::Done;
use crate::wire::Response;
use crate::worker::WorkerSetup;

/// Each connector's actions, as the reader names them after `contacts`.
pub fn actions(op: &str) -> &'static [&'static str] {
    match op {
        "messages" => &["search", "thread", "chats", "unreplied"],
        "contacts" => &["search", "lookup"],
        _ => &[],
    }
}

/// Refuses a `messages` or `contacts` call on a host that isn't a Mac,
/// before anything else about it is looked at.
pub fn supported_here(op: &str) -> Result<(), Failure> {
    if cfg!(target_os = "macos") || actions(op).is_empty() {
        return Ok(());
    }
    let mut failure = Failure::new(
        code::unsupported_platform,
        format!("this host is not a Mac; {op} are read on macOS hosts only"),
    );
    failure.next = Some("ask a macOS host with --host H; `latch status` lists your hosts".into());
    Err(failure)
}

/// The reader's sha256, when this build pins one (a release build does).
const READER_SHA256: Option<&str> = option_env!("LATCH_MESSAGES_READER_SHA256");

/// The reader stops itself after 10 s; past this, the host stops it.
const READER_LIMIT: Duration = Duration::from_secs(20);

/// More than the reader's own 8 MiB answer budget, less than a wire frame.
const MAX_ANSWER: u64 = 12 << 20;

/// The effective request for one `messages` or `contacts` call, as the
/// reader works it out from the caller's words (a cursor's filters
/// included), checked and with its handles canonical: what the gatekeeper
/// judges and `run` then runs. A request the reader refuses is that
/// refusal, judged by no one.
pub async fn plan(workers: &WorkerSetup, op: &str, args: &Value) -> Result<Value, Box<Done>> {
    let (action, words) = request(op, args).map_err(|failure| {
        let action = args["action"].as_str().unwrap_or("unknown");
        Box::new(refused(
            failure,
            audit(op, action, &args["args"], &Value::Null),
        ))
    })?;
    let kept = audit(op, &action, &json!(words), &Value::Null);
    let mut argv = Vec::new();
    if let Some(region) = region().await {
        argv.extend(["--region".to_string(), region]);
    }
    argv.push("plan".to_string());
    if op == "contacts" {
        argv.push("contacts".to_string());
    }
    argv.push(action.clone());
    argv.extend(words.iter().cloned());
    // Planning reads nothing of the owner's.
    let planned = read(workers, &[], &argv).await;
    match planned {
        Ok(answer) if answer["plan"].is_object() => {
            Ok(json!({"action": action, "args": words, "plan": answer["plan"]}))
        }
        Ok(_) => Err(Box::new(refused(
            Failure::new(code::reader_failed, "the reader gave no plan".to_string()),
            kept.clone(),
        ))),
        Err(failure) => Err(Box::new(refused(failure, kept))),
    }
}

/// Runs one planned call (`plan`'s request) for a caller the gatekeeper
/// allowed. `home` is the owner's home folder, whose Messages and address
/// book are read: Messages and the address book for `messages`, the
/// address book alone for `contacts`. Without `names` (Contacts is not
/// enabled), a `messages` call's worker never sees the address book, and
/// its answer says why names are missing.
pub async fn run(workers: &WorkerSetup, home: &Path, names: bool, op: &str, args: &Value) -> Done {
    let action = args["action"].as_str().unwrap_or(op);
    let mut audit = audit(op, action, &args["args"], &args["plan"]);
    let [messages, address_book] = folders(home);
    let mut argv = vec![
        "--store".to_string(),
        messages.join("chat.db").display().to_string(),
        "--addressbook".to_string(),
        address_book.display().to_string(),
        "--plan".to_string(),
        args["plan"].to_string(),
    ];
    if let Some(region) = region().await {
        argv.splice(0..0, ["--region".to_string(), region]);
    }
    let mut reads = reads(op, home);
    if !names {
        reads.retain(|folder| *folder != address_book);
    }
    let result = read(workers, &reads, &argv).await;
    match result {
        Ok(mut answer) => {
            if !names {
                answer["names_unavailable"] = json!("contacts_not_enabled");
            }
            audit["result"] = json!({
                "count": answer["count"],
                "unreadable": answer["unreadable"],
                "unchecked": answer["unchecked"],
                "incomplete": answer["incomplete"],
                "more": !answer["next"].is_null(),
            });
            Done {
                response: Response::Ok(answer),
                audit_args: audit,
                outcome: "ok".to_string(),
            }
        }
        Err(failure) => refused(failure, audit),
    }
}

/// The folders a connector's reader may read: `messages` names senders, so
/// it reads the address book too; `contacts` reads the address book alone.
/// Only folders that exist can be allowed; the reader reports the rest.
pub fn reads(op: &str, home: &Path) -> Vec<PathBuf> {
    let [messages, address_book] = folders(home);
    let folders = if op == "messages" {
        vec![messages, address_book]
    } else {
        vec![address_book]
    };
    folders
        .into_iter()
        .filter(|folder| folder.exists())
        .collect()
}

/// What the audit and the log keep of a call: its action, the caller's
/// words and its plan, minus what an answer put there. A cursor holds the
/// page before's last row (a contact's name, for `contacts search`), so a
/// cursor's token is not kept, and neither is a plan's position by name.
pub fn audit(op: &str, action: &str, words: &Value, plan: &Value) -> Value {
    let words: Vec<Value> = words
        .as_array()
        .into_iter()
        .flatten()
        .scan(false, |after_cursor, word| {
            let kept = if *after_cursor {
                json!("[cursor]")
            } else {
                word.clone()
            };
            *after_cursor = word == "--cursor";
            Some(kept)
        })
        .collect();
    let mut audit = json!({"action": format!("{op}.{action}"), "args": words});
    if !plan.is_null() {
        audit["plan"] = kept_plan(op, plan);
    }
    audit
}

/// A plan as the audit and the log keep it: a contacts page's position,
/// the name the page before ended on, is not kept.
pub fn kept_plan(op: &str, plan: &Value) -> Value {
    let mut plan = plan.clone();
    if op == "contacts" && plan["after"].is_object() {
        plan["after"]["key"] = json!("[name]");
    }
    plan
}

fn refused(failure: Failure, audit: Value) -> Done {
    Done {
        response: Response::error(&failure),
        audit_args: audit,
        outcome: failure.code,
    }
}

/// The action and the reader's words for it, checked for shape only: the
/// reader checks their meaning.
pub fn request(op: &str, args: &Value) -> Result<(String, Vec<String>), Failure> {
    let bad = |message: &str| Failure::new(code::bad_request, message.to_string());
    let action = args["action"].as_str().unwrap_or_default();
    if !actions(op).contains(&action) {
        return Err(bad(&format!("{op} takes {}", actions(op).join(", "))));
    }
    let words: Vec<String> = serde_json::from_value(args["args"].clone())
        .map_err(|_| bad("args must be a list of strings"))?;
    if words.len() > 128 || words.iter().any(|word| word.len() > 4096) {
        return Err(bad("too many arguments, or one too long"));
    }
    Ok((action.to_string(), words))
}

/// Where the reader is installed: the state folder's plugins, in a folder
/// named for this latch's version, which installed it.
pub fn reader(state_dir: &Path) -> PathBuf {
    state_dir
        .join("plugins/messages")
        .join(env!("CARGO_PKG_VERSION"))
        .join("latch-messages")
}

/// The folders the reader may read under `home`.
pub fn folders(home: &Path) -> [PathBuf; 2] {
    [
        home.join("Library/Messages"),
        home.join("Library/Application Support/AddressBook"),
    ]
}

/// Runs the reader with `argv` in a Messages worker that may read `reads`,
/// and returns its answer.
pub async fn read(
    workers: &WorkerSetup,
    reads: &[PathBuf],
    argv: &[String],
) -> Result<Value, Failure> {
    let program = reader(&workers.state_dir);
    verify(&program, READER_SHA256)?;
    let mut child = workers
        .messages(&program, reads, argv)
        .map_err(|error| Failure::reported(&error.code, error.message))?;
    let mut stdout = child.stdout.take().expect("piped").take(MAX_ANSWER + 1);
    let mut stderr = child.stderr.take().expect("piped").take(64 << 10);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ran = tokio::time::timeout(READER_LIMIT, async {
        let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        child.wait().await
    })
    .await;
    let Ok(Ok(status)) = ran else {
        let message = format!(
            "the reader did not finish within {}s",
            READER_LIMIT.as_secs()
        );
        return Err(Failure::new(code::reader_timeout, message));
    };
    if status.success() {
        if out.len() as u64 > MAX_ANSWER {
            return Err(Failure::new(
                code::reader_failed,
                "the reader's answer is too large".to_string(),
            ));
        }
        return serde_json::from_slice(&out).map_err(|_| {
            Failure::new(
                code::reader_failed,
                "the reader's answer is not JSON".to_string(),
            )
        });
    }
    Err(reader_error(&String::from_utf8_lossy(&err)))
}

/// The region of the owner's macOS locale (`en_GB` gives `GB`), if it names one.
async fn region() -> Option<String> {
    let output = tokio::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let locale = String::from_utf8(output.stdout).ok()?;
    let region = locale.trim().split('@').next()?.rsplit('_').next()?;
    (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| region.to_ascii_uppercase())
}

/// The reader's error line, `{"error": {"code", "message", "exit", ...}}`,
/// or the worker's own, as a failure. Neither holds message content.
fn reader_error(stderr: &str) -> Failure {
    if let Some(error) = crate::worker::worker_error(stderr) {
        return Failure::reported(&error.code, error.message);
    }
    let line: Value = stderr
        .lines()
        .last()
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or(Value::Null);
    let error = &line["error"];
    let Some(code) = error["code"].as_str() else {
        return Failure::new(
            code::reader_failed,
            "the reader stopped without saying why".to_string(),
        );
    };
    let message = error["message"].as_str().unwrap_or_default().to_string();
    let mut failure = Failure::reported(code, message);
    let mut details = error.clone();
    if let Some(object) = details.as_object_mut() {
        for key in ["code", "message", "exit"] {
            object.remove(key);
        }
        if !object.is_empty() {
            failure.details = details;
        }
    }
    failure
}

/// The reader must be there and, in a build that pins it (`pinned`), be
/// the one this latch was released with.
fn verify(program: &Path, pinned: Option<&str>) -> Result<(), Failure> {
    let missing = || {
        let message = format!(
            "the Messages reader is not installed at {}; run install.sh",
            program.display()
        );
        Failure::new(code::messages_not_installed, message)
    };
    let mut file = std::fs::File::open(program).map_err(|_| missing())?;
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| missing())?;
    let digest = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    if digest != pinned {
        let message =
            "the installed Messages reader is not the one this latch was built with".to_string();
        return Err(Failure::new(code::messages_unverified, message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;

    #[test]
    fn a_request_names_one_of_its_actions_and_gives_strings() {
        let ok = request(
            "messages",
            &json!({"action": "search", "args": ["--from", "+1"]}),
        )
        .unwrap();
        assert_eq!(
            ok,
            (
                "search".to_string(),
                vec!["--from".to_string(), "+1".to_string()]
            )
        );
        assert!(request("contacts", &json!({"action": "lookup", "args": ["a"]})).is_ok());
        for bad in [
            json!({"action": "send", "args": []}),
            json!({"action": "lookup", "args": []}),
            json!({"action": "search", "args": [1]}),
            json!({"action": "search"}),
        ] {
            assert_eq!(
                request("messages", &bad).unwrap_err().code,
                "bad_request",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_audit_keeps_no_cursor_and_no_name_from_a_page() {
        let words = json!(["--cursor", "eyJrZXkiOiJBbm4gRXhhbXBsZSJ9", "--limit", "5"]);
        let plan = json!({"command": "contacts search", "text": "a", "after": {"key": "Ann Example", "id": 0}});
        let kept = audit("contacts", "search", &words, &plan);
        assert_eq!(kept["action"], "contacts.search");
        assert_eq!(
            kept["args"],
            json!(["--cursor", "[cursor]", "--limit", "5"])
        );
        assert_eq!(kept["plan"]["after"], json!({"key": "[name]", "id": 0}));
        assert_eq!(kept["plan"]["text"], "a");
        assert!(!kept.to_string().contains("Ann") && !kept.to_string().contains("eyJ"));
        // A messages page is placed by a date, which is kept.
        let plan = json!({"command": "search", "after": {"key": "782000000000000000", "id": 103}});
        let kept = audit("messages", "search", &json!(["--cursor", "x"]), &plan);
        assert_eq!(kept["plan"]["after"]["key"], "782000000000000000");
        assert_eq!(kept["args"], json!(["--cursor", "[cursor]"]));
        assert!(
            audit("messages", "chats", &json!([]), &Value::Null)
                .get("plan")
                .is_none()
        );
    }

    #[test]
    fn only_the_pinned_reader_runs() {
        let dir = tempfile::tempdir().unwrap();
        let program = reader(dir.path());
        assert_eq!(
            verify(&program, None).unwrap_err().code,
            "messages_not_installed"
        );
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"a reader").unwrap();
        let sha = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(b"a reader"));
        assert!(verify(&program, Some(&sha)).is_ok());
        let other = verify(&program, Some(&"0".repeat(64))).unwrap_err();
        assert_eq!(
            (other.exit, other.code.as_str()),
            (Exit::Denied, "messages_unverified")
        );
        // An unpinned build (a developer's) runs whatever is installed.
        assert!(verify(&program, None).is_ok());
        assert!(program.ends_with(format!(
            "plugins/messages/{}/latch-messages",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn reader_errors_keep_their_code_exit_and_details() {
        let stderr = r#"{"error":{"code":"ambiguous_chat","exit":2,"message":"pick one","chats":[{"id":1}]}}"#;
        let failure = reader_error(&format!("noise\n{stderr}"));
        assert_eq!(
            (failure.exit, failure.code.as_str()),
            (Exit::Usage, "ambiguous_chat")
        );
        assert_eq!(failure.details, json!({"chats": [{"id": 1}]}));
        assert_eq!(reader_error("").code, "reader_failed");
    }
}
