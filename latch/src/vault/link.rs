//! Phone links: the owner adds an item from a phone, away from their
//! machines, and the value goes straight to the host.
//!
//! An owner device asks the host for a link (`latch vault link`). The host
//! keeps the request in memory: the item's kind, title, sites and the
//! non-secret fields the owner gave, which fields the phone is asked for,
//! and the sha256 of a fresh one-time code. The link names the host's key,
//! its relay and the code, all after `#`, so the server that serves the
//! phone page (see `phone/`) never sees them.
//!
//! The phone page dials the host over iroh on its own protocol name
//! (`ALPN`), through the relay, with a throwaway key. The code, not the
//! phone's key, is what lets it in: Plow is never asked who the phone is.
//! On that protocol a code can only fetch its own form (`page`) and save
//! its own item once (`submit`), within 15 minutes. A link only ever adds
//! a new item; it never edits or removes one.
//!
//! An agent may open a link for a login too, judged by the gatekeeper like
//! its other work, and send it to the owner. It learns only the link's
//! state (`state`): pending, saved with the new item's id, or expired.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iroh::RelayUrl;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::host::{Vault, add, item_from};
use super::item::{entry_labels, kind_code};
use crate::failure::{Code, Failure, code};
use crate::state::to_hex;

/// The protocol name a phone page announces. A host accepts it beside
/// `wire::ALPN`, and serves only `page` and `submit` on it.
pub const ALPN: &[u8] = b"plow/latch-phone/1";

/// How long a link stays open.
const OPEN_FOR: Duration = Duration::from_secs(15 * 60);

/// The form the phone page shows, served by the host itself.
const PAGE: &str = include_str!("phone.html");

/// One open link, by the sha256 of its code.
pub struct Request {
    id: String,
    kind: String,
    title: String,
    sites: Vec<String>,
    fields: Vec<Field>,
    made: Instant,
    used: bool,
    /// The item the link saved, once it has.
    saved: Option<String>,
}

/// A field the phone shows: a non-secret one the owner gave comes filled
/// in; secret ones are always typed on the phone.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Field {
    label: String,
    secret: bool,
    /// Must be filled before the item is saved (see `required`).
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

/// Where the phone dials: the host's key, name and relay.
pub struct Here {
    pub key: String,
    pub name: String,
    pub relay: Option<RelayUrl>,
}

/// `vault link`: opens a request and returns what the link carries.
pub fn open(
    vault: &Vault,
    here: &Here,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let relay = here.relay.as_ref().ok_or_else(|| {
        Failure::new(
            code::relay_unreachable,
            "the host has no relay yet, and a phone can only reach it through one".into(),
        )
    })?;
    // The item the link would save, checked now (title, sites) so a bad
    // one is refused here rather than on the phone.
    let mut item = item_from(args)?;
    item.validate()?;
    let kind = args["kind"].as_str().unwrap_or_default().to_string();
    let fields = fields(&kind, &args["values"], &args["ask"])?;
    let code: [u8; 16] = rand::random();
    let code = to_hex(&code);
    let id = format!("l{:08x}", rand::random::<u32>());
    let request = Request {
        id: id.clone(),
        kind,
        title: item.raw.name.clone(),
        sites: item.sites.clone(),
        fields,
        made: Instant::now(),
        used: false,
        saved: None,
    };
    vault
        .links
        .lock()
        .expect("links lock")
        .insert(hash(&code), request);
    audit.insert("link".into(), json!(id));
    audit.insert("kind".into(), json!(args["kind"]));
    audit.insert("title".into(), json!(item.raw.name));
    audit.insert("sites".into(), json!(item.sites));
    if let Some(username) = args["values"]["username"].as_str() {
        audit.insert("username".into(), json!(username));
    }
    Ok(json!({
        "link": id,
        "host": here.key,
        "relay": page_relay(relay),
        "code": code,
        "expires_in": OPEN_FOR.as_secs(),
    }))
}

