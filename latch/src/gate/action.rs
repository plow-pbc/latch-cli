//! What the judge is shown of an agent's exec, file or Google operation,
//! and what the audit keeps of one the judge refused.

use std::path::Path;

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::failure::Failure;

/// The most stdin an agent's exec may bring: the judge reads all of it,
/// and the whole judgment must fit Plow's decision route (60 KiB).
pub const JUDGED_STDIN: usize = 16 * 1024;

/// An operation the gatekeeper judges.
pub struct Gated {
    /// What the judge is shown.
    pub action: Box<RawValue>,
    /// What the audit keeps if it is refused: no Google arguments, which
    /// can hold a message body or a search.
    pub audit: Value,
    /// Whether it is a one-time action, which the judge then sees as
    /// already done: a Google command that changes something. Running,
    /// reading or searching again is normal, so exec, files and Google
    /// reads are not.
    pub once: bool,
    /// Why the judge cannot be asked at all: an exec's stdin it cannot read.
    pub unjudgeable: Option<Failure>,
}

/// The proposed action, its keys in this order.
#[derive(Serialize)]
struct Action<'a> {
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdin: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_folder: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    writes: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accounts: Option<&'a Value>,
}

/// `op` with `args` as the judge sees it, if the gatekeeper judges `op`.
/// An exec without a working folder runs in `first_area`, if the host
/// shares one. An exec's stdin
/// may be the program itself (`sh`, `python -`), so the judge reads it
/// too; the audit keeps only its size and sha256.
pub fn gated(op: &str, args: &Value, first_area: Option<&Path>) -> Option<Gated> {
    let mut unjudgeable = None;
    let stdin = match op {
        "exec" => judged_stdin(&args["stdin"]).unwrap_or_else(|failure| {
            unjudgeable = Some(failure);
            None
        }),
        _ => None,
    };
    let none = Action {
        operation: op,
        argv: None,
        stdin: None,
        working_folder: None,
        writes: None,
        network: None,
        path: None,
        size: None,
        file_name: None,
        command: None,
        accounts: None,
    };
    let first_area = first_area.map(Path::to_string_lossy);
    let mut once = false;
    let (action, audit) = match op {
        "exec" => {
            let folder = args["workdir"].as_str().or(first_area.as_deref());
            let action = Action {
                argv: Some(&args["argv"]),
                stdin: stdin.as_deref(),
                working_folder: folder,
                writes: Some(&args["writes"]),
                network: Some(&args["network"]),
                ..none
            };
            let mut audit = json!({"argv": args["argv"], "workdir": args["workdir"],
                                   "writes": args["writes"], "network": args["network"]});
            if let Ok(bytes) = serde_json::from_value::<Vec<u8>>(args["stdin"].clone()) {
                audit["stdin"] = crate::exec::stdin_audit(&bytes);
            }
            (action, audit)
        }
        "file_get" => {
            let action = Action {
                path: Some(&args["remote_path"]),
                ..none
            };
            (action, json!({"path": args["remote_path"]}))
        }
        "file_put" => {
            let action = Action {
                path: Some(&args["remote_path"]),
                size: Some(&args["size"]),
                file_name: Some(&args["name"]),
                ..none
            };
            (
                action,
                json!({"path": args["remote_path"], "size": args["size"]}),
            )
        }
        "google" => {
            let action = Action {
                command: Some(&args["gog_args"]),
                accounts: Some(&args["account"]),
                ..none
            };
            let words: Vec<String> =
                serde_json::from_value(args["gog_args"].clone()).unwrap_or_default();
            let command = crate::google::policy::command_prefix(&words);
            once = crate::google::policy::parse(&words).map_or(true, |plan| !plan.read_only());
            (action, json!({"command": command}))
        }
        _ => return None,
    };
    Some(Gated {
        action: serde_json::value::to_raw_value(&action).expect("an action serializes"),
        audit,
        once,
        unjudgeable,
    })
}

/// An exec's stdin as the judge reads it: text, at most `JUDGED_STDIN`
/// bytes. None when it brings none.
fn judged_stdin(stdin: &Value) -> Result<Option<String>, Failure> {
    if stdin.is_null() {
        return Ok(None);
    }
    let refused = |why: &str| {
        let message = format!(
            "this agent's exec stdin is {why}, and the gatekeeper must read all of it: \
             send at most {} KiB of UTF-8 text, or put the input in a file and name it",
            JUDGED_STDIN / 1024
        );
        Failure::usage("stdin_not_judgeable", message)
    };
    let bytes: Vec<u8> = serde_json::from_value(stdin.clone()).map_err(|_| refused("not bytes"))?;
    if bytes.len() > JUDGED_STDIN {
        return Err(refused(&format!("{} bytes", bytes.len())));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| refused("not UTF-8 text"))
}
