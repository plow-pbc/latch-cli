//! `latch vault …`: the owner's commands, run from an owner device (a latch
//! client state logged in as a device, never the serving host's own state)
//! and sent to the host over latch's connection.
//!
//! Secrets are typed at a prompt without echo, never taken from argv or the
//! environment. An import shows what the export holds, values reduced to
//! whether they are there, and imports only the rows the owner picks;
//! nothing is picked by default.

use std::path::Path;

use serde_json::{Map, Value, json};
use tokio::fs::File;

use super::item::{entry_labels, kind_code};
use crate::cli::{VaultArgs, VaultCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::state::StateDir;
use crate::term;
use crate::wire::{self, Request};

pub async fn run(state: &StateDir, args: VaultArgs) -> Result<Value, Failure> {
    let host = hosts::resolve(state, args.host).await?;
    let call = |args: Value| Request {
        op: "vault".to_string(),
        args,
    };
    match args.command {
        VaultCommand::List => {
            dial::call(
                state,
                &host,
                &call(json!({"command": "list"})),
                ANSWER_WITHIN,
            )
            .await
        }
        VaultCommand::Remove { item, revision } => {
            let request = call(json!({"command": "remove", "item": item, "revision": revision}));
            dial::call(state, &host, &request, ANSWER_WITHIN).await
        }
        VaultCommand::Add {
            kind,
            title,
            site,
            other_top_site,
            field,
            ask,
            totp,
        } => {
            let values = values(&kind, &field, &ask, totp)?;
            let request = call(json!({
                "command": "add",
                "kind": kind,
                "title": title,
                "sites": site,
                "other_top_sites": other_top_site,
                "values": values,
            }));
            dial::call(state, &host, &request, ANSWER_WITHIN).await
        }
        VaultCommand::Import { file } => {
            let link = dial::connect(state, &host).await?;
            let result = import(&link.connection, &file).await;
            link.close().await;
            result
        }
    }
}

/// An item's values: ordinary ones from `--field`, secret ones typed at a
/// prompt without echo. A secret on the command line is refused.
fn values(
    kind: &str,
    fields: &[String],
    ask: &[String],
    totp: bool,
) -> Result<Map<String, Value>, Failure> {
    let code = kind_code(kind).ok_or_else(|| {
        Failure::usage("bad_item", "--kind is login, card, identity or note".into())
    })?;
    let labels = entry_labels(code);
    let known = |label: &str| {
        labels
            .iter()
            .find(|(l, _)| *l == label || (kind == "card" && label == "cvv" && *l == "code"))
    };
    let mut values = Map::new();
    for pair in fields {
        let (label, value) = pair.split_once('=').ok_or_else(|| {
            Failure::usage(
                "bad_arguments",
                format!("--field takes LABEL=VALUE, not {pair:?}"),
            )
        })?;
        let Some((_, secret)) = known(label) else {
            return Err(Failure::usage(
                "bad_item",
                format!("a {kind} has no field {label:?}"),
            ));
        };
        if *secret {
            return Err(Failure::usage(
                "secret_in_argv",
                format!(
                    "{label} is secret: it is asked for at a prompt, never given on the command line"
                ),
            ));
        }
        values.insert(label.to_string(), json!(value));
    }
    // What every kind is asked for, and what the owner adds with --ask.
    let mut asked: Vec<String> = match kind {
        "login" => vec!["password".into()],
        "card" => vec!["number".into(), "code".into()],
        _ => Vec::new(),
    };
    if totp {
        asked.push("totp".into());
    }
    asked.extend(ask.iter().cloned());
    for label in asked {
        if known(&label).is_none() && label != "notes" {
            return Err(Failure::usage(
                "bad_item",
                format!("a {kind} has no field {label:?}"),
            ));
        }
        let prompt = if label == "totp" {
            "authenticator key (or otpauth:// link)".to_string()
        } else {
            label.clone()
        };
        let value = term::ask_secret(&format!("{prompt}: "));
        if !value.is_empty() {
            values.insert(label, json!(value));
        }
    }
    Ok(values)
}

/// Sends the export, shows the preview, asks which rows to import, and
/// commits only those.
async fn import(connection: &iroh::endpoint::Connection, path: &Path) -> Result<Value, Failure> {
    let local =
        |err: std::io::Error| Failure::usage("local_file", format!("{}: {err}", path.display()));
    let mut file = File::open(path).await.map_err(local)?;
    let size = file.metadata().await.map_err(local)?.len();
    let request = Request {
        op: "vault".to_string(),
        args: json!({"command": "import_preview", "size": size}),
    };
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    wire::copy(&mut file, &mut send, size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let previewed = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let preview = &previewed["preview"];
    show(preview);
    let rows = preview["items"].as_array().map_or(0, Vec::len);
    let picks = if rows == 0 {
        Vec::new()
    } else {
        pick(
            &term::ask(
                "Import which? Numbers or ranges (1,3-5), \"all\", or nothing to import none: ",
            ),
            rows,
        )?
    };
    let request = Request {
        op: "vault".to_string(),
        args: json!({"command": "import_commit", "import": previewed["import"], "picks": picks}),
    };
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let mut outcome = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    outcome["source"] = preview["source"].clone();
    outcome["picked"] = json!(picks);
    Ok(outcome)
}

/// The preview, for the owner, on stderr: stdout stays the command's JSON.
fn show(preview: &Value) {
    eprintln!("{} export:", preview["source"].as_str().unwrap_or("this"));
    for row in preview["items"].as_array().into_iter().flatten() {
        let mut facts = Vec::new();
        if row["has_password"] == true {
            facts.push("password");
        }
        if row["has_totp"] == true {
            facts.push("authenticator key");
        }
        let state = if row["duplicate"] == true {
            " (already in the vault)".to_string()
        } else if row["changed"].as_array().is_some_and(|c| !c.is_empty()) {
            format!(" (updates the vault's {})", row["changed"])
        } else {
            String::new()
        };
        let vault = row["vault"]["name"]
            .as_str()
            .map(|v| format!(" [{v}]"))
            .unwrap_or_default();
        eprintln!(
            "  {:>3}. {} {}{} {} {}{}{}",
            row["pick"],
            row["kind"].as_str().unwrap_or(""),
            term::one_line(row["title"].as_str().unwrap_or("")),
            vault,
            term::one_line(row["username"].as_str().unwrap_or("")),
            term::one_line(row["url"].as_str().unwrap_or("")),
            if facts.is_empty() {
                String::new()
            } else {
                format!(" ({})", facts.join(", "))
            },
            state
        );
        for warning in row["warnings"].as_array().into_iter().flatten() {
            eprintln!("       {}", warning.as_str().unwrap_or(""));
        }
    }
    for skipped in preview["skipped"].as_array().into_iter().flatten() {
        eprintln!(
            "  set aside: {}: {}",
            term::one_line(skipped["title"].as_str().unwrap_or("")),
            skipped["reason"].as_str().unwrap_or("")
        );
    }
}

/// `1,3-5`, `all`, or nothing (none).
pub fn pick(answer: &str, rows: usize) -> Result<Vec<usize>, Failure> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(Vec::new());
    }
    if answer.eq_ignore_ascii_case("all") {
        return Ok((1..=rows).collect());
    }
    let bad = || {
        Failure::new(
            Exit::Usage,
            "bad_pick",
            format!("{answer:?} is not a pick of rows 1-{rows}"),
        )
    };
    let mut picks = Vec::new();
    for part in answer.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (from, to) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim().parse::<usize>().map_err(|_| bad())?,
                b.trim().parse::<usize>().map_err(|_| bad())?,
            ),
            None => {
                let n = part.parse::<usize>().map_err(|_| bad())?;
                (n, n)
            }
        };
        if from == 0 || to > rows || from > to {
            return Err(bad());
        }
        for n in from..=to {
            if !picks.contains(&n) {
                picks.push(n);
            }
        }
    }
    Ok(picks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_takes_numbers_ranges_all_or_nothing() {
        assert_eq!(pick("", 4).unwrap(), Vec::<usize>::new());
        assert_eq!(pick("all", 3).unwrap(), [1, 2, 3]);
        assert_eq!(pick("1, 3-4,3", 4).unwrap(), [1, 3, 4]);
        assert!(pick("0", 4).is_err());
        assert!(pick("5", 4).is_err());
        assert!(pick("2-1", 4).is_err());
        assert!(pick("x", 4).is_err());
    }

    #[test]
    fn secrets_are_never_taken_from_argv() {
        let refused = values("login", &["password=hunter2".into()], &[], false)
            .err()
            .unwrap();
        assert_eq!(refused.code, "secret_in_argv");
        assert!(!refused.message.contains("hunter2"));
        let refused = values("card", &["cvv=737".into()], &[], false)
            .err()
            .unwrap();
        assert_eq!(refused.code, "secret_in_argv");
        assert_eq!(
            values("identity", &["shoe=9".into()], &[], false)
                .err()
                .unwrap()
                .code,
            "bad_item"
        );
    }
}