/// The fields the phone shows: the owner's non-secret ones, filled in;
/// what each kind always asks for (a login's username and password, a
/// card's number and code, a note's text); and any more the owner names
/// with `--ask` (an identity's ssn, say), typed on the phone.
fn fields(kind: &str, given: &Value, ask: &Value) -> Result<Vec<Field>, Failure> {
    let labels = kind_code(kind).map(entry_labels).unwrap_or_default();
    // A secret is typed on the phone only, never given with the request.
    let given_secret = given
        .as_object()
        .into_iter()
        .flatten()
        .find(|(label, _)| labels.iter().any(|(l, secret)| l == label && *secret));
    if let Some((label, _)) = given_secret {
        return Err(Failure::new(
            code::secret_in_argv,
            format!("{label} is secret: it is typed on the phone, never sent with the link"),
        ));
    }
    let mut fields: Vec<Field> = given
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(label, value)| {
            Some(Field {
                label: label.clone(),
                secret: false,
                required: required(kind, label, false),
                value: Some(value.as_str()?.to_string()),
            })
        })
        .collect();
    let always: &[&str] = match kind {
        "login" => &["username", "password"],
        "card" => &["number", "code"],
        "note" => &["notes"],
        _ => &[],
    };
    let asked = ask
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for label in always.iter().copied().chain(asked) {
        let Some((_, secret)) = labels.iter().find(|(l, _)| *l == label) else {
            return Err(Failure::new(
                code::bad_item,
                format!("a {kind} has no field {label:?}"),
            ));
        };
        if fields.iter().any(|f| f.label == label) {
            continue;
        }
        fields.push(Field {
            label: label.to_string(),
            secret: *secret,
            required: required(kind, label, *secret),
            value: None,
        });
    }
    Ok(fields)
}

/// Whether a field must be filled before the item is saved, whether the
/// owner gave it or the phone types it: every secret, and a note's text,
/// which is all a note is.
fn required(kind: &str, label: &str, secret: bool) -> bool {
    secret || (kind == "note" && label == "notes")
}

/// The relay as a browser must dial it. iroh names relays as fully
/// qualified (`…iroh.link.`), and WebKit refuses the relay's certificate
/// for that dotted name.
fn page_relay(relay: &RelayUrl) -> String {
    let mut url: url::Url = (**relay).clone();
    if let Some(host) = url.host_str().and_then(|h| h.strip_suffix('.')) {
        let host = host.to_string();
        url.set_host(Some(&host)).expect("a relay's own host");
    }
    url.to_string()
}

fn hash(code: &str) -> String {
    to_hex(&Sha256::digest(code.as_bytes()))
}

fn refused(code: Code, message: &str) -> Failure {
    Failure::new(code, message.into())
}

/// The open request a code names, or why it is not usable.
fn usable<'a>(
    links: &'a mut HashMap<String, Request>,
    code: &str,
) -> Result<&'a mut Request, Failure> {
    let request = links
        .get_mut(&hash(code))
        .ok_or_else(|| refused(code::no_such_link, "this link is not one this machine made"))?;
    if request.used {
        return Err(refused(code::link_used, "this link has already been used"));
    }
    if request.made.elapsed() >= OPEN_FOR {
        return Err(refused(code::link_expired, "this link has expired"));
    }
    Ok(request)
}

/// `page`: the form, and what it is for.
pub fn page(
    vault: &Vault,
    here: &Here,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let code = args["code"].as_str().unwrap_or_default();
    let mut links = vault.links.lock().expect("links lock");
    let request = usable(&mut links, code)?;
    audit.insert("link".into(), json!(request.id));
    let left = OPEN_FOR.saturating_sub(request.made.elapsed());
    Ok(json!({
        "html": PAGE,
        "request": {
            "host": here.name,
            "art": crate::keyart::art(&here.key, &here.name),
            "kind": request.kind,
            "title": request.title,
            "sites": request.sites,
            "fields": request.fields,
            "expires_in": left.as_secs(),
        },
    }))
}

