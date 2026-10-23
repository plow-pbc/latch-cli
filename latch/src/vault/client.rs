//! `latch vault …`: local owner management, or calls to another host.
//! Phone links always use a running host's receiver.
//!
//! Secrets are typed at a prompt without echo, or read as JSON on stdin,
//! never taken from argv or the environment. An import shows what the
//! export holds, values reduced to whether they are there, and imports only
//! the rows the owner picks, at a prompt or with `--pick`; nothing is
//! picked by default.

use std::io::{IsTerminal, Read};
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
    let host = if hosts::local_target(state, args.host.as_deref())? {
        None
    } else {
        Some(hosts::resolve(state, args.host).await?)
    };
    let target = Target { state, host };
    let call = |args: Value| Request {
        op: "vault".to_string(),
        args,
        chat: None,
    };
    match args.command {
        VaultCommand::List => target.call(call(json!({"command": "list"}))).await,
        VaultCommand::Show { item } => {
            target
                .call(call(json!({"command": "show", "item": item})))
                .await
        }
        VaultCommand::Remove { item, revision } => {
            let request = call(json!({"command": "remove", "item": item, "revision": revision}));
            target.call(request).await
        }
        VaultCommand::Add {
            kind,
            title,
            site,
            other_top_site,
            field,
            ask,
            totp,
            stdin,
        } => {
            let values = match stdin {
                true => with_stdin(&kind, &field)?,
                false => values(&kind, &field, &ask, totp)?,
            };
            let request = call(json!({
                "command": "add",
                "kind": kind,
                "title": title,
                "sites": site,
                "other_top_sites": other_top_site,
                "values": values,
            }));
            target.call(request).await
        }
        VaultCommand::Edit {
            item,
            revision,
            title,
            site,
            no_sites,
            other_top_site,
            no_other_top_sites,
            field,
            ask,
            stdin,
        } => {
            let sites = replaced(site, no_sites);
            let other_top_sites = replaced(other_top_site, no_other_top_sites);
            if title.is_none()
                && sites.is_none()
                && other_top_sites.is_none()
                && field.is_empty()
                && ask.is_empty()
                && !stdin
            {
                return Err(Failure::usage(
                    "bad_arguments",
                    "say what to change: --title, --site, --no-sites, --other-top-site, \
                     --no-other-top-sites, --field, --ask or --stdin"
                        .into(),
                ));
            }
            // The item's kind says which fields it has and which are secret.
            let listed = target.call(call(json!({"command": "list"}))).await?;
            let kind = listed["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|meta| meta["item"] == item.as_str())
                .and_then(|meta| meta["kind"].as_str())
                .ok_or_else(|| Failure::usage("no_such_item", format!("there is no item {item}")))?
                .to_string();
            let values = match stdin {
                true => with_stdin(&kind, &field)?,
                false => edits(&kind, &field, &ask)?,
            };
            let mut request = json!({
                "command": "edit",
                "item": item,
                "revision": revision,
                "values": values,
            });
            if let Some(title) = title {
                request["title"] = json!(title);
            }
            if let Some(sites) = sites {
                request["sites"] = json!(sites);
            }
            if let Some(tops) = other_top_sites {
                request["other_top_sites"] = json!(tops);
            }
            target.call(call(request)).await
        }
        VaultCommand::Link {
            kind,
            title,
            site,
            field,
            ask,
            page,
        } => {
            let values = given(&kind, &field)?;
            let request = call(json!({
                "command": "link",
                "kind": kind,
                "title": title,
                "sites": site,
                "values": values,
                "ask": ask,
            }));
            let opened = target.call(request).await?;
            let url = link_url(&page, &opened);
            if term::interactive() {
                eprintln!("{}", term::qr(&url));
                eprintln!("Open on your phone; it works once, for 15 minutes.");
            }
            Ok(json!({"url": url, "link": opened["link"], "expires_in": opened["expires_in"]}))
        }
        VaultCommand::Import {
            file,
            preview,
            pick,
        } => {
            let Some(host) = &target.host else {
                return match preview {
                    true => super::local::preview(state, &file),
                    false => super::local::import(state, &file, pick.as_deref()),
                };
            };
            let link = dial::connect(state, host).await?;
            let result = import(&link.connection, &file, preview, pick.as_deref()).await;
            link.close();
            result
        }
    }
}

struct Target<'a> {
    state: &'a StateDir,
    host: Option<crate::plow::Peer>,
}

impl Target<'_> {
    async fn call(&self, request: Request) -> Result<Value, Failure> {
        if let Some(host) = &self.host {
            return dial::call(self.state, host, &request, ANSWER_WITHIN).await;
        }
        if request.args["command"] == "link" {
            super::local::link(self.state, request.args).await
        } else {
            super::local::call(self.state, request.args)
        }
    }
}

/// An edit's new site list: the sites given, or none when cleared; `None`
/// leaves the item's list as it is.
fn replaced(given: Vec<String>, clear: bool) -> Option<Vec<String>> {
    (clear || !given.is_empty()).then_some(given)
}

