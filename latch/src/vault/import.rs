//! Imports: the exports other password managers write, read into vault
//! items, ported from the Latch app with its rules and tests.
//!
//! Formats: the CSVs Apple Passwords, 1Password, Chrome and Bitwarden
//! write (and any CSV with Title, URL, Username and Password columns),
//! 1Password's "Copy item JSON", 1Password's 1PUX export, and Bitwarden's
//! JSON export. Logins first; Bitwarden's exports also carry cards,
//! identities and notes, which come along, and every other kind is set
//! aside with its reason, never squeezed into a login.
//!
//! The owner picks what to import from a preview; nothing is picked by
//! default. A rule that holds everywhere here: no preview, warning, skip
//! reason or error ever contains a field's value.

use std::io::{Cursor, Read};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::item::{CARD, IDENTITY, Item, LOGIN, Login, NOTE, Raw, Uri};
use super::sites;
use super::store::Backend;
use super::totp::Params;
use crate::failure::Failure;

/// Larger than any real export; a cap, not a target.
pub const MAX_IMPORT_BYTES: usize = 20 * 1024 * 1024;
/// The most a 1PUX's `export.data` may inflate to.
const MAX_INFLATED_BYTES: u64 = 64 * 1024 * 1024;
const NOT_1PUX: &str = "this doesn't look like a 1PUX export. In 1Password choose File > Export, pick your account, and choose the 1PUX format";

/// The 1Password vault a row came from (1PUX only).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Source {
    pub id: String,
    pub name: String,
}