/// `submit`: saves the item once, with every required field (secrets, a
/// note's text) filled in. The link stays locked while it saves, so two taps cannot
/// save twice; a save that fails leaves it open.
pub fn submit(
    vault: &Vault,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let code = args["code"].as_str().unwrap_or_default();
    let mut links = vault.links.lock().expect("links lock");
    let request = usable(&mut links, code)?;
    audit.insert("link".into(), json!(request.id));
    let mut values = Map::new();
    for field in &request.fields {
        if let Some(value) = &field.value {
            values.insert(field.label.clone(), json!(value));
        }
    }
    for (label, value) in args["values"].as_object().into_iter().flatten() {
        if !request.fields.iter().any(|f| &f.label == label) {
            return Err(Failure::new(
                code::bad_item,
                format!("this link does not ask for {label:?}"),
            ));
        }
        values.insert(label.clone(), value.clone());
    }
    let empty = |field: &&Field| {
        let value = values.get(&field.label).and_then(Value::as_str);
        value.unwrap_or_default().is_empty()
    };
    if let Some(field) = request.fields.iter().filter(|f| f.required).find(empty) {
        return Err(Failure::new(
            code::missing_field,
            format!("fill in the {}", field.label),
        ));
    }
    let item = json!({
        "kind": request.kind,
        "title": request.title,
        "sites": request.sites,
        "values": values,
    });
    let saved = add(vault, &item, audit)?;
    request.used = true;
    request.saved = saved["item"]["item"].as_str().map(str::to_string);
    Ok(saved)
}