/// A new item's values: ordinary ones from `--field`, secret ones typed at
/// a prompt without echo. A secret on the command line is refused.
fn values(
    kind: &str,
    fields: &[String],
    ask: &[String],
    totp: bool,
) -> Result<Map<String, Value>, Failure> {
    let mut values = given(kind, fields)?;
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
    values.extend(prompted(kind, &asked, false)?);
    Ok(values)
}

/// An edit's changed values: `--field` ones (empty clears), and the
/// `--ask` ones typed at a prompt, where typing nothing is refused.
fn edits(kind: &str, fields: &[String], ask: &[String]) -> Result<Map<String, Value>, Failure> {
    let mut values = given(kind, fields)?;
    values.extend(prompted(kind, ask, true)?);
    Ok(values)
}

/// The labels a `kind` has, and whether each is secret.
fn labels(kind: &str) -> Result<Vec<(&'static str, bool)>, Failure> {
    let code = kind_code(kind).ok_or_else(|| {
        Failure::usage("bad_item", "--kind is login, card, identity or note".into())
    })?;
    Ok(entry_labels(code))
}

fn known(labels: &[(&'static str, bool)], kind: &str, label: &str) -> Option<(&'static str, bool)> {
    labels
        .iter()
        .find(|(l, _)| *l == label || (kind == "card" && label == "cvv" && *l == "code"))
        .copied()
}

/// `--field LABEL=VALUE` values; a secret label is refused, its value unread.
fn given(kind: &str, fields: &[String]) -> Result<Map<String, Value>, Failure> {
    let labels = labels(kind)?;
    let mut values = Map::new();
    for pair in fields {
        let (label, value) = pair.split_once('=').ok_or_else(|| {
            Failure::usage(
                "bad_arguments",
                format!("--field takes LABEL=VALUE, not {pair:?}"),
            )
        })?;
        let Some((_, secret)) = known(&labels, kind, label) else {
            return Err(Failure::usage(
                "bad_item",
                format!("a {kind} has no field {label:?}"),
            ));
        };
        if secret {
            return Err(Failure::usage(
                "secret_in_argv",
                format!(
                    "{label} is secret: it is asked for at a prompt or read with --stdin, never given on the command line"
                ),
            ));
        }
        values.insert(label.to_string(), json!(value));
    }
    Ok(values)
}

/// `--field` values and `--stdin` ones, which are never prompted for.
fn with_stdin(kind: &str, fields: &[String]) -> Result<Map<String, Value>, Failure> {
    let mut values = given(kind, fields)?;
    if std::io::stdin().is_terminal() {
        let message = "--stdin reads a JSON object piped in, e.g. \
                       `latch vault add … --stdin < values.json`; here stdin is a terminal";
        return Err(Failure::usage("stdin_is_terminal", message.into()));
    }
    let read = read_values(std::io::stdin(), kind)?;
    for (label, value) in read {
        if values.contains_key(&label) {
            return Err(Failure::usage(
                "bad_arguments",
                format!("{label} is given twice: in --field and on stdin"),
            ));
        }
        values.insert(label, value);
    }
    Ok(values)
}

/// The most `--stdin` reads: far more than any item's values.
const MAX_STDIN_VALUES: u64 = 1 << 20;

/// `--stdin`'s values: one JSON object of strings by the kind's labels. An
/// error never repeats what was read.
fn read_values(input: impl Read, kind: &str) -> Result<Map<String, Value>, Failure> {
    let bad = || {
        let message = "--stdin takes one JSON object of field values by label, each a string, \
                       e.g. {\"password\": \"…\"}";
        Failure::usage("bad_values", message.into())
    };
    let mut text = String::new();
    input
        .take(MAX_STDIN_VALUES)
        .read_to_string(&mut text)
        .map_err(|_| bad())?;
    let values: Map<String, Value> = serde_json::from_str(&text).map_err(|_| bad())?;
    let labels = labels(kind)?;
    for (label, value) in &values {
        if !value.is_string() {
            return Err(bad());
        }
        if known(&labels, kind, label).is_none() {
            return Err(Failure::usage(
                "bad_item",
                format!("a {kind} has no field {label:?}"),
            ));
        }
    }
    Ok(values)
}

/// Values typed at a prompt without echo, one per label. With `required`,
/// typing nothing is refused rather than read as "leave it out".
fn prompted(kind: &str, asked: &[String], required: bool) -> Result<Map<String, Value>, Failure> {
    let labels = labels(kind)?;
    for label in asked {
        if known(&labels, kind, label).is_none() {
            return Err(Failure::usage(
                "bad_item",
                format!("a {kind} has no field {label:?}"),
            ));
        }
    }
    let mut values = Map::new();
    for label in asked {
        let prompt = if label == "totp" {
            "authenticator key (or otpauth:// link)"
        } else {
            label.as_str()
        };
        let value = term::ask_secret(&format!("{prompt}: "));
        if value.is_empty() {
            if required {
                return Err(Failure::usage(
                    "nothing_typed",
                    format!("nothing was typed for {label}; leave out --ask {label} to keep it"),
                ));
            }
            continue;
        }
        values.insert(label.clone(), json!(value));
    }
    Ok(values)
}

/// The phone link: the page's address, then the host's key, its relay and
/// the code after `#`, which a browser never sends to the page's server.
fn link_url(page: &str, opened: &Value) -> String {
    let page = page.split('#').next().unwrap_or_default();
    let part = |key: &str| opened[key].as_str().unwrap_or_default().to_string();
    let relay: String = url::form_urlencoded::byte_serialize(part("relay").as_bytes()).collect();
    format!("{page}#h={}&r={relay}&c={}", part("host"), part("code"))
}

/// Sends the export and gets the preview; with `preview` that is the
/// answer, and the host keeps nothing. Otherwise picks rows (`pick`, or at
/// a prompt) and commits only those.
async fn import(
    connection: &crate::transport::Connection,
    path: &Path,
    preview: bool,
    pick: Option<&str>,
) -> Result<Value, Failure> {
    let local =
        |err: std::io::Error| Failure::usage("local_file", format!("{}: {err}", path.display()));
    let mut file = File::open(path).await.map_err(local)?;
    let size = file.metadata().await.map_err(local)?.len();
    let request = Request {
        op: "vault".to_string(),
        args: json!({"command": if preview { "preview" } else { "import" }, "size": size}),
        chat: None,
    };
    // One stream: the go-ahead, the bytes, the preview, the picks, the
    // outcome. The host drops the export if the stream ends before picks.
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    wire::copy(&mut file, &mut send, size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    if preview {
        send.finish().map_err(|_| dial::lost(connection, true))?;
        return dial::answer(connection, &mut recv, ANSWER_WITHIN).await;
    }
    let previewed = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let preview = &previewed["preview"];
    let picks = choose(preview, pick)?;
    let picked = serde_json::to_vec(&json!({"picks": picks})).expect("serializes");
    wire::write_frame(&mut send, &picked)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let mut outcome = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    outcome["source"] = preview["source"].clone();
    outcome["picked"] = json!(picks);
    Ok(outcome)
}

/// The rows `given` names (`--pick`), or those picked at a prompt.
pub(super) fn choose(preview: &Value, given: Option<&str>) -> Result<Vec<usize>, Failure> {
    let rows = preview["items"].as_array().map_or(0, Vec::len);
    if let Some(given) = given {
        return pick(given, rows);
    }
    show(preview);
    if rows == 0 {
        return Ok(Vec::new());
    }
    pick(
        &term::ask("Import which? Numbers or ranges (1,3-5), \"all\", or nothing to import none: "),
        rows,
    )
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
    fn the_link_carries_everything_after_the_hash() {
        let opened = json!({"host": "ab12", "relay": "https://relay.example/", "code": "c0de"});
        assert_eq!(
            link_url("http://localhost:8731/#old", &opened),
            "http://localhost:8731/#h=ab12&r=https%3A%2F%2Frelay.example%2F&c=c0de"
        );
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

    #[test]
    fn stdin_values_are_strings_by_the_kinds_labels_and_never_echoed() {
        let read = |text: &str, kind: &str| read_values(text.as_bytes(), kind);
        let values = read(r#"{"password": "made-up-9f3k", "totp": ""}"#, "login").unwrap();
        assert_eq!(values["password"], "made-up-9f3k");
        assert_eq!(values["totp"], "");
        assert_eq!(read(r#"{"cvv": "737"}"#, "card").unwrap()["cvv"], "737");
        for (text, code) in [
            (r#"{"password": 41551}"#, "bad_values"),
            (r#"["made-up-9f3k"]"#, "bad_values"),
            (r#"{"password": "made-up-9f3k""#, "bad_values"),
            (r#"{"pin": "made-up-9f3k"}"#, "bad_item"),
        ] {
            let refused = read(text, "login").unwrap_err();
            assert_eq!(refused.code, code, "{text}");
            assert!(!refused.message.contains("made-up-9f3k"), "{text}");
            assert!(!refused.message.contains("41551"), "{text}");
        }
    }

    #[test]
    fn a_site_list_is_kept_replaced_or_cleared() {
        assert_eq!(replaced(vec![], false), None);
        assert_eq!(
            replaced(vec!["shop.example".into()], false),
            Some(vec!["shop.example".to_string()])
        );
        assert_eq!(replaced(vec![], true), Some(vec![]));
    }

    #[test]
    fn an_edit_takes_no_secret_from_argv_and_clears_with_an_empty_value() {
        for (kind, field) in [
            ("login", "password=made-up-9f3k"),
            ("login", "totp=JBSWY3DPEHPK3PXP"),
            ("card", "number=4242424242424242"),
            ("identity", "ssn=078-05-1120"),
        ] {
            let refused = edits(kind, &[field.into()], &[]).err().unwrap();
            assert_eq!(refused.code, "secret_in_argv", "{field}");
            let value = field.split_once('=').unwrap().1;
            assert!(!refused.message.contains(value));
        }
        let cleared = edits("login", &["username=".into()], &[]).unwrap();
        assert_eq!(cleared["username"], "");
        // A field the kind lacks is refused before anything is asked.
        assert_eq!(
            edits("login", &[], &["code".into()]).err().unwrap().code,
            "bad_item"
        );
    }
}
