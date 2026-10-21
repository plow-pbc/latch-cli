//! The host's `messages` and `contacts` operations: run the pinned reader in
//! a worker that may read the Messages and address book folders and nothing
//! else of the owner's, and hand back its answer. The audit keeps the
//! action, its arguments and the answer's counts, never a row.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::failure::{Exit, Failure};
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

/// The reader's sha256, when this build pins one (a release build does).
const READER_SHA256: Option<&str> = option_env!("LATCH_MESSAGES_READER_SHA256");

/// The reader stops itself after 10 s; past this, the host stops it.
const READER_LIMIT: Duration = Duration::from_secs(20);

/// More than the reader's own 8 MiB answer budget, less than a wire frame.
const MAX_ANSWER: u64 = 12 << 20;

/// Runs one `messages` or `contacts` call for a caller the gatekeeper
/// allowed. `home` is the owner's home folder, whose Messages and address
/// book are read.
pub async fn run(workers: &WorkerSetup, home: &Path, op: &str, args: &Value) -> Done {
    let (action, words) = match request(op, args) {
        Ok(request) => request,
        Err(failure) => return refused(failure, json!({"action": op})),
    };
    let mut audit = json!({"action": format!("{op}.{action}"), "args": words});
    let result = answer(workers, home, op, &action, &words).await;
    match result {
        Ok(answer) => {
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
    let bad = |message: &str| Failure::usage("bad_request", message.to_string());
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

async fn answer(
    workers: &WorkerSetup,
    home: &Path,
    op: &str,
    action: &str,
    words: &[String],
) -> Result<Value, Failure> {
    if !cfg!(target_os = "macos") {
        let message = "Messages and contacts are read on macOS hosts only".to_string();
        return Err(Failure::failed("messages_unavailable", message));
    }
    let program = reader(&workers.state_dir);
    verify(&program, READER_SHA256)?;
    let [messages, address_book] = folders(home);
    let mut argv = vec![
        "--store".to_string(),
        messages.join("chat.db").display().to_string(),
        "--addressbook".to_string(),
        address_book.display().to_string(),
    ];
    // The owner's region, for phone numbers written without a country
    // code: the reader cannot ask macOS from inside its sandbox.
    if let Some(region) = region().await {
        argv.extend(["--region".to_string(), region]);
    }
    if op == "contacts" {
        argv.push("contacts".to_string());
    }
    argv.push(action.to_string());
    argv.extend(words.iter().cloned());
    // Only the folders that exist can be allowed; the reader reports the rest.
    let reads: Vec<PathBuf> = [messages, address_book]
        .into_iter()
        .filter(|folder| folder.exists())
        .collect();
    let mut child = workers
        .messages(&program, &reads, &argv)
        .map_err(|error| Failure::failed(&error.code, error.message))?;
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
        return Err(Failure::failed("reader_timeout", message));
    };
    if status.success() {
        if out.len() as u64 > MAX_ANSWER {
            return Err(Failure::failed(
                "reader_failed",
                "the reader's answer is too large".to_string(),
            ));
        }
        return serde_json::from_slice(&out).map_err(|_| {
            Failure::failed(
                "reader_failed",
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
        return Failure::failed(&error.code, error.message);
    }
    let line: Value = stderr
        .lines()
        .last()
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or(Value::Null);
    let error = &line["error"];
    let Some(code) = error["code"].as_str() else {
        return Failure::failed(
            "reader_failed",
            "the reader stopped without saying why".to_string(),
        );
    };
    let exit = if error["exit"] == 2 {
        Exit::Usage
    } else {
        Exit::Failed
    };
    let mut failure = Failure::new(
        exit,
        code,
        error["message"].as_str().unwrap_or_default().to_string(),
    );
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
        Failure::failed("messages_not_installed", message)
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
        return Err(Failure::new(Exit::Denied, "messages_unverified", message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