/// `link_state`: how the link `args["link"]` names is doing, by its id, never
/// its code. Saved, it names the new item; never a value.
pub fn state(
    vault: &Vault,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let id = args["link"].as_str().unwrap_or_default();
    audit.insert("link".into(), json!(id));
    let links = vault.links.lock().expect("links lock");
    let request = links.values().find(|r| r.id == id).ok_or_else(|| {
        refused(
            code::no_such_link,
            "this host holds no such link; a restart drops open links, so ask for a new one",
        )
    })?;
    Ok(match (&request.saved, request.made.elapsed() >= OPEN_FOR) {
        (Some(item), _) => json!({"link": id, "state": "saved", "item": item}),
        (None, true) => json!({"link": id, "state": "expired"}),
        (None, false) => json!({
            "link": id,
            "state": "pending",
            "expires_in": OPEN_FOR.saturating_sub(request.made.elapsed()).as_secs(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    use crate::vault::store::Backend;

    fn here() -> Here {
        Here {
            key: "ab".repeat(32),
            name: "kitchen".into(),
            relay: Some("https://usw1-1.relay.n0.iroh.link./".parse().unwrap()),
        }
    }

    fn opened(vault: &Vault) -> String {
        let args = json!({
            "kind": "login", "title": "Shop", "sites": ["shop.example"],
            "values": {"username": "ada@shop.example"},
        });
        let link = open(vault, &here(), &args, &mut Map::new()).unwrap();
        link["code"].as_str().unwrap().to_string()
    }

    fn submitted(vault: &Vault, code: &str, values: Value) -> Result<Value, Failure> {
        submit(
            vault,
            &json!({"code": code, "values": values}),
            &mut Map::new(),
        )
    }

    #[test]
    fn the_link_names_the_relay_as_a_browser_can_dial_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({"kind": "login", "title": "Shop", "sites": ["shop.example"]});
        let link = open(&vault, &here(), &args, &mut Map::new()).unwrap();
        assert_eq!(link["relay"], "https://usw1-1.relay.n0.iroh.link/");
        assert_eq!(link["code"].as_str().unwrap().len(), 32);
    }

    #[test]
    fn a_link_saves_its_item_once_with_the_phones_values() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let code = opened(&vault);

        let shown = page(&vault, &here(), &json!({"code": code}), &mut Map::new()).unwrap();
        let fields = &shown["request"]["fields"];
        assert_eq!(fields[0]["value"], "ada@shop.example");
        assert_eq!(
            fields[1],
            json!({"label": "password", "secret": true, "required": true})
        );
        assert!(!shown.to_string().contains(&code));

        let saved = submitted(&vault, &code, json!({"password": "made-up-1"})).unwrap();
        assert_eq!(saved["item"]["username"], "ada@shop.example");
        let refused = submitted(&vault, &code, json!({"password": "made-up-2"})).unwrap_err();
        assert_eq!(
            (refused.exit, refused.code.as_str()),
            (Exit::Denied, "link_used")
        );
        let page_again = page(&vault, &here(), &json!({"code": code}), &mut Map::new());
        assert_eq!(page_again.unwrap_err().code, "link_used");

        let items = vault.with(|store| Ok(store.list())).unwrap();
        assert_eq!(items.len(), 1);
        let password = vault
            .with(|store| store.release(&items[0].item, "password"))
            .unwrap();
        assert_eq!(password, "made-up-1");
    }

    #[test]
    fn a_link_takes_only_the_fields_it_asks_for_and_only_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let code = opened(&vault);

        let refused = submitted(&vault, &"0".repeat(32), json!({"password": "x"})).unwrap_err();
        assert_eq!(refused.code, "no_such_link");
        let refused = submitted(&vault, &code, json!({"totp": "JBSWY3DPEHPK3PXP"})).unwrap_err();
        assert_eq!(refused.code, "bad_item");
        // A refused submit leaves the link open.
        assert!(submitted(&vault, &code, json!({"password": "made-up"})).is_ok());
    }

    #[test]
    fn a_link_asks_the_phone_for_an_identitys_secrets_and_a_notes_text() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({
            "kind": "identity", "title": "Me",
            "values": {"first name": "Ada"}, "ask": ["ssn", "passport number"],
        });
        let link = open(&vault, &here(), &args, &mut Map::new()).unwrap();
        let code = link["code"].as_str().unwrap();
        let shown = page(&vault, &here(), &json!({"code": code}), &mut Map::new()).unwrap();
        assert_eq!(
            shown["request"]["fields"],
            json!([
                {"label": "first name", "secret": false, "required": false, "value": "Ada"},
                {"label": "ssn", "secret": true, "required": true},
                {"label": "passport number", "secret": true, "required": true},
            ])
        );
        let values = json!({"ssn": "078-05-1120", "passport number": "X1234567"});
        submitted(&vault, code, values).unwrap();

        let args = json!({"kind": "note", "title": "Wifi"});
        let link = open(&vault, &here(), &args, &mut Map::new()).unwrap();
        let code = link["code"].as_str().unwrap();
        submitted(&vault, code, json!({"notes": "made-up text"})).unwrap();

        let items = vault.with(|store| Ok(store.list())).unwrap();
        let release = |item: usize, label: &str| {
            vault
                .with(|store| store.release(&items[item].item, label))
                .unwrap()
        };
        assert_eq!(release(0, "ssn"), "078-05-1120");
        assert_eq!(release(0, "passport number"), "X1234567");
        assert_eq!(release(1, "notes"), "made-up text");
    }

    #[test]
    fn a_link_refuses_to_ask_for_a_field_its_kind_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({"kind": "identity", "title": "Me", "ask": ["cvv"]});
        let refused = open(&vault, &here(), &args, &mut Map::new()).unwrap_err();
        assert_eq!(refused.code, "bad_item");
        assert!(vault.links.lock().unwrap().is_empty());
    }

    #[test]
    fn a_link_saves_nothing_until_its_secrets_or_a_notes_text_are_filled() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let code = opened(&vault);
        for values in [json!({}), json!({"password": ""})] {
            let refused = submitted(&vault, &code, values).unwrap_err();
            assert_eq!(
                (refused.exit, refused.code.as_str()),
                (Exit::Usage, "missing_field")
            );
        }
        let note = open(
            &vault,
            &here(),
            &json!({"kind": "note", "title": "Wifi"}),
            &mut Map::new(),
        )
        .unwrap();
        let note = note["code"].as_str().unwrap();
        let refused = submitted(&vault, note, json!({"notes": ""})).unwrap_err();
        assert_eq!(refused.code, "missing_field");
        // A note's text the owner gave empty is still required.
        let given = open(
            &vault,
            &here(),
            &json!({"kind": "note", "title": "Wifi", "values": {"notes": ""}}),
            &mut Map::new(),
        )
        .unwrap();
        let given = given["code"].as_str().unwrap();
        let shown = page(&vault, &here(), &json!({"code": given}), &mut Map::new()).unwrap();
        assert_eq!(shown["request"]["fields"][0]["required"], true);
        for values in [json!({}), json!({"notes": ""})] {
            let refused = submitted(&vault, given, values).unwrap_err();
            assert_eq!(refused.code, "missing_field");
        }
        assert!(vault.list_if_present().is_empty());
        // The links are still open for the filled-in forms.
        assert!(submitted(&vault, &code, json!({"password": "made-up"})).is_ok());
        assert!(submitted(&vault, note, json!({"notes": "made-up text"})).is_ok());
    }

    fn link_state(vault: &Vault, link: &Value) -> Value {
        state(vault, &json!({"link": link}), &mut Map::new()).unwrap()
    }

    #[test]
    fn a_links_state_goes_from_pending_to_saved_naming_only_the_new_item() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({
            "kind": "login", "title": "Shop", "sites": ["shop.example"],
            "values": {"username": "ada@shop.example"},
        });
        let mut audit = Map::new();
        let opened = open(&vault, &here(), &args, &mut audit).unwrap();
        // The audit keeps what the rules can name: the site, title and username.
        assert_eq!(audit["title"], "Shop");
        assert_eq!(audit["sites"], json!(["shop.example"]));
        assert_eq!(audit["username"], "ada@shop.example");
        let id = &opened["link"];
        let pending = link_state(&vault, id);
        assert_eq!(pending["state"], "pending");
        assert!(pending["expires_in"].as_u64().unwrap() <= OPEN_FOR.as_secs());

        let code = opened["code"].as_str().unwrap();
        submitted(&vault, code, json!({"password": "made-up-pass-5d0e"})).unwrap();
        let saved = link_state(&vault, id);
        assert_eq!(saved, json!({"link": id, "state": "saved", "item": "v1"}));
        // Used once, it stays saved, whatever else is tried with it.
        assert_eq!(
            submitted(&vault, code, json!({"password": "x"}))
                .unwrap_err()
                .code,
            "link_used"
        );
        assert_eq!(link_state(&vault, id), saved);

        let unknown = state(&vault, &json!({"link": "l00000000"}), &mut Map::new());
        assert_eq!(unknown.unwrap_err().code, "no_such_link");
    }

    #[test]
    fn a_link_never_takes_a_secret_from_whoever_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({
            "kind": "login", "title": "Shop", "sites": ["shop.example"],
            "values": {"password": "made-up-pass-90ab"},
        });
        let refused = open(&vault, &here(), &args, &mut Map::new()).unwrap_err();
        assert_eq!(
            (refused.exit, refused.code.as_str()),
            (Exit::Usage, "secret_in_argv")
        );
        assert!(!refused.message.contains("made-up-pass-90ab"));
        assert!(vault.links.lock().unwrap().is_empty());
    }

    #[test]
    fn an_old_link_is_expired() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let code = opened(&vault);
        for request in vault.links.lock().unwrap().values_mut() {
            request.made -= OPEN_FOR;
        }
        let refused = submitted(&vault, &code, json!({"password": "made-up"})).unwrap_err();
        assert_eq!(refused.code, "link_expired");
        assert!(vault.list_if_present().is_empty());
        let id = vault
            .links
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .id
            .clone();
        assert_eq!(
            link_state(&vault, &json!(id)),
            json!({"link": id, "state": "expired"})
        );
    }

    #[test]
    fn a_link_with_a_bad_item_is_refused_before_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().into());
        let args = json!({"kind": "login", "title": "Shop", "sites": []});
        let refused = open(&vault, &here(), &args, &mut Map::new()).unwrap_err();
        assert_eq!(refused.code, "bad_item");
        assert!(vault.links.lock().unwrap().is_empty());
    }
}
