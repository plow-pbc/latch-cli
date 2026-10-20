//! What the judge is shown of an agent's exec, file or Google operation,
//! and what the audit keeps of one the judge refused.

use std::path::Path;

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

/// An operation the gatekeeper judges.
pub struct Gated {
    /// What the judge is shown.
    pub action: Box<RawValue>,
    /// What the audit keeps if it is refused: no Google arguments, which
    /// can hold a message body or a search.
    pub audit: Value,
}

/// The proposed action, its keys in this order.
#[derive(Serialize)]
struct Action<'a> {
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_folder: Option<&'a str>,
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
/// An exec without a working folder runs in `first_area`.
pub fn gated(op: &str, args: &Value, first_area: &Path) -> Option<Gated> {
    let none = Action {
        operation: op,
        argv: None,
        working_folder: None,
        path: None,
        size: None,
        file_name: None,
        command: None,
        accounts: None,
    };
    let first_area = first_area.to_string_lossy();
    let (action, audit) = match op {
        "exec" => {
            let folder = args["workdir"].as_str().unwrap_or(&first_area);
            let action = Action {
                argv: Some(&args["argv"]),
                working_folder: Some(folder),
                ..none
            };
            let argv: Vec<String> =
                serde_json::from_value(args["argv"].clone()).unwrap_or_default();
            let audit =
                json!({"argv": crate::exec::capped_argv(&argv), "workdir": args["workdir"]});
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
            (action, json!({"command": command}))
        }
        _ => return None,
    };
    Some(Gated {
        action: serde_json::value::to_raw_value(&action).expect("an action serializes"),
        audit,
    })
}