/// One item as the export described it, ready to save.
#[derive(Clone, Debug)]
pub struct Imported {
    pub item: Item,
    /// The first site's address as written, for the preview.
    pub url: String,
    pub warnings: Vec<String>,
    pub vault: Option<Source>,
    /// The vault already holds this item with the same secrets.
    pub duplicate: bool,
    /// The vault holds this login with a changed password or key: saving it
    /// edits that item, only these fields, at the revision read here.
    pub update: Option<Update>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub item: String,
    pub revision: u64,
    pub password: bool,
    pub totp: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Skipped {
    pub title: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<Source>,
}

pub struct Parsed {
    pub source: String,
    pub items: Vec<Imported>,
    pub skipped: Vec<Skipped>,
}

fn usage(message: &str) -> Failure {
    Failure::usage("bad_import", message.to_string())
}

/// Reads an export: a zip is a 1PUX, text opening with `{` is JSON
/// (Bitwarden's export or 1Password's item), anything else is CSV.
pub fn parse(bytes: &[u8]) -> Result<Parsed, Failure> {
    if bytes.starts_with(b"PK\x03\x04") {
        return parse_1pux(bytes);
    }
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(usage("that is too large to be a passwords export"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| usage("that export is not text"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lead = text.trim_start();
    if lead.is_empty() {
        return Err(usage("there is nothing to read"));
    }
    if lead.starts_with('{') {
        let value: Value = serde_json::from_str(lead).map_err(|_| {
            usage("that is not readable as JSON. From 1Password, right-click the item, choose \"Copy item JSON\", and paste all of it; from Bitwarden, export the vault as unencrypted JSON")
        })?;
        if value.get("items").is_some() {
            return parse_bitwarden_json(&value);
        }
        return parse_onepassword_item(&value);
    }
    parse_csv(text)
}

/// RFC 4180 rows, as both Apple and 1Password write them; blank lines are
/// not empty logins.
pub fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, Failure> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|_| usage("that CSV could not be read"))?;
        let row: Vec<String> = record.iter().map(str::to_string).collect();
        if row.len() == 1 && row[0].trim().is_empty() {
            continue;
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Header cells the known exports use, folded to this vault's fields.
fn header_key(cell: &str) -> Option<&'static str> {
    Some(match cell {
        "title" | "name" => "title",
        "url" | "urls" | "website" | "login_uri" => "url",
        "username" | "login_username" => "username",
        "password" | "login_password" => "password",
        "notes" | "note" => "notes",
        "otpauth" | "totp" | "login_totp" => "totp",
        "archived" => "archived",
        "type" => "type",
        "favorite" | "tags" | "folder" | "fields" | "reprompt" => "ignored",
        _ => return None,
    })
}

const APPLE_HEADER: &str = "title,url,username,password,notes,otpauth";
const ONEPW_HEADER: &str = "title,url,username,password,otpauth,favorite,archived,tags,notes";
const CHROME_HEADER: &str = "name,url,username,password,note";
const BITWARDEN_HEADER: &str = "folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp";

fn parse_csv(text: &str) -> Result<Parsed, Failure> {
    let rows = csv_rows(text)?;
    let header: Vec<String> = rows
        .first()
        .map(|row| row.iter().map(|h| h.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let joined = header.join(",");
    let mapped: Vec<Option<&str>> = header.iter().map(|h| header_key(h)).collect();
    let has = |key: &str| mapped.contains(&Some(key));
    if !(has("title") && has("url") && has("username") && has("password")) {
        return Err(usage(
            "this doesn't look like a passwords export. Expected the CSV that Apple Passwords, 1Password, Chrome or Bitwarden writes, with Title, URL, Username and Password columns",
        ));
    }
    let source = match joined.as_str() {
        APPLE_HEADER => "Apple Passwords",
        ONEPW_HEADER => "1Password",
        CHROME_HEADER => "Chrome",
        BITWARDEN_HEADER => "Bitwarden",
        _ => "CSV",
    };
    let mut parsed = Parsed {
        source: source.into(),
        items: Vec::new(),
        skipped: Vec::new(),
    };
    for row in rows.iter().skip(1) {
        let mut record: Map<String, Value> = Map::new();
        for (cell, key) in row.iter().zip(&mapped) {
            if let Some(key) = key
                && !record.contains_key(*key)
            {
                record.insert((*key).into(), json!(cell));
            }
        }
        let get = |key: &str| {
            record
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let title = get("title").trim().to_string();
        // Bitwarden's CSV carries secure notes too: a note is a note.
        if get("type") == "note" {
            let mut item = blank(NOTE, &title);
            item.raw.notes = Some(get("notes")).filter(|n| !n.is_empty());
            push_other(&mut parsed, item, None);
            continue;
        }
        if !get("type").is_empty() && get("type") != "login" {
            parsed.skipped.push(Skipped {
                title: if title.is_empty() {
                    "(untitled)".into()
                } else {
                    title
                },
                reason: "not a login".into(),
                vault: None,
            });
            continue;
        }
        let url = get("url").trim().to_string();
        let username = get("username").trim().to_string();
        let password = get("password");
        if title.is_empty() && url.is_empty() && username.is_empty() && password.is_empty() {
            continue;
        }
        if truthy(&get("archived")) {
            skip(&mut parsed, &title, "archived in 1Password", None);
            continue;
        }
        // Bitwarden's CSV may list several sites in one cell.
        let urls: Vec<String> = url
            .split(',')
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(String::from)
            .collect();
        login_row(
            &mut parsed,
            &title,
            &urls,
            &username,
            &password,
            &get("totp"),
            &get("notes"),
            None,
            Vec::new(),
        );
    }
    Ok(parsed)
}

fn truthy(cell: &str) -> bool {
    matches!(cell.trim().to_lowercase().as_str(), "true" | "yes" | "1")
}

fn skip(parsed: &mut Parsed, title: &str, reason: &str, vault: Option<Source>) {
    parsed.skipped.push(Skipped {
        title: if title.is_empty() {
            "(untitled)".into()
        } else {
            title.into()
        },
        reason: reason.into(),
        vault,
    });
}

fn blank(kind: u8, title: &str) -> Item {
    Item {
        id: String::new(),
        revision: 0,
        sites: Vec::new(),
        other_top_sites: Vec::new(),
        raw: Raw {
            kind,
            name: title.to_string(),
            ..Raw::default()
        },
    }
}

/// A site as `https://host`, and its registrable domain; `None` when no
/// site latch fills on can be read from it.
fn read_site(raw: &str) -> Option<(String, String)> {
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let domain = sites::registrable(&candidate).ok()?;
    Some((candidate, domain))
}

/// The last mile every login row walks: sites read (one bad one does not
/// sink the rest), a title from the site if it has none, the key checked.
#[allow(clippy::too_many_arguments)]
fn login_row(
    parsed: &mut Parsed,
    title: &str,
    urls: &[String],
    username: &str,
    password: &str,
    totp: &str,
    notes: &str,
    vault: Option<Source>,
    mut warnings: Vec<String>,
) {
    if urls.is_empty() {
        skip(
            parsed,
            title,
            "has no website address; add it by hand as a new item if you still use it",
            vault,
        );
        return;
    }
    let read: Vec<(String, String)> = urls.iter().filter_map(|u| read_site(u)).collect();
    if read.is_empty() {
        skip(
            parsed,
            title,
            "its website address could not be read",
            vault,
        );
        return;
    }
    if read.len() < urls.len() {
        warnings.push("one of its website addresses could not be read and was left off".into());
    }
    let host = url::Url::parse(&read[0].0)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let title = if title.is_empty() {
        host
    } else {
        title.to_string()
    };
    if password.is_empty() {
        warnings.push("the export holds no password for it".into());
    }
    let totp = totp.trim();
    let totp = if totp.is_empty() {
        ""
    } else if Params::parse(totp).is_ok() {
        totp
    } else {
        warnings.push("its one-time password key could not be read and was not imported".into());
        ""
    };
    let mut item = blank(LOGIN, &title);
    let mut domains: Vec<String> = Vec::new();
    for (_, domain) in &read {
        if !domains.contains(domain) {
            domains.push(domain.clone());
        }
    }
    item.sites = domains;
    item.raw.notes = Some(notes.to_string()).filter(|n| !n.is_empty());
    item.raw.login = Some(Login {
        username: Some(username.to_string()).filter(|u| !u.is_empty()),
        password: Some(password.to_string()).filter(|p| !p.is_empty()),
        totp: Some(totp.to_string()).filter(|t| !t.is_empty()),
        uris: read
            .iter()
            .map(|(u, _)| Uri {
                uri: Some(u.clone()),
            })
            .collect(),
    });
    parsed.items.push(Imported {
        url: read[0].0.clone(),
        item,
        warnings,
        vault,
        duplicate: false,
        update: None,
    });
}

/// A card, identity or note that came along.
fn push_other(parsed: &mut Parsed, item: Item, vault: Option<Source>) {
    parsed.items.push(Imported {
        item,
        url: String::new(),
        warnings: Vec::new(),
        vault,
        duplicate: false,
        update: None,
    });
}

/// The JSON behind 1Password's "Copy item JSON": logins only.
fn parse_onepassword_item(item: &Value) -> Result<Parsed, Failure> {
    let (Some(overview), Some(details)) = (item.get("overview"), item.get("details")) else {
        return Err(usage(
            "that JSON is not a 1Password item; expected the shape \"Copy item JSON\" produces",
        ));
    };
    if let Some(template) = item["templateUuid"].as_str()
        && template != "001"
    {
        return Err(usage(
            "only logins can be imported, and this 1Password item is a different type",
        ));
    }
    let title = overview["title"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() {
        return Err(usage("this 1Password item has no title"));
    }
    let fields = details["fields"].as_array().cloned().unwrap_or_default();
    let named = |want: &str| {
        fields
            .iter()
            .find(|f| f["designation"] == want)
            .or_else(|| fields.iter().find(|f| f["name"] == want))
            .and_then(|f| f["value"].as_str().map(str::to_string))
    };
    let (password, username) = (named("password"), named("username"));
    if password.is_none() && username.is_none() {
        return Err(usage(
            "this 1Password item has no username or password fields, so it does not look like a login",
        ));
    }
    // The key lives in a section field: named TOTP_<id>, or labelled
    // "one-time password" (labels are translated, the name is surer).
    let mut totp = String::new();
    for section in details["sections"].as_array().into_iter().flatten() {
        for field in section["fields"].as_array().into_iter().flatten() {
            let named_totp = field["n"]
                .as_str()
                .is_some_and(|n| n.to_uppercase().starts_with("TOTP_"));
            let labelled = field["t"].as_str().is_some_and(one_time_password);
            if (named_totp || labelled)
                && let Some(v) = field["v"].as_str().filter(|v| !v.trim().is_empty())
            {
                totp = v.trim().to_string();
            }
        }
    }
    let mut urls: Vec<String> = overview["URLs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| u["u"].as_str())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    if urls.is_empty()
        && let Some(url) = overview["url"].as_str().filter(|u| !u.trim().is_empty())
    {
        urls.push(url.trim().to_string());
    }
    let mut parsed = Parsed {
        source: "1Password item".into(),
        items: Vec::new(),
        skipped: Vec::new(),
    };
    if urls.is_empty() {
        skip(
            &mut parsed,
            &title,
            "has no website address; add it by hand as a new item instead",
            None,
        );
        return Ok(parsed);
    }
    let notes = details["notesPlain"].as_str().unwrap_or("");
    login_row(
        &mut parsed,
        &title,
        &urls,
        &username.unwrap_or_default(),
        &password.unwrap_or_default(),
        &totp,
        notes,
        None,
        Vec::new(),
    );
    Ok(parsed)
}

fn one_time_password(label: &str) -> bool {
    let label = label.to_lowercase();
    ["one-time password", "one time password", "onetime password"]
        .iter()
        .any(|l| label.contains(l))
}

/// What a 1Password category is called when it is set aside.
fn category_name(category: &str) -> &'static str {
    match category {
        "002" => "a credit card",
        "003" => "a secure note",
        "004" => "an identity",
        "005" => "a saved password",
        "006" => "a document",
        "100" => "a software license",
        "101" => "a bank account",
        "110" => "a server",
        "111" => "an email account",
        "112" => "an API credential",
        "114" => "an SSH key",
        "115" => "a crypto wallet",
        _ => "another kind of item",
    }
}

/// 1PUX: a zip holding `export.data`, items nested in vaults in accounts.
/// `export.data` is bounded before it is read: its declared and its
/// compressed size must both be under 64 MiB (a declared size of zero is
/// no bound at all), and the read stops at the bound whatever the header
/// says, so neither a lying header nor a zip bomb runs away with memory.
pub fn parse_1pux(bytes: &[u8]) -> Result<Parsed, Failure> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| usage(NOT_1PUX))?;
    let mut entry = zip.by_name("export.data").map_err(|_| usage(NOT_1PUX))?;
    if entry.size() == 0 {
        return Err(usage(NOT_1PUX));
    }
    if entry.size() > MAX_INFLATED_BYTES || entry.compressed_size() > MAX_INFLATED_BYTES {
        return Err(usage("that export is too large to read"));
    }
    let declared = entry.size();
    let mut data = Vec::new();
    (&mut entry)
        .take(declared + 1)
        .read_to_end(&mut data)
        .map_err(|_| usage(NOT_1PUX))?;
    if data.len() as u64 != declared {
        return Err(usage(NOT_1PUX));
    }
    let root: Value = serde_json::from_slice(&data).map_err(|_| usage(NOT_1PUX))?;
    let Some(accounts) = root["accounts"].as_array() else {
        return Err(usage(NOT_1PUX));
    };
    let mut parsed = Parsed {
        source: "1Password".into(),
        items: Vec::new(),
        skipped: Vec::new(),
    };
    for account in accounts {
        for vault in account["vaults"].as_array().into_iter().flatten() {
            let name = vault["attrs"]["name"].as_str().unwrap_or("").trim();
            let name = if name.is_empty() { "Vault" } else { name }.to_string();
            let id = vault["attrs"]["uuid"].as_str().unwrap_or("").trim();
            let from = Source {
                id: if id.is_empty() {
                    name.clone()
                } else {
                    id.to_string()
                },
                name,
            };
            for item in vault["items"].as_array().into_iter().flatten() {
                onepux_item(&mut parsed, item, &from);
            }
        }
    }
    Ok(parsed)
}

fn onepux_item(parsed: &mut Parsed, item: &Value, from: &Source) {
    let raw_title = item["overview"]["title"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let vault = Some(from.clone());
    let category = item["categoryUuid"].as_str().unwrap_or("");
    if category != "001" {
        let reason = format!("{}, not a login", category_name(category));
        skip(parsed, &raw_title, &reason, vault);
        return;
    }
    if item["state"] == "archived" {
        skip(parsed, &raw_title, "archived in 1Password", vault);
        return;
    }
    let overview = &item["overview"];
    let mut urls: Vec<String> = overview["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| u["url"].as_str())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    if urls.is_empty()
        && let Some(url) = overview["url"].as_str().filter(|u| !u.trim().is_empty())
    {
        urls.push(url.trim().to_string());
    }
    let details = &item["details"];
    let designated = |want: &str| {
        details["loginFields"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["designation"] == want)
            .and_then(|f| f["value"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut totp = String::new();
    for section in details["sections"].as_array().into_iter().flatten() {
        for field in section["fields"].as_array().into_iter().flatten() {
            let looks = field["id"]
                .as_str()
                .is_some_and(|i| i.to_uppercase().starts_with("TOTP_"))
                || field["title"].as_str().is_some_and(one_time_password);
            let value = &field["value"];
            let candidate = value["totp"]
                .as_str()
                .filter(|t| !t.is_empty())
                .or(if looks {
                    value["concealed"].as_str()
                } else {
                    None
                })
                .unwrap_or("");
            if !candidate.trim().is_empty() {
                totp = candidate.trim().to_string();
            }
        }
    }
    login_row(
        parsed,
        &raw_title,
        &urls,
        designated("username").trim(),
        &designated("password"),
        &totp,
        details["notesPlain"].as_str().unwrap_or(""),
        vault,
        Vec::new(),
    );
}

/// Bitwarden's unencrypted JSON export: its items are already this
/// vault's shape. Logins need a site; cards, identities and notes come as
/// they are; anything else is set aside.
fn parse_bitwarden_json(root: &Value) -> Result<Parsed, Failure> {
    if root["encrypted"] == true {
        return Err(usage(
            "that Bitwarden export is encrypted; export it again as unencrypted JSON",
        ));
    }
    let Some(items) = root["items"].as_array() else {
        return Err(usage("that JSON is not a Bitwarden export"));
    };
    let mut parsed = Parsed {
        source: "Bitwarden".into(),
        items: Vec::new(),
        skipped: Vec::new(),
    };
    for value in items {
        let title = value["name"].as_str().unwrap_or("").trim().to_string();
        let Ok(raw) = serde_json::from_value::<Raw>(value.clone()) else {
            skip(&mut parsed, &title, "its fields could not be read", None);
            continue;
        };
        match raw.kind {
            LOGIN => {
                let login = raw.login.clone().unwrap_or_default();
                let urls: Vec<String> = login.uris.iter().filter_map(|u| u.uri.clone()).collect();
                let before = parsed.items.len();
                login_row(
                    &mut parsed,
                    &title,
                    &urls,
                    login.username.as_deref().unwrap_or(""),
                    login.password.as_deref().unwrap_or(""),
                    login.totp.as_deref().unwrap_or(""),
                    raw.notes.as_deref().unwrap_or(""),
                    None,
                    Vec::new(),
                );
                // Custom fields come along with the login.
                if let Some(imported) = parsed.items.get_mut(before) {
                    imported.item.raw.fields = raw.fields.clone();
                }
            }
            NOTE | CARD | IDENTITY => {
                let mut item = blank(raw.kind, &title);
                item.raw = Raw {
                    login: None,
                    ssh_key: None,
                    ..raw
                };
                if item.raw.name.trim().is_empty() {
                    item.raw.name = "(untitled)".into();
                }
                push_other(&mut parsed, item, None);
            }
            _ => skip(
                &mut parsed,
                &title,
                "an SSH key or another kind latch does not keep",
                None,
            ),
        }
    }
    Ok(parsed)
}

/// The rows' logins against what the vault holds: the same title, username
/// and first site name the same item, and the secrets decide. Identical
/// secrets mark a duplicate; a changed password or key marks an update of
/// that item (pinned to its revision). One identity can mean several items
/// and several rows, so each is settled as a batch: exact matches pair off
/// first, an update happens only when one row and one item are left facing
/// each other, rows with no items left are new, and anything else is left
/// alone, saying so, rather than guessing which password to write over.
pub fn mark_against(vault: &dyn Backend, parsed: &mut Parsed) -> Result<(), Failure> {
    let key =
        |title: &str, username: &str, site: &str| format!("{title}\u{0}{username}\u{0}{site}");
    let held = vault.list();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (n, row) in parsed.items.iter().enumerate() {
        if row.item.raw.kind != LOGIN {
            continue;
        }
        let username = row
            .item
            .raw
            .login
            .as_ref()
            .and_then(|l| l.username.clone())
            .unwrap_or_default();
        let k = key(
            &row.item.raw.name,
            &username,
            row.item.sites.first().map_or("", |s| s),
        );
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, rows)) => rows.push(n),
            None => groups.push((k, vec![n])),
        }
    }
    for (k, rows) in groups {
        let items: Vec<_> = held
            .iter()
            .filter(|m| m.kind == "login")
            .filter(|m| {
                key(
                    &m.title,
                    m.username.as_deref().unwrap_or(""),
                    m.sites.first().map_or("", |s| s),
                ) == k
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        let mut diffs: Vec<Vec<(bool, bool)>> = Vec::new();
        for &r in &rows {
            let login = parsed.items[r].item.raw.login.clone().unwrap_or_default();
            let mut per = Vec::new();
            for item in &items {
                per.push(vault.differs(
                    &item.item,
                    login.password.as_deref().unwrap_or(""),
                    login.totp.as_deref().unwrap_or(""),
                )?);
            }
            diffs.push(per);
        }
        let mut free = vec![true; items.len()];
        let mut unmatched = Vec::new();
        for (i, &r) in rows.iter().enumerate() {
            let exact = (0..items.len()).find(|&j| free[j] && diffs[i][j] == (false, false));
            match exact {
                Some(j) => {
                    parsed.items[r].duplicate = true;
                    free[j] = false;
                }
                None => unmatched.push(i),
            }
        }
        let left: Vec<usize> = (0..items.len()).filter(|&j| free[j]).collect();
        if unmatched.is_empty() || left.is_empty() {
            continue;
        }
        if let ([i], [j]) = (unmatched.as_slice(), left.as_slice()) {
            let (password, totp) = diffs[*i][*j];
            parsed.items[rows[*i]].update = Some(Update {
                item: items[*j].item.clone(),
                revision: items[*j].revision,
                password,
                totp,
            });
            continue;
        }
        for i in unmatched {
            let row = &mut parsed.items[rows[i]];
            row.duplicate = true;
            row.warnings.push(
                "several entries share this name, username and site; left alone rather than guess which updates which".into(),
            );
        }
    }
    Ok(())
}

/// What the owner is shown before picking: never a value.
pub fn preview(parsed: &Parsed) -> Value {
    let mut vaults: Vec<(Source, usize)> = Vec::new();
    let mut note = |vault: &Option<Source>, counts: bool| {
        if let Some(vault) = vault {
            match vaults.iter_mut().find(|(v, _)| v.id == vault.id) {
                Some((_, n)) => *n += usize::from(counts),
                None => vaults.push((vault.clone(), usize::from(counts))),
            }
        }
    };
    for item in &parsed.items {
        note(&item.vault, true);
    }
    for skipped in &parsed.skipped {
        note(&skipped.vault, false);
    }
    let items: Vec<Value> = parsed
        .items
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let login = row.item.raw.login.clone().unwrap_or_default();
            let mut changed = Vec::new();
            if let Some(update) = &row.update {
                if update.password {
                    changed.push("password");
                }
                if update.totp {
                    changed.push("totp");
                }
            }
            let mut entry = json!({
                "pick": n + 1,
                "kind": super::item::kind_name(row.item.raw.kind),
                "title": row.item.raw.name,
                "username": login.username.unwrap_or_default(),
                "url": row.url,
                "has_password": login.password.is_some(),
                "has_totp": login.totp.is_some(),
                "warnings": row.warnings,
                "duplicate": row.duplicate,
                "changed": changed,
            });
            if let Some(vault) = &row.vault {
                entry["vault"] = json!(vault);
            }
            entry
        })
        .collect();
    json!({
        "source": parsed.source,
        "items": items,
        "skipped": parsed.skipped,
        "vaults": vaults.iter().map(|(v, n)| json!({"id": v.id, "name": v.name, "items": n})).collect::<Vec<_>>(),
    })
}

#[derive(Serialize, Debug, PartialEq, Default)]
pub struct Outcome {
    pub saved: usize,
    pub updated: usize,
    pub duplicates: usize,
    pub failed: Vec<Skipped>,
}

/// Saves the picked rows (1-based, as the preview numbers them). A
/// duplicate is left alone; an update edits only the secrets that changed,
/// at the revision read when the rows were marked. One bad row does not
/// sink the rest.
pub fn commit(vault: &mut dyn Backend, parsed: &Parsed, picks: &[usize]) -> Outcome {
    let mut outcome = Outcome::default();
    for &pick in picks {
        let Some(row) = pick.checked_sub(1).and_then(|n| parsed.items.get(n)) else {
            outcome.failed.push(Skipped {
                title: format!("#{pick}"),
                reason: "there is no such row".into(),
                vault: None,
            });
            continue;
        };
        if row.duplicate {
            outcome.duplicates += 1;
            continue;
        }
        let fail = |outcome: &mut Outcome, failure: Failure| {
            outcome.failed.push(Skipped {
                title: row.item.raw.name.clone(),
                reason: failure.message,
                vault: row.vault.clone(),
            });
        };
        if let Some(update) = &row.update {
            let Some(current) = vault.item(&update.item) else {
                fail(
                    &mut outcome,
                    Failure::failed("gone", "the item is gone from the vault".into()),
                );
                continue;
            };
            let mut edited = current;
            let incoming = row.item.raw.login.clone().unwrap_or_default();
            let login = edited.raw.login.get_or_insert_with(Login::default);
            if update.password {
                login.password = incoming.password;
            }
            if update.totp {
                login.totp = incoming.totp;
            }
            match vault.update(edited, update.revision) {
                Ok(_) => outcome.updated += 1,
                Err(failure) if failure.code == "stale_revision" => fail(
                    &mut outcome,
                    Failure::failed(
                        "stale_revision",
                        "the item changed somewhere else since this import was read".into(),
                    ),
                ),
                Err(failure) => fail(&mut outcome, failure),
            }
            continue;
        }
        match vault.add(row.item.clone()) {
            Ok(_) => outcome.saved += 1,
            Err(failure) => fail(&mut outcome, failure),
        }
    }
    outcome
}

#[cfg(test)]
mod tests;
