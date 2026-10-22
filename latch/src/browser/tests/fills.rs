//! Vault fills on the pinned engine, through the host's own fill path, on
//! the test site's login, checkout, address and code pages.
//!
//! The pages are local (`http://127.0.0.1`), and the site rules refuse
//! local HTTP; the host's test-only origin map reads each local origin as
//! the HTTPS site it stands in for (`login.shop.example`, a subdomain of
//! the login's site, and a lookalike, `shop.example.evil.example`), so
//! the real rules decide. A sandboxed frame stays opaque.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{CALLER, HIDDEN, exec, latch, test_state, test_state_alone};
use crate::browser::host::{self, Browsers};
use crate::vault::host::Vault;
use crate::vault::item::{CARD, IDENTITY, Item, LOGIN, Raw};
use crate::vault::store::Backend;
use crate::vault::totp::Params;

const PASSWORD: &str = "made-up-pass-7c41";
const NUMBER: &str = "4242424242424242";
const CVV: &str = "319";
const KEY: &str = "JBSWY3DPEHPK3PXP";

conformance!(
    vault_fills_go_only_where_the_item_allows_and_never_show_a_value,
    a_fill_that_cannot_finish_shows_no_value_and_keeps_the_agent_out,
    observe_lists_items_by_the_frames_that_hold_fields,
    a_login_goes_on_to_its_password_stage_in_one_pause,
    observe_suggests_from_hints_and_the_checks_refuse_contradictions,
    a_card_confirmation_binds_its_mapping_and_frames,
    a_fill_in_one_session_pauses_every_session_on_its_browser,
    agent_code_is_noted_per_site_and_kept_with_the_profile,
    a_login_after_an_edit_fills_the_new_password,
    no_secret_goes_into_a_form_sent_by_get,
    requests_go_on_while_a_guard_outlives_its_call,
    a_filled_value_is_guarded_in_every_tab_and_from_screenshots,
);

/// `value` as an agent or the audit saw it, for the leak checks, less what
/// holds digits by chance and is no value: a screenshot's size and digest,
/// the id of each confirmation it carries, wherever it is quoted, and the
/// test site's ports.
fn seen(value: &Value) -> String {
    let mut value = value.clone();
    let mut ids = Vec::new();
    confirmation_ids(&value, &mut ids);
    without_incidentals(&mut value, &ids);
    value.to_string()
}

/// The ids of the confirmations `value` carries: a `confirmation`'s `id`,
/// and a `confirmation` or `confirm` given as an id.
fn confirmation_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let id = match (key.as_str(), item) {
                    ("confirmation", Value::Object(confirmation)) => confirmation["id"].as_str(),
                    ("confirmation" | "confirm", Value::String(id)) => Some(id.as_str()),
                    _ => None,
                };
                ids.extend(id.map(str::to_string));
                confirmation_ids(item, ids);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| confirmation_ids(item, ids)),
        _ => {}
    }
}

fn without_incidentals(value: &mut Value, ids: &[String]) {
    match value {
        Value::Object(map) => {
            if let Some(shot) = map.get_mut("screenshot").and_then(Value::as_object_mut) {
                shot.remove("size");
                shot.remove("sha256");
            }
            map.values_mut()
                .for_each(|item| without_incidentals(item, ids));
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| without_incidentals(item, ids)),
        Value::String(text) => {
            *text = ids
                .iter()
                .fold(without_port(text), |text, id| text.replace(id.as_str(), ""));
        }
        _ => {}
    }
}

/// `text` with the port after the test site's host left out, and a test
/// site's own port where the payment page's address names it
/// (`?frame=PORT`); any other `?frame=` value stays.
fn without_port(text: &str) -> String {
    let text = ["127.0.0.1:", "localhost:"]
        .iter()
        .fold(text.to_string(), |text, host| {
            let mut parts = text.split(host);
            let mut out = parts.next().unwrap_or_default().to_string();
            for part in parts {
                out.push_str(host);
                out.push_str(part.trim_start_matches(|ch: char| ch.is_ascii_digit()));
            }
            out
        });
    let ports = SITE_PORTS.lock().unwrap();
    let mut parts = text.split("?frame=");
    let mut out = parts.next().unwrap_or_default().to_string();
    for part in parts {
        out.push_str("?frame=");
        let digits = part.len()
            - part
                .trim_start_matches(|ch: char| ch.is_ascii_digit())
                .len();
        let ours = part[..digits]
            .parse()
            .is_ok_and(|port: u16| ports.contains(&port));
        out.push_str(if ours { &part[digits..] } else { part });
    }
    out
}

/// The ports of the test sites [`site`] started.
static SITE_PORTS: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

/// Each time a test page reached `/beacon`, the test site's port, and
/// whether a value came along in the address (`leak=`).
static BEACONS: std::sync::Mutex<Vec<(u16, bool)>> = std::sync::Mutex::new(Vec::new());

/// How many times a page of the test site on `port` reached `/beacon`,
/// and how many of those carried a value.
fn beacons(port: u16) -> (usize, usize) {
    let hits = BEACONS.lock().unwrap();
    let mine = hits.iter().filter(|(p, _)| *p == port);
    (mine.clone().count(), mine.filter(|(_, leak)| *leak).count())
}

/// A failure as shown, for the leak checks (`seen`): its message is left
/// without the ids of the confirmations its details carry.
fn seen_failure(failure: &crate::failure::Failure) -> String {
    let shown = json!({"message": failure.message, "details": failure.details});
    format!("{} {}", failure.code, seen(&shown))
}

#[test]
fn the_leak_check_leaves_out_incidental_digits_and_nothing_else() {
    let shown = seen(&json!({
        "origin": "http://127.0.0.1:50319",
        "confirmation": "c3190a1b2c3d4",
        "details": {"confirmation": {
            "id": "c3190a1b2c3d4",
            "screenshot": {"sha256": "ab3190", "shot": "final", "size": 23190},
        }},
        "lines": ["Total $100.00", "cvv319"],
        "url": "http://localhost:50319/a?c=319",
    }));
    // The size, digest, confirmation id and port are gone; the two values
    // in the page's text and its query are still there.
    assert_eq!(shown.matches(CVV).count(), 2, "{shown}");

    // An issued confirmation's id is left out wherever it is quoted, and a
    // test site's port (here 50319) wherever the page's address names it; a
    // value, and an id-like string no confirmation carries, are still there.
    SITE_PORTS.lock().unwrap().push(50319);
    let stale = crate::failure::Failure {
        details: json!({
            "steps": [{"code": "confirm_stale", "confirm": "c3190a1b2c3d4"}],
            "stop": {"message": "confirmation c3190a1b2c3d4 was already used"},
            "page": {"url": "http://127.0.0.1:50000/processor.html?frame=50319",
                     "lines": ["c3190ffffffff", "frame=319"],
                     "links": ["http://127.0.0.1:50000/processor.html?frame=319"]},
        }),
        ..crate::failure::Failure::usage(
            "confirm_stale",
            "confirmation c3190a1b2c3d4 was already used; nothing was sent".into(),
        )
    };
    let shown = seen_failure(&stale);
    assert_eq!(shown.matches(CVV).count(), 3, "{shown}");
    assert!(
        shown.contains("c3190ffffffff") && shown.contains("\"frame=319\""),
        "{shown}"
    );
    // A secret where the processor's port goes is no port of a test site.
    assert!(shown.contains("processor.html?frame=319\""), "{shown}");
    let needed = crate::failure::Failure {
        details: json!({"confirmation": {"id": "c3190a1b2c3d4", "lines": ["cvv 319"]}}),
        ..crate::failure::Failure::usage(
            "confirm_needed",
            "confirm it with `latch browser do --confirm c3190a1b2c3d4`".into(),
        )
    };
    assert_eq!(seen_failure(&needed).matches(CVV).count(), 1);
}

/// Form fields from a POST body.
fn form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| {
            let decode = |s: &str| {
                let bytes = s.replace('+', " ").into_bytes();
                let mut out = Vec::new();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'%' && i + 2 < bytes.len() {
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                        out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                        i += 3;
                    } else {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
                String::from_utf8_lossy(&out).into_owned()
            };
            (decode(k), decode(v))
        })
        .collect()
}

/// The test site, plus the answers to its forms: each says only whether
/// what arrived was right, never the value.
fn site() -> u16 {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-site");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    SITE_PORTS.lock().unwrap().push(port);
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(socket) = socket else { continue };
            let folder = folder.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap_or(0);
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap_or(());
                let fields = form(&String::from_utf8_lossy(&body));
                let get = |k: &str| {
                    fields
                        .iter()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("")
                };
                let path = line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let answer = |title: &str, text: String| {
                    format!("<!doctype html><title>{title}</title><p id=out>{text}</p>")
                        .into_bytes()
                };
                let page = match path.as_str() {
                    "/beacon" => {
                        BEACONS.lock().unwrap().push((port, line.contains("leak=")));
                        Some(Vec::new())
                    }
                    "/welcome" | "/slow-welcome" => {
                        if path == "/slow-welcome" {
                            std::thread::sleep(Duration::from_secs(3));
                        }
                        let ok = get("password") == PASSWORD;
                        Some(answer(
                            "latch welcome",
                            format!(
                                "signed in: password {}",
                                if ok { "accepted" } else { "rejected" }
                            ),
                        ))
                    }
                    "/pay" => {
                        let check = |ok: bool| if ok { "ok" } else { "wrong" };
                        Some(answer(
                            "latch paid",
                            format!(
                                "payment: number {}, expiry {} ({}), cvv {}, name {}",
                                check(get("cc-number") == NUMBER),
                                check(get("cc-exp") == "04 / 31"),
                                get("cc-exp"),
                                check(get("cc-csc") == CVV),
                                get("cc-name"),
                            ),
                        ))
                    }
                    "/verify" => {
                        let params = Params::parse(KEY).unwrap();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let ok = [now - 30, now, now + 30]
                            .iter()
                            .any(|t| params.code(*t) == get("otp"));
                        Some(answer(
                            "latch verified",
                            format!("code {}", if ok { "accepted" } else { "rejected" }),
                        ))
                    }
                    file => std::fs::read(folder.join(file.trim_start_matches('/'))).ok(),
                };
                let mut socket = socket;
                let (status, page) = match page {
                    Some(page) => ("200 OK", page),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    page.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(&page);
            });
        }
    });
    port
}

fn item(kind: u8, title: &str, sites: &[&str], values: &[(&str, &str)]) -> Item {
    let values: Vec<(String, String)> = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Item {
        id: String::new(),
        revision: 0,
        sites: sites.iter().map(|s| s.to_string()).collect(),
        other_top_sites: vec![],
        raw: Raw::from_labels(kind, title, &values).unwrap(),
    }
}

struct Harness {
    browsers: Arc<Browsers>,
    /// The session the harness calls in, and its id.
    entry: Arc<host::Entry>,
    id: String,
    /// Everything an agent or the audit was shown, for the final grep.
    shown: Vec<String>,
}

impl Harness {
    async fn start(browsers: Browsers) -> Harness {
        let browsers = Arc::new(browsers);
        let (id, entry) = browsers.open("default", CALLER, None).await.unwrap();
        Harness {
            browsers,
            entry,
            id,
            shown: Vec::new(),
        }
    }

    /// A call as the host makes it for an agent (pause and eval flags
    /// included). An `open` starts a new session, which takes the place of
    /// the one before.
    async fn call(&mut self, command: &str, args: Value) -> Result<Value, crate::failure::Failure> {
        let mut audit = Map::new();
        let name = if command == "open" {
            "default"
        } else {
            &self.id
        };
        let result = host::call(
            &self.browsers,
            CALLER.into(),
            command,
            name,
            &args,
            None,
            &mut audit,
        )
        .await;
        if command == "open"
            && let Ok((opened, _)) = &result
        {
            let id = opened["session"].as_str().unwrap().to_string();
            self.entry = self.browsers.session(&id, CALLER).await.unwrap();
            let before = std::mem::replace(&mut self.id, id);
            self.browsers.close(&before, CALLER).await.unwrap();
        }
        self.shown.push(seen(&Value::Object(audit)));
        match result {
            Ok((value, _)) => {
                self.shown.push(seen(&value));
                Ok(value)
            }
            Err(failure) => {
                self.shown.push(seen_failure(&failure));
                Err(failure)
            }
        }
    }

    async fn fill(
        &mut self,
        args: Value,
    ) -> (Result<Value, crate::failure::Failure>, Map<String, Value>) {
        let mut audit = Map::new();
        let result =
            crate::browser::fill::run(&self.browsers, &self.entry, &args, &mut audit).await;
        self.shown.push(seen(&Value::Object(audit.clone())));
        match &result {
            Ok(value) => self.shown.push(seen(value)),
            Err(failure) => self.shown.push(seen_failure(failure)),
        }
        (result, audit)
    }

    /// `fill` as the host gates it: one releasing a secret field stops for
    /// a confirmation first.
    async fn gated_fill(
        &mut self,
        args: Value,
    ) -> (Result<Value, crate::failure::Failure>, Map<String, Value>) {
        let mut audit = Map::new();
        let mut args = args;
        args["request"] = json!(format!("r-fill-{}", self.shown.len()));
        let result = host::fill(
            &self.browsers,
            &self.entry,
            &self.id,
            CALLER.into(),
            &args,
            &mut audit,
        )
        .await;
        self.shown.push(seen(&Value::Object(audit.clone())));
        if let Err(failure) = &result {
            self.shown.push(seen_failure(failure));
        }
        (result, audit)
    }

    /// latch's suggestion for `item` on the page now, by `observe`: the
    /// fill's arguments that take it.
    async fn suggested(&mut self, item: &str) -> Value {
        let observed = self.call("observe", json!({})).await.unwrap();
        let suggestion = observed["fill"]["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["item"] == item)
            .unwrap_or_else(|| panic!("no suggestion for {item}: {}", observed["fill"]))
            .clone();
        json!({"item": item, "suggested": suggestion["id"], "snapshot": observed["snapshot"]})
    }

    /// A sign-in with `item`, taking latch's suggestion for the page.
    async fn login(
        &mut self,
        item: &str,
    ) -> (Result<Value, crate::failure::Failure>, Map<String, Value>) {
        let args = self.suggested(item).await;
        self.fill(args).await
    }

    async fn text(&mut self) -> String {
        self.call("text", json!({})).await.unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn find(result: &Value, role: &str, name: &str) -> String {
    super::find(result, role, name)
}

async fn vault_fills_go_only_where_the_item_allows_and_never_show_a_value(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[
                    ("username", "ada@shop.example"),
                    ("password", PASSWORD),
                    ("totp", KEY),
                ],
            ))
        })
        .unwrap();
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("cardholder name", "Ada Lovelace"),
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let identity = vault
        .with(|s| {
            s.add(item(
                IDENTITY,
                "Home",
                &[],
                &[
                    ("first name", "Ada"),
                    ("last name", "Lovelace"),
                    ("address1", "12 St James Square"),
                    ("city", "London"),
                    ("postal code", "SW1Y 4JH"),
                    ("country", "GB"),
                ],
            ))
        })
        .unwrap();

    let pages = site();
    let lookalike = site();
    let local = format!("http://127.0.0.1:{pages}");
    let evil = format!("http://localhost:{lookalike}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![
            (local.clone(), "https://login.shop.example".into()),
            (evil.clone(), "https://shop.example.evil.example".into()),
        ],
    ));
    let mut h = Harness::start(browsers).await;

    // A login on a subdomain of its site, through latch's suggestion: the
    // email field before the password, and the form's one submit button.
    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (signed_in, audit) = h.login(&login.item).await;
    let signed_in = signed_in.unwrap();
    println!(
        "login: submitted {} -> {:?}; audit {}",
        signed_in["submitted"],
        signed_in["title"],
        Value::Object(audit.clone())
    );
    assert_eq!(signed_in["submitted"], true);
    assert!(h.text().await.contains("password accepted"));
    assert_eq!(audit["agent_code_before_fill"], false);
    assert!(audit["mapping"]["username"].is_array() && audit["mapping"]["password"].is_array());
    assert!(audit["source"].as_str().unwrap().starts_with('s'));

    // Refused, with nothing written: a lookalike site, and a sandboxed frame.
    // The login doesn't fit the lookalike, so latch suggests nothing there;
    // the agent's own mapping is refused.
    let page = h
        .call("open", json!({"url": format!("{evil}/login.html")}))
        .await
        .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    assert_eq!(observed["fill"]["suggestions"], json!([]));
    let mapped = |page: &Value| {
        json!({
            "item": login.item,
            "fields": [["username", [find(page, "textbox", "Email")]],
                       ["password", [find(page, "textbox", "Password")]]],
            "press": find(page, "button", "Sign in"),
        })
    };
    let (refused, _) = h.fill(mapped(&page)).await;
    let refused = refused.unwrap_err();
    println!("lookalike: {} ({})", refused.code, refused.message);
    assert_eq!(
        (refused.exit, refused.code.as_str()),
        (crate::failure::Exit::Denied, "origin_mismatch")
    );
    let observed = h.call("observe", json!({})).await.unwrap();
    assert!(
        observed["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("value").is_none()),
        "nothing filled"
    );
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/sandboxed-login.html")}),
        )
        .await
        .unwrap();
    let (refused, _) = h.fill(mapped(&page)).await;
    let refused = refused.unwrap_err();
    println!("sandboxed frame: {} ({})", refused.code, refused.message);
    assert_eq!(refused.code, "origin_mismatch");
    assert!(refused.message.contains("opaque"));

    // During a fill, the agent gets session_paused for observe and screenshots.
    h.call("open", json!({"url": format!("{local}/slow-login.html")}))
        .await
        .unwrap();
    h.call("eval", json!({"script": "document.title"}))
        .await
        .unwrap();
    let args = h.suggested(&login.item).await;
    let (browsers, entry) = (h.browsers.clone(), h.entry.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result = crate::browser::fill::run(&browsers, &entry, &args, &mut audit).await;
        (result, audit)
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for command in ["observe", "screenshot", "eval"] {
        let paused = h.call(command, json!({"script": "1"})).await.unwrap_err();
        println!("{command} during the fill: {}", paused.code);
        assert_eq!(paused.code, "session_paused");
    }
    let (slow, audit) = filling.await.unwrap();
    h.shown.push(seen(&Value::Object(audit.clone())));
    assert_eq!(slow.unwrap()["submitted"], true);
    // Agent code ran in this session before that fill, and the audit says so.
    assert_eq!(audit["agent_code_before_fill"], true);
    assert!(h.text().await.contains("password accepted"));

    // A card: secret fields only with --press; reshaped expiry; gone after.
    let checkout = h
        .call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    let fields = json!([
        [
            "cardholder name",
            [find(&checkout, "textbox", "Name on card")]
        ],
        ["number", [find(&checkout, "textbox", "Card number")]],
        ["expiry", [find(&checkout, "textbox", "Expiry")]],
        ["cvv", [find(&checkout, "textbox", "CVV")]],
    ]);
    let (no_press, _) = h
        .gated_fill(json!({"item": card.item, "fields": fields}))
        .await;
    let no_press = no_press.unwrap_err();
    println!("card without --press: {}", no_press.code);
    assert_eq!(
        (no_press.exit, no_press.code.as_str()),
        (crate::failure::Exit::Denied, "press_required")
    );
    // With --press, it stops for the agent's confirmation before any value
    // is released; the confirmation fills, presses and cleans up.
    let pay = find(&checkout, "button", "Pay");
    let (stop, _) = h
        .gated_fill(json!({"item": card.item, "fields": fields, "press": pay}))
        .await;
    let stop = stop.unwrap_err();
    println!("card with --press: {} {}", stop.code, stop.details);
    assert_eq!(
        (stop.exit, stop.code.as_str()),
        (crate::failure::Exit::Denied, "confirm_needed")
    );
    let issued = &stop.details["confirmation"];
    assert_eq!(issued["item_title"], "Test Visa");
    // The exact mapping, by the page's names, and where its values go.
    assert_eq!(issued["mapping"].as_array().unwrap().len(), 4);
    assert_eq!(issued["mapping"][1]["name"], "Card number");
    assert_eq!(issued["mapping"][1]["evidence"], "autocomplete=cc-number");
    assert_eq!(issued["frames"], json!(["https://login.shop.example"]));
    assert_eq!(issued["target"]["name"], "Pay");
    assert!(issued["title"].is_string() && issued["lines"].is_array());
    let handle = &issued["screenshot"];
    let png = crate::browser::steps::shot(
        &h.browsers,
        &h.id,
        CALLER,
        handle["request"].as_str().unwrap(),
        handle["shot"].as_str().unwrap(),
    )
    .unwrap();
    assert!(png.starts_with(b"\x89PNG"), "the confirmation's screenshot");
    let untouched = h.call("observe", json!({})).await.unwrap();
    assert!(
        untouched["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["filled"].is_null() && e["value"].is_null()),
        "nothing was released before the confirmation"
    );
    let mut audit = Map::new();
    let paid = crate::browser::steps::run(
        &h.browsers,
        &h.entry,
        &h.id,
        CALLER,
        None,
        &json!({"request": "r-card", "plan": {"steps": []}, "confirm": issued["id"]}),
        &mut audit,
    )
    .await
    .unwrap();
    h.shown.push(seen(&paid));
    h.shown.push(seen(&Value::Object(audit.clone())));
    println!("card confirmed: {}", paid["trail"]);
    assert_eq!(paid["trail"][0]["op"], "fill");
    assert_eq!(paid["trail"][0]["submitted"], true);
    let audit = audit["step_records"][0].as_object().unwrap().clone();
    println!(
        "card: audit sites: top {} frames {}",
        audit["top_origin"], audit["frame_origins"]
    );
    let page = h.text().await;
    println!("checkout answered: {page}");
    assert!(
        page.contains("number ok, expiry ok (04 / 31), cvv ok, name Ada Lovelace"),
        "{page}"
    );
    assert_eq!(audit["top_origin"], json!(local));
    assert_eq!(audit["frame_origins"], json!([local]));
    let after = seen(&h.call("observe", json!({})).await.unwrap());
    assert!(!after.contains(NUMBER) && !after.contains(CVV));

    // A confirmed card fill whose page reprices once the number is in: Pay
    // is not pressed, the values are cleared, and the agent is let back in.
    let checkout = h
        .call(
            "open",
            json!({"url": format!("{local}/reprice-checkout.html")}),
        )
        .await
        .unwrap();
    let fields = json!([
        ["number", [find(&checkout, "textbox", "Card number")]],
        ["expiry", [find(&checkout, "textbox", "Expiry")]],
        ["cvv", [find(&checkout, "textbox", "CVV")]],
    ]);
    let pay = find(&checkout, "button", "Pay");
    let (stop, _) = h
        .gated_fill(json!({"item": card.item, "fields": fields, "press": pay}))
        .await;
    let stop = stop.unwrap_err();
    assert_eq!(stop.code, "confirm_needed");
    assert!(
        stop.details["confirmation"]["lines"]
            .to_string()
            .contains("$10.00")
    );
    let mut audit = Map::new();
    let repriced = crate::browser::steps::run(
        &h.browsers,
        &h.entry,
        &h.id,
        CALLER,
        None,
        &json!({"request": "r-reprice", "plan": {"steps": []},
                "confirm": stop.details["confirmation"]["id"]}),
        &mut audit,
    )
    .await
    .unwrap_err();
    h.shown.push(seen_failure(&repriced));
    h.shown.push(seen(&Value::Object(audit.clone())));
    println!(
        "repriced during the fill: {} ({})",
        repriced.code, repriced.message
    );
    assert_eq!(
        (repriced.exit, repriced.code.as_str()),
        (crate::failure::Exit::Denied, "confirm_stale")
    );
    let page = h.text().await;
    assert!(
        page.contains("$100.00") && !page.contains("payment:"),
        "{page}"
    );
    // The agent is back (the cleanup was confirmed), and the fields are
    // empty: eval, refused while one holds anything, reads them.
    let left = h
        .call(
            "eval",
            json!({"script": "['cc-number', 'cc-exp', 'cc-csc'].map((n) => document.querySelector(`[name=${n}]`).value)"}),
        )
        .await
        .unwrap();
    assert_eq!(left["value"], json!(["", "", ""]));

    // An identity's ordinary fields fill without --press; while they are on
    // the page, eval is refused.
    let address = h
        .call("open", json!({"url": format!("{local}/address.html")}))
        .await
        .unwrap();
    let fields = json!([
        ["first name", [find(&address, "textbox", "First name")]],
        ["last name", [find(&address, "textbox", "Last name")]],
        ["address1", [find(&address, "textbox", "Address")]],
        ["city", [find(&address, "textbox", "City")]],
        ["postal code", [find(&address, "textbox", "Postcode")]],
        [
            "country",
            [address["elements"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["role"] == "combobox")
                .unwrap()["ref"]
                .clone()]
        ],
    ]);
    let (shipped, _) = h
        .fill(json!({"item": identity.item, "fields": fields}))
        .await;
    let shipped = shipped.unwrap();
    assert_eq!(shipped["submitted"], false);
    // latch shows that the fields hold vault values, never the values.
    let marked = shipped["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["filled"] == true && e.get("value").is_none())
        .count();
    assert_eq!(marked, 6);
    let page = h.text().await;
    println!(
        "address form, as the page says: {}",
        page.lines().find(|l| l.starts_with("form:")).unwrap_or("")
    );
    assert!(
        page.contains("form: Ada | Lovelace | 12 St James Square | London | SW1Y 4JH | GB"),
        "{page}"
    );
    let refused = h.call("eval", json!({"script": "1"})).await.unwrap_err();
    println!("eval with the address on the page: {}", refused.code);
    assert_eq!(
        (refused.exit, refused.code.as_str()),
        (crate::failure::Exit::Denied, "secret_on_page")
    );

    // An authenticator code, computed on the host and checked by the page.
    // No suggestion for a code field: the agent maps `otp` itself.
    let page = h
        .call("open", json!({"url": format!("{local}/totp.html")}))
        .await
        .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    assert_eq!(observed["fill"]["page_offers"], json!(["code"]));
    assert_eq!(observed["fill"]["suggestions"], json!([]));
    let (verified, audit) = h
        .fill(json!({
            "item": login.item,
            "fields": [["otp", [find(&page, "textbox", "Authentication code")]]],
            "press": find(&page, "button", "Verify"),
        }))
        .await;
    assert_eq!(verified.unwrap()["submitted"], true);
    assert!(audit["mapping"]["otp"].is_array());
    let page = h.text().await;
    println!("code page answered: {page}");
    assert!(page.contains("code accepted"));

    // No value anywhere an agent or the audit could see.
    let params = Params::parse(KEY).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let codes: Vec<String> = [now - 60, now - 30, now, now + 30]
        .iter()
        .map(|t| params.code(*t))
        .collect();
    let everything = h.shown.join("\n");
    for value in [PASSWORD, NUMBER, CVV, KEY]
        .iter()
        .map(|s| s.to_string())
        .chain(codes)
    {
        assert!(
            !everything.contains(&value),
            "a value reached a result or the audit"
        );
    }
    println!(
        "grep: none of the password, card number, CVV, key or codes in {} results and audit records",
        h.shown.len()
    );
    h.call("close", json!({})).await.ok();
    let _ = h.browsers.vault.with(|s| Ok(s.list()));
}

/// The vault's files from inside `exec` and a file worker: never readable.
#[test]
fn exec_and_file_workers_cannot_read_the_vault_or_its_key() {
    let (_dir, state, area) = test_state("cft", &[]);
    let vault = Vault::new(state.clone());
    vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("password", PASSWORD)],
            ))
        })
        .unwrap();
    let real = state.canonicalize().unwrap();
    for file in ["vault.json", "vault.key"] {
        let path = real.join(file);
        assert!(path.exists());
        let (ok, text) = exec(&state, &area, &format!("cat '{}'", path.display()));
        println!("exec: cat {file}: {}", text.trim());
        assert!(!ok && text.contains(HIDDEN), "{text}");
        let out = std::process::Command::new(latch())
            .args(["worker", "--area"])
            .arg(&area)
            .arg("--denied")
            .arg(&state)
            .args(["read", "--"])
            .arg(&path)
            .env_clear()
            .current_dir(&area)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        println!("file worker: read {file}: {}", stderr.trim());
        // Seatbelt refuses the read; Linux's bubblewrap leaves the file out.
        let refused = if cfg!(target_os = "macos") {
            "sandbox_denied"
        } else {
            "not_found"
        };
        assert!(!out.status.success() && stderr.contains(refused));
        assert!(out.stdout.is_empty());
    }
}

/// Fills that cannot finish: no submit to press, a page that covers its
/// submit and keeps the value. Nothing is filled without a way to send it,
/// no reading shows a filled value, and a fill that cannot confirm the
/// value left the page keeps the agent out.
async fn a_fill_that_cannot_finish_shows_no_value_and_keeps_the_agent_out(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("username", "ada@shop.example"), ("password", PASSWORD)],
            ))
        })
        .unwrap();
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let pages = site();
    let local = format!("http://127.0.0.1:{pages}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;

    // A password with no press button: refused before any value is
    // released, whether the agent mapped it or took a suggestion with no
    // press (a page with no submit button gets one with none).
    let page = h
        .call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (named, _) = h
        .fill(json!({
            "item": login.item,
            "fields": [["username", [find(&page, "textbox", "Email")]],
                       ["password", [find(&page, "textbox", "Password")]]],
        }))
        .await;
    let named = named.unwrap_err();
    println!(
        "named fields, no --press: {} ({})",
        named.code, named.message
    );
    assert_eq!(
        (named.exit, named.code.as_str()),
        (crate::failure::Exit::Denied, "press_required")
    );
    h.call(
        "open",
        json!({"url": format!("{local}/no-submit-login.html")}),
    )
    .await
    .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    let suggestion = &observed["fill"]["suggestions"][0];
    println!("no submit button: suggestion {suggestion}");
    assert!(suggestion["press"].is_null());
    let args = h.suggested(&login.item).await;
    let (refused, _) = h.fill(args).await;
    let refused = refused.unwrap_err();
    println!(
        "its suggestion, no --press: {} ({})",
        refused.code, refused.message
    );
    assert_eq!(refused.code, "press_required");
    assert!(
        h.call("eval", json!({"script": "1"})).await.is_ok(),
        "nothing was left on the page"
    );

    // A page that covers its submit and keeps the value: the fill stops,
    // its error shows no value, and the session stays paused.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/trap-checkout.html")}),
        )
        .await
        .unwrap();
    let fields = json!([
        ["number", [find(&page, "textbox", "Card number")]],
        ["expiry", [find(&page, "textbox", "Expiry")]],
        ["cvv", [find(&page, "textbox", "CVV")]],
    ]);
    let (trapped, audit) = h
        .fill(json!({"item": card.item, "fields": fields, "press": find(&page, "button", "Pay")}))
        .await;
    let trapped = trapped.unwrap_err();
    println!(
        "trap: {} ({}); audit {}",
        trapped.code,
        trapped.message,
        Value::Object(audit)
    );
    assert_eq!(trapped.code, "session_paused");
    for command in ["observe", "screenshot", "text"] {
        assert_eq!(
            h.call(command, json!({})).await.unwrap_err().code,
            "session_paused",
            "{command}"
        );
    }
    // A later fill that fails before releasing anything does not let the
    // agent back in: the trapped value is still on the page.
    let (later, _) = h
        .fill(
            json!({"item": login.item, "fields": [["username", ["e99"]], ["password", ["e98"]]],
                     "press": "e97"}),
        )
        .await;
    let later = later.unwrap_err();
    println!(
        "a later fill on the trapped page: {} ({})",
        later.code, later.message
    );
    assert_eq!(later.code, "session_paused");
    assert_eq!(
        h.call("observe", json!({})).await.unwrap_err().code,
        "session_paused",
        "still paused after the later fill"
    );
    let everything = h.shown.join("\n");
    for value in [PASSWORD, NUMBER, CVV] {
        assert!(
            !everything.contains(value),
            "a value reached a result, an error or the audit"
        );
    }
    h.call("close", json!({})).await.ok();
}

/// A fill in one session holds the whole browser: every other session on
/// the profile gets session_paused until the fill's cleanup is confirmed,
/// and stays out while a value a fill could not clear may be on a page.
async fn a_fill_in_one_session_pauses_every_session_on_its_browser(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("username", "ada@shop.example"), ("password", PASSWORD)],
            ))
        })
        .unwrap();
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let pages = site();
    let local = format!("http://127.0.0.1:{pages}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;
    // Another caller's session on the same profile.
    let other = "another-caller";
    let browsers = h.browsers.clone();
    let mut shown_b = Vec::new();
    let mut call_b = async |command: &str, name: &str, args: Value| {
        let mut audit = Map::new();
        let result = host::call(
            &browsers,
            other.into(),
            command,
            name,
            &args,
            None,
            &mut audit,
        )
        .await
        .map(|(result, _)| result);
        shown_b.push(seen(&Value::Object(audit)));
        shown_b.push(match &result {
            Ok(value) => seen(value),
            Err(failure) => seen_failure(failure),
        });
        result
    };
    let b = call_b(
        "open",
        "default",
        json!({"url": format!("{local}/next.html")}),
    )
    .await
    .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();

    // While a login fills in this session, the other gets nothing.
    h.call("open", json!({"url": format!("{local}/slow-login.html")}))
        .await
        .unwrap();
    let args = h.suggested(&login.item).await;
    let (browsers, entry) = (h.browsers.clone(), h.entry.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result = crate::browser::fill::run(&browsers, &entry, &args, &mut audit).await;
        (result, audit)
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for command in ["observe", "screenshot", "eval"] {
        let paused = call_b(command, &b, json!({"script": "1"}))
            .await
            .unwrap_err();
        println!(
            "{command} in the other session during the fill: {}",
            paused.code
        );
        assert_eq!(paused.code, "session_paused");
    }
    let opened = call_b(
        "open",
        "default",
        json!({"url": format!("{local}/next.html")}),
    )
    .await
    .unwrap_err();
    println!("a new session during the fill: {}", opened.code);
    assert_eq!(opened.code, "session_paused");
    let (slow, audit) = filling.await.unwrap();
    h.shown.push(seen(&Value::Object(audit)));
    assert_eq!(slow.unwrap()["submitted"], true);
    let seen = call_b("observe", &b, json!({})).await.unwrap();
    println!("the other session after cleanup: {}", seen["title"]);
    assert_eq!(seen["title"], "latch next page");

    // A fill whose cleanup can't be confirmed keeps every session out,
    // until the session it filled in is closed.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/trap-checkout.html")}),
        )
        .await
        .unwrap();
    let fields = json!([
        ["number", [find(&page, "textbox", "Card number")]],
        ["expiry", [find(&page, "textbox", "Expiry")]],
        ["cvv", [find(&page, "textbox", "CVV")]],
    ]);
    let (trapped, _) = h
        .fill(json!({"item": card.item, "fields": fields, "press": find(&page, "button", "Pay")}))
        .await;
    assert_eq!(trapped.unwrap_err().code, "session_paused");
    let held = call_b("observe", &b, json!({})).await.unwrap_err();
    println!("the other session while the trap holds: {}", held.code);
    assert_eq!(held.code, "session_paused");
    h.call("close", json!({})).await.unwrap();
    if engine == "cft" {
        let seen = call_b("observe", &b, json!({})).await.unwrap();
        println!("the other session once it is closed: {}", seen["title"]);
        assert_eq!(seen["title"], "latch next page");
        call_b("close", &b, json!({})).await.unwrap();
    } else {
        // Camoufox can't close a page that may hold a secret without
        // letting it send it: the browser was killed instead.
        let gone = call_b("observe", &b, json!({})).await.unwrap_err();
        println!("the other session once it is closed: {}", gone.code);
        assert_eq!(gone.code, "browser_gone");
        call_b("close", &b, json!({})).await.ok();
    }

    let everything = format!("{}\n{}", h.shown.join("\n"), shown_b.join("\n"));
    for value in [PASSWORD, NUMBER, CVV] {
        assert!(
            !everything.contains(value),
            "a value reached a result, an error or the audit"
        );
    }
}

/// Agent code is noted per site, with the profile: a fill's audit says
/// whether `eval` ever ran on its sites, in any session, before this
/// runtime or in it, and not when it ran only on another site.
async fn agent_code_is_noted_per_site_and_kept_with_the_profile(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("username", "ada@shop.example"), ("password", PASSWORD)],
            ))
        })
        .unwrap();
    let pages = site();
    let local = format!("http://127.0.0.1:{pages}");
    let elsewhere = format!("http://localhost:{pages}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;
    let login_once = async |h: &mut Harness| {
        h.call("open", json!({"url": format!("{local}/login.html")}))
            .await
            .unwrap();
        let (signed_in, audit) = h.login(&login.item).await;
        assert_eq!(signed_in.unwrap()["submitted"], true);
        audit["agent_code_before_fill"].clone()
    };

    // Agent code on another site: a fill here is not flagged.
    h.call("open", json!({"url": format!("{elsewhere}/next.html")}))
        .await
        .unwrap();
    h.call("eval", json!({"script": "1"})).await.unwrap();
    let flagged = login_once(&mut h).await;
    println!("after eval on localhost, a fill on 127.0.0.1: agent_code_before_fill {flagged}");
    assert_eq!(flagged, false);

    // On this site, in a session since closed, in a runtime since ended:
    // a later fill is flagged.
    h.call("eval", json!({"script": "1"})).await.unwrap();
    h.call("close", json!({})).await.unwrap();
    let kept: Value = serde_json::from_slice(
        &std::fs::read(state.join("browser/profiles/default/agent-code.json")).unwrap(),
    )
    .unwrap();
    println!("kept with the profile: {kept}");
    assert_eq!(kept, json!(["127.0.0.1", "localhost"]));
    let (id, entry) = h.browsers.open("default", CALLER, None).await.unwrap();
    (h.id, h.entry) = (id, entry);
    let flagged = login_once(&mut h).await;
    println!("in a new runtime, a fill on 127.0.0.1: agent_code_before_fill {flagged}");
    assert_eq!(flagged, true);
    h.call("close", json!({})).await.unwrap();
}

/// Observe lists a login whose form sits in a frame on its own site, under
/// a page it allows as another top-level site: the rule is applied to the
/// origins of the frames that hold fields, not to the page alone.
async fn observe_lists_items_by_the_frames_that_hold_fields(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let embedded = vault
        .with(|s| {
            s.add(Item {
                other_top_sites: vec!["portal.example".into()],
                ..item(
                    LOGIN,
                    "Shop via portal",
                    &["shop.example"],
                    &[("password", PASSWORD)],
                )
            })
        })
        .unwrap();
    let plain = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("password", PASSWORD)],
            ))
        })
        .unwrap();
    let (portal, login) = (site(), site());
    let top = format!("http://127.0.0.1:{portal}");
    let frame = format!("http://localhost:{login}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![
            (top.clone(), "https://www.portal.example".into()),
            (frame.clone(), "https://login.shop.example".into()),
        ],
    ));
    let mut h = Harness::start(browsers).await;

    h.call(
        "open",
        json!({"url": format!("{top}/embed-login.html?cross={login}")}),
    )
    .await
    .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    println!(
        "portal page embedding the login frame: fill {}",
        observed["fill"]
    );
    let listed: Vec<&str> = observed["fill"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    // Only the login that allows the portal as its top-level site.
    assert_eq!(listed, [embedded.item.as_str()]);
    assert_ne!(listed, [plain.item.as_str()]);
    h.call("close", json!({})).await.ok();
}

async fn a_login_goes_on_to_its_password_stage_in_one_pause(engine: &str) {
    let (_dir, state, _area) = test_state_alone(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("username", "ada@shop.example"), ("password", PASSWORD)],
            ))
        })
        .unwrap();
    let (pages, provider, lookalike) = (site(), site(), site());
    let local = format!("http://127.0.0.1:{pages}");
    let provider_origin = format!("http://localhost:{provider}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![
            (local.clone(), "https://login.shop.example".into()),
            (provider_origin.clone(), "https://auth.shop.example".into()),
            (
                format!("http://localhost:{lookalike}"),
                "https://shop.example.evil.example".into(),
            ),
        ],
    ));
    let mut h = Harness::start(browsers).await;

    // A new document: the identifier stage's suggestion (the `username
    // webauthn` field and its own submit button), then the password stage,
    // which latch reads and fills itself, inside the one pause.
    h.call("open", json!({"url": format!("{local}/idf.html")}))
        .await
        .unwrap();
    let observed = h.call("observe", json!({})).await.unwrap();
    println!("identifier stage: {}", observed["fill"]);
    assert_eq!(
        observed["fill"]["page_offers"],
        json!(["username_only", "passkey"])
    );
    let suggestion = &observed["fill"]["suggestions"][0];
    assert_eq!(suggestion["fields"].as_object().unwrap().len(), 1);
    assert_eq!(
        suggestion["press"],
        json!(find(&observed, "button", "Continue"))
    );
    let (signed_in, audit) = h.login(&login.item).await;
    println!(
        "new document: {:?}; audit {}",
        signed_in.as_ref().map(|r| (&r["fields"], &r["stages"])),
        Value::Object(audit.clone())
    );
    let signed_in = signed_in.unwrap();
    assert_eq!(signed_in["stages"], 2);
    assert_eq!(signed_in["fields"][1]["field"], "password");
    assert!(h.text().await.contains("password accepted"));

    // The same document: the email form hides, the password form shows.
    h.call("open", json!({"url": format!("{local}/idf-spa.html")}))
        .await
        .unwrap();
    let (signed_in, _) = h.login(&login.item).await;
    let signed_in = signed_in.unwrap();
    println!("same document: stages {}", signed_in["stages"]);
    assert_eq!(signed_in["stages"], 2);
    assert!(h.text().await.contains("password accepted"));

    // The agent's own mapping of a page with no tokens, whose second page is
    // on another of the item's sites a second and a half later: the agent is
    // kept out between the stages.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/id-first-email.html?next={provider}")}),
        )
        .await
        .unwrap();
    let args = json!({
        "item": login.item,
        "fields": [["username", [find(&page, "textbox", "Email")]]],
        "press": find(&page, "button", "Next"),
    });
    let (browsers, entry, mapped) = (h.browsers.clone(), h.entry.clone(), args.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result = crate::browser::fill::run(&browsers, &entry, &mapped, &mut audit).await;
        (result, audit)
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let paused = h.call("observe", json!({})).await.unwrap_err();
    println!("observe between the stages: {}", paused.code);
    assert_eq!(paused.code, "session_paused");
    let (signed_in, audit) = filling.await.unwrap();
    h.shown.push(seen(&Value::Object(audit.clone())));
    let signed_in = signed_in.unwrap();
    h.shown.push(seen(&signed_in));
    assert_eq!(signed_in["stages"], 2);
    assert!(h.text().await.contains("password accepted"));
    assert_eq!(audit["top_origin"], json!(local));
    assert_eq!(audit["second_stage"]["top_origin"], json!(provider_origin));

    // The second page on a lookalike: nothing goes there, and the agent is
    // let back in.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/id-first-email.html?next={lookalike}")}),
        )
        .await
        .unwrap();
    let (refused, _) = h
        .fill(json!({
            "item": login.item,
            "fields": [["username", [find(&page, "textbox", "Email")]]],
            "press": find(&page, "button", "Next"),
        }))
        .await;
    let refused = refused.unwrap_err();
    println!(
        "lookalike second page: {} ({})",
        refused.code, refused.message
    );
    assert_eq!(refused.code, "origin_mismatch");
    let observed = h.call("observe", json!({})).await.unwrap();
    assert!(
        observed["url"]
            .as_str()
            .unwrap()
            .contains("id-first-password.html")
    );
    assert!(
        observed["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("filled").is_none() && e.get("value").is_none()),
        "nothing filled on the lookalike"
    );

    // Second stages latch does not fill by itself: a code, a method chooser,
    // a form with two submit buttons, and a password with no form or submit
    // button (cross-site frames coming and going meanwhile). Each ends after
    // cleanup with needs_fields and the new stage, the password unreleased.
    for (url, offers) in [
        (format!("{local}/idf.html?then=code"), json!(["code"])),
        (format!("{local}/idf.html?then=chooser"), json!([])),
        (format!("{local}/idf.html?then=two"), json!(["password"])),
        (
            format!("{local}/id-first.html?cross={provider}"),
            json!(["password"]),
        ),
    ] {
        let page = h.call("open", json!({"url": url})).await.unwrap();
        let (ended, audit) = h
            .fill(json!({
                "item": login.item,
                "fields": [["username", [find(&page, "textbox", "Email")]]],
                "press": find(&page, "button", "Continue"),
            }))
            .await;
        let ended = ended.unwrap_err();
        println!(
            "{url}: {} ({}); offers {}, alternatives {}; second stage {}",
            ended.code,
            ended.message,
            ended.details["fill"]["page_offers"],
            ended.details["fill"]["alternatives"],
            audit["second_stage"]
        );
        assert_eq!(
            (ended.exit, ended.code.as_str()),
            (crate::failure::Exit::Failed, "needs_fields")
        );
        assert_eq!(ended.details["first_stage"], "submitted");
        assert_eq!(ended.details["fill"]["page_offers"], offers);
        assert!(ended.details["page"]["snapshot"].is_string());
        if url.contains("chooser") {
            let names: Vec<&str> = ended.details["fill"]["alternatives"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|a| a["name"].as_str())
                .collect();
            assert!(
                names.contains(&"Text me a code") && names.contains(&"Use your password"),
                "{names:?}"
            );
        } else {
            assert!(!ended.details["candidates"].as_array().unwrap().is_empty());
        }
        let text = h.text().await;
        assert!(!text.contains("accepted"), "{text}");
        assert!(
            h.call("eval", json!({"script": "1"})).await.is_ok(),
            "the agent is back"
        );
    }

    let everything = h.shown.join("\n");
    assert!(
        !everything.contains(PASSWORD),
        "the password reached a result or the audit"
    );
    h.call("close", json!({})).await.ok();
}

/// What `observe` offers, from the page's own hints only, and the checks a
/// mapping passes before any value leaves the vault.
async fn observe_suggests_from_hints_and_the_checks_refuse_contradictions(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[
                    ("username", "ada@shop.example"),
                    ("password", PASSWORD),
                    ("totp", KEY),
                ],
            ))
        })
        .unwrap();
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("cardholder name", "Ada Lovelace"),
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let pages = site();
    let local = format!("http://127.0.0.1:{pages}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;
    let open = async |h: &mut Harness, page: &str| {
        h.call("open", json!({"url": format!("{local}/{page}")}))
            .await
            .unwrap();
        h.call("observe", json!({})).await.unwrap()
    };
    let suggested_for = |observed: &Value, item: &str| -> Vec<Value> {
        observed["fill"]["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["item"] == item)
            .cloned()
            .collect()
    };

    // The items that fit, with what they hold and never a value.
    let observed = open(&mut h, "login.html").await;
    let items = &observed["fill"]["items"];
    println!("login page items: {items}");
    let shop = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item"] == login.item.as_str())
        .unwrap();
    let ids: Vec<&str> = shop["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"username") && ids.contains(&"password") && ids.contains(&"otp"),
        "{ids:?}"
    );
    assert!(!ids.contains(&"totp"));
    let otp = shop["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "otp")
        .unwrap();
    assert_eq!(
        (otp["derived"].as_str(), otp["secret"].as_bool()),
        (Some("totp"), Some(true))
    );
    assert!(
        !observed["fill"].to_string().contains("ada@shop.example"),
        "no username shown"
    );

    // A suggestion fills exactly its mapping.
    let [made] = suggested_for(&observed, &login.item).try_into().unwrap();
    println!("login.html suggestion: {made}");
    assert_eq!(
        made["evidence"]
            .as_object()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        json!(["the text field before the password", "type=password"])
            .as_array()
            .unwrap()
            .clone()
    );
    let (filled, audit) = h
        .fill(
            json!({"item": login.item, "suggested": made["id"], "snapshot": observed["snapshot"]}),
        )
        .await;
    assert_eq!(filled.unwrap()["submitted"], true);
    assert_eq!(audit["mapping"], made["fields"]);
    assert_eq!(audit["source"], made["id"]);

    // Two forms with the same tokens: one suggestion each.
    let observed = open(&mut h, "two-logins.html").await;
    let made = suggested_for(&observed, &login.item);
    println!("two forms: {} suggestions", made.len());
    assert_eq!(made.len(), 2);
    assert_ne!(made[0]["press"], made[1]["press"]);

    // No suggestion: registration forms, a newsletter box.
    for page in ["signup.html", "signup.html?two=1", "newsletter.html"] {
        let observed = open(&mut h, page).await;
        println!(
            "{page}: offers {}, suggestions {}",
            observed["fill"]["page_offers"], observed["fill"]["suggestions"]
        );
        assert!(suggested_for(&observed, &login.item).is_empty(), "{page}");
    }
    // A passkey-only page: what it asks for, and its other buttons.
    let observed = open(&mut h, "passkey-only.html").await;
    let names: Vec<&str> = observed["fill"]["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    println!(
        "passkey-only: offers {}, alternatives {names:?}",
        observed["fill"]["page_offers"]
    );
    assert_eq!(observed["fill"]["page_offers"], json!(["passkey"]));
    assert!(names.contains(&"Sign in with a passkey") && names.contains(&"Continue with Google"));
    let observed = open(&mut h, "idf.html").await;
    let names: Vec<&str> = observed["fill"]["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(
        names.contains(&"Sign in with a passkey") && names.contains(&"Continue with Google"),
        "{names:?}"
    );

    // Hidden copies of the card fields are never listed, so never suggested.
    let observed = open(&mut h, "dupes-checkout.html").await;
    let [made] = suggested_for(&observed, &card.item).try_into().unwrap();
    println!("checkout with hidden copies: {made}");
    let listed = observed["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["role"] == "textbox" || e["role"] == "searchbox")
        .count();
    let inputs = h
        .call(
            "eval",
            json!({"script": "document.querySelectorAll('input, textarea').length"}),
        )
        .await
        .unwrap();
    println!("fields listed {listed} of {} on the page", inputs["value"]);
    assert_eq!((listed, inputs["value"].as_u64()), (8, Some(11)));
    assert_eq!(
        made["fields"]["number"],
        json!([find(&observed, "textbox", "Card number")])
    );
    assert_eq!(made["press"], json!(find(&observed, "button", "Pay")));

    // Refused, naming the field and why, with nothing released: what the
    // page's markup contradicts.
    let pay = find(&observed, "button", "Pay");
    for (field, target, why) in [
        (
            "number",
            find(&observed, "textbox", "Gift note"),
            "textarea",
        ),
        (
            "number",
            find(&observed, "searchbox", "Search help"),
            "search",
        ),
        (
            "number",
            find(&observed, "textbox", "Receipt email"),
            "email",
        ),
    ] {
        let (refused, _) = h
            .gated_fill(json!({"item": card.item, "fields": [[field, [target]]], "press": pay}))
            .await;
        let refused = refused.unwrap_err();
        println!(
            "card {field} into {target}: {} ({})",
            refused.code, refused.message
        );
        assert_eq!(
            (refused.exit, refused.code.as_str()),
            (crate::failure::Exit::Denied, "bad_target")
        );
        assert!(refused.message.contains(why), "{}", refused.message);
    }
    let number = find(&observed, "textbox", "Card number");
    let (twice, _) = h
        .gated_fill(json!({"item": card.item, "fields": [["number", [number]], ["cvv", [number]]], "press": pay}))
        .await;
    let twice = twice.unwrap_err();
    println!(
        "two fields into one element: {} ({})",
        twice.code, twice.message
    );
    assert_eq!(twice.code, "bad_target");
    let observed = open(&mut h, "login.html").await;
    let (email, password, sign_in) = (
        find(&observed, "textbox", "Email"),
        find(&observed, "textbox", "Password"),
        find(&observed, "button", "Sign in"),
    );
    for fields in [
        json!([["password", [email]]]),
        json!([["username", [password]], ["password", [email]]]),
    ] {
        let (refused, _) = h
            .fill(json!({"item": login.item, "fields": fields, "press": sign_in}))
            .await;
        let refused = refused.unwrap_err();
        println!("login {fields}: {} ({})", refused.code, refused.message);
        assert_eq!(refused.code, "bad_target");
    }
    let observed = open(&mut h, "signup.html").await;
    let (refused, _) = h
        .fill(
            json!({"item": login.item, "press": find(&observed, "button", "Create account"),
                     "fields": [["password", [find(&observed, "textbox", "New password")]]]}),
        )
        .await;
    let refused = refused.unwrap_err();
    println!(
        "a password into new-password: {} ({})",
        refused.code, refused.message
    );
    assert!(refused.code == "bad_target" && refused.message.contains("new-password"));
    let observed = open(&mut h, "idf-password.html").await;
    let (refused, _) = h
        .fill(
            json!({"item": login.item, "press": find(&observed, "button", "Sign in"),
                     "fields": [["otp", [find(&observed, "textbox", "Password")]]]}),
        )
        .await;
    let refused = refused.unwrap_err();
    println!(
        "otp into the password field: {} ({})",
        refused.code, refused.message
    );
    assert_eq!(refused.code, "bad_target");
    assert!(
        h.call("eval", json!({"script": "1"})).await.is_ok(),
        "nothing was released"
    );

    // Allowed: a revealed password; an email into a box marked as a phone
    // number, flagged; an unmarked card field on the agent's map, whose
    // confirmation shows that map.
    let observed = open(&mut h, "revealed.html").await;
    let (filled, _) = h
        .fill(
            json!({"item": login.item, "press": find(&observed, "button", "Sign in"),
                     "fields": [["username", [find(&observed, "textbox", "Email")]],
                                ["password", [find(&observed, "textbox", "Password")]]]}),
        )
        .await;
    assert_eq!(filled.unwrap()["submitted"], true);
    assert!(h.text().await.contains("password accepted"));
    let observed = open(&mut h, "wrong-token.html").await;
    let (filled, audit) = h
        .fill(
            json!({"item": login.item, "press": find(&observed, "button", "Log in"),
                     "fields": [["username", [find(&observed, "textbox", "Phone number or email")]],
                                ["password", [find(&observed, "textbox", "Password")]]]}),
        )
        .await;
    let filled = filled.unwrap();
    println!(
        "email into tel-national: fields {}; audit hint_mismatch {}",
        filled["fields"], audit["hint_mismatch"]
    );
    assert_eq!(filled["fields"][0]["hint_mismatch"], "tel-national");
    assert!(audit["hint_mismatch"].is_array());
    assert!(h.text().await.contains("password accepted"));
    let observed = open(&mut h, "plain-checkout.html").await;
    assert!(suggested_for(&observed, &card.item).is_empty());
    let (stop, _) = h
        .gated_fill(
            json!({"item": card.item, "press": find(&observed, "button", "Pay"),
                           "fields": [["number", [find(&observed, "textbox", "Card number")]],
                                      ["expiry", [find(&observed, "textbox", "Expiry")]],
                                      ["cvv", [find(&observed, "textbox", "CVV")]]]}),
        )
        .await;
    let stop = stop.unwrap_err();
    println!(
        "unmarked card fields: {} mapping {}",
        stop.code, stop.details["confirmation"]["mapping"]
    );
    assert_eq!(stop.code, "confirm_needed");
    assert_eq!(
        stop.details["confirmation"]["mapping"][0]["name"],
        "Card number"
    );

    // A suggestion that no longer comes out the same: a moved field, or a
    // changed item. Refused, with fresh suggestions, nothing released.
    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let args = h.suggested(&login.item).await;
    h.call("eval", json!({"script": "const e = document.querySelector('[name=email]'); e.replaceWith(e.cloneNode()); 1"}))
        .await
        .unwrap();
    let (stale, _) = h.fill(args).await;
    let stale = stale.unwrap_err();
    println!(
        "moved field: {} ({}); fresh {}",
        stale.code, stale.message, stale.details["fill"]["suggestions"]
    );
    assert_eq!(stale.code, "stale_snapshot");
    assert_eq!(
        stale.details["fill"]["suggestions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let args = h.suggested(&login.item).await;
    crate::vault::host::edit(
        &vault,
        &json!({"item": login.item, "revision": login.revision, "values": {"username": "ada2@shop.example"}}),
        &mut Map::new(),
    )
    .unwrap();
    let (stale, _) = h.fill(args).await;
    let stale = stale.unwrap_err();
    println!("changed item: {} ({})", stale.code, stale.message);
    assert_eq!(stale.code, "stale_snapshot");
    assert!(!h.text().await.contains("accepted"));

    let everything = h.shown.join("\n");
    for value in [PASSWORD, NUMBER, CVV, KEY] {
        assert!(
            !everything.contains(value),
            "a value reached a result or the audit"
        );
    }
    h.call("close", json!({})).await.ok();
}

/// A card's confirmation names the exact mapping, the site of the frame its
/// values go into and the press button, and binds them: used once, and
/// refused once the frame or a mapped field is swapped.
async fn a_card_confirmation_binds_its_mapping_and_frames(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    let (pages, processor) = (site(), site());
    let local = format!("http://127.0.0.1:{pages}");
    let frame = format!("http://localhost:{processor}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![
            (local.clone(), "https://shop.example".into()),
            (frame.clone(), "https://js.processor.example".into()),
        ],
    ));
    let mut h = Harness::start(browsers).await;
    let confirm = async |h: &mut Harness, id: &Value, request: &str| {
        let mut audit = Map::new();
        let done = crate::browser::steps::run(
            &h.browsers,
            &h.entry,
            &h.id,
            CALLER,
            None,
            &json!({"request": request, "plan": {"steps": []}, "confirm": id}),
            &mut audit,
        )
        .await;
        h.shown.push(seen(&Value::Object(audit)));
        match &done {
            Ok(value) => h.shown.push(seen(value)),
            Err(failure) => h.shown.push(seen_failure(failure)),
        }
        done
    };

    h.call(
        "open",
        json!({"url": format!("{local}/processor.html?frame={processor}")}),
    )
    .await
    .unwrap();
    let args = h.suggested(&card.item).await;
    let (stop, _) = h.gated_fill(args).await;
    let stop = stop.unwrap_err();
    let issued = stop.details["confirmation"].clone();
    println!(
        "processor frame: {} ({}); confirmation {issued}",
        stop.code, stop.message
    );
    assert_eq!(stop.code, "confirm_needed");
    assert_eq!(issued["frames"], json!(["https://js.processor.example"]));
    assert_eq!(issued["mapping"].as_array().unwrap().len(), 3);
    assert_eq!(issued["target"]["name"], "Pay $10.00");
    assert!(issued["suggestion"].is_string());
    let paid = confirm(&mut h, &issued["id"], "r-frame").await.unwrap();
    println!("confirmed: trail {}", paid["trail"]);
    assert_eq!(paid["trail"][0]["submitted"], true);
    let page = h.text().await;
    println!("the processor answered: {page}");
    assert!(
        page.contains("number ok, expiry ok (04 / 31), cvv ok"),
        "{page}"
    );
    let again = confirm(&mut h, &issued["id"], "r-frame-2")
        .await
        .unwrap_err();
    println!("used again: {} ({})", again.code, again.message);
    assert_eq!(again.code, "confirm_stale");

    // The frame swapped after the confirmation: refused, nothing paid.
    h.call(
        "open",
        json!({"url": format!("{local}/processor.html?frame={processor}")}),
    )
    .await
    .unwrap();
    let args = h.suggested(&card.item).await;
    let (stop, _) = h.gated_fill(args).await;
    let id = stop.unwrap_err().details["confirmation"]["id"].clone();
    h.call("eval", json!({"script": "const f = document.getElementById('pay'); f.src = f.src + '?again=1'; 1"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let swapped = confirm(&mut h, &id, "r-swap").await.unwrap_err();
    println!("frame swapped: {} ({})", swapped.code, swapped.message);
    assert_eq!(swapped.code, "confirm_stale");
    assert!(!h.text().await.contains("payment:"));

    // A mapped field swapped for a copy: refused, nothing paid.
    let checkout = h
        .call("open", json!({"url": format!("{local}/checkout.html")}))
        .await
        .unwrap();
    let (stop, _) = h
        .gated_fill(
            json!({"item": card.item, "press": find(&checkout, "button", "Pay"),
                           "fields": [["number", [find(&checkout, "textbox", "Card number")]],
                                      ["expiry", [find(&checkout, "textbox", "Expiry")]],
                                      ["cvv", [find(&checkout, "textbox", "CVV")]]]}),
        )
        .await;
    let id = stop.unwrap_err().details["confirmation"]["id"].clone();
    h.call("eval", json!({"script": "const e = document.querySelector('[name=cc-number]'); e.replaceWith(e.cloneNode()); 1"}))
        .await
        .unwrap();
    let swapped = confirm(&mut h, &id, "r-field").await.unwrap_err();
    println!("field swapped: {} ({})", swapped.code, swapped.message);
    assert_eq!(swapped.code, "confirm_stale");
    assert!(!h.text().await.contains("payment:"));
    assert!(
        h.call("eval", json!({"script": "1"})).await.is_ok(),
        "the agent is back"
    );

    let everything = h.shown.join("\n");
    for value in [NUMBER, CVV] {
        assert!(
            !everything.contains(value),
            "a value reached a result or the audit"
        );
    }
    h.call("close", json!({})).await.ok();
}

async fn a_filled_value_is_guarded_in_every_tab_and_from_screenshots(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let identity = vault
        .with(|s| {
            s.add(item(
                IDENTITY,
                "Home",
                &[],
                &[("first name", "Ada"), ("city", "Lovelace-on-Thames")],
            ))
        })
        .unwrap();
    let local = format!("http://127.0.0.1:{}", site());
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;
    let address = h
        .call("open", json!({"url": format!("{local}/address.html")}))
        .await
        .unwrap();
    let fields = json!([
        ["first name", [find(&address, "textbox", "First name")]],
        ["city", [find(&address, "textbox", "City")]],
    ]);
    let (filled, _) = h
        .fill(json!({"item": identity.item, "fields": fields}))
        .await;
    assert_eq!(filled.unwrap()["submitted"], false);
    let refused = |result: Result<Value, crate::failure::Failure>| {
        let failure = result.unwrap_err();
        (failure.exit, failure.code)
    };
    let guarded = (crate::failure::Exit::Denied, "secret_on_page".to_string());

    // A picture would show the values.
    assert_eq!(refused(h.call("screenshot", json!({})).await), guarded);
    // Nor does another tab let anything read them: eval there, and back
    // on the filled tab, are refused alike.
    h.call(
        "tab",
        json!({"action": "new", "url": format!("{local}/login.html")}),
    )
    .await
    .unwrap();
    let read = json!({"script": "document.title"});
    assert_eq!(refused(h.call("eval", read.clone()).await), guarded);
    h.call("tab", json!({"action": "switch", "tab": "t1"}))
        .await
        .unwrap();
    let values = json!({"script": "[...document.querySelectorAll('input')].map((i) => i.value)"});
    assert_eq!(refused(h.call("eval", values).await), guarded);
    // Once the filled tab is closed, nothing of it is left to guard.
    h.call("tab", json!({"action": "close"})).await.unwrap();
    let title = h.call("eval", read).await.unwrap();
    assert_eq!(title["value"], "latch login test");
    assert!(h.call("screenshot", json!({})).await.is_ok());
    assert!(!h.shown.join("\n").contains("Lovelace-on-Thames"));
    h.call("close", json!({})).await.unwrap();
}

async fn no_secret_goes_into_a_form_sent_by_get(engine: &str) {
    const MEMO: &str = "made-up memo\nsecond line 5d2e";
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let mut shop = item(
        LOGIN,
        "Shop",
        &["shop.example"],
        &[("username", "ada@shop.example"), ("password", PASSWORD)],
    );
    // A hidden custom field: a secret, with a line break.
    shop.raw.fields.push(crate::vault::item::Custom {
        name: Some("memo".into()),
        value: Some(MEMO.into()),
        kind: 1,
        linked_id: None,
    });
    let login = vault.with(|s| s.add(shop)).unwrap();
    let local = format!("http://127.0.0.1:{}", site());
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;
    let open = async |h: &mut Harness| {
        h.call("open", json!({"url": format!("{local}/get-login.html")}))
            .await
            .unwrap()
    };
    // A form that says GET, one whose button says so, one that says
    // nothing (a GET too), and a secret's line breaks, sent as CRLF.
    let rows: [(&[(&str, &str)], &str); 4] = [
        (
            &[("username", "Email"), ("password", "Password")],
            "Sign in",
        ),
        (
            &[("username", "Work email"), ("password", "Work password")],
            "Sign in at work",
        ),
        (
            &[("username", "Other email"), ("password", "Other password")],
            "Sign in elsewhere",
        ),
        (&[("memo", "Memo")], "Send memo"),
    ];
    for (fields, button) in rows {
        let page = open(&mut h).await;
        let fields: Vec<Value> = fields
            .iter()
            .map(|(label, name)| json!([label, [find(&page, "textbox", name)]]))
            .collect();
        let (refused, _) = h
            .fill(json!({
                "item": login.item,
                "fields": fields,
                "press": find(&page, "button", button),
            }))
            .await;
        let refused = refused.unwrap_err();
        println!("{button}: {} ({})", refused.code, refused.message);
        assert_eq!(
            (refused.exit, refused.code.as_str()),
            (crate::failure::Exit::Denied, "get_form")
        );
        // Nothing was sent: every tab is still on the page, none on the
        // address that would carry a secret, and the agent is back.
        let seen = h.call("observe", json!({})).await.unwrap();
        for tab in seen["tabs"].as_array().unwrap() {
            let url = tab["url"].as_str().unwrap();
            assert!(url.ends_with("/get-login.html"), "{seen}");
        }
    }
    // A username alone is no secret: an email-first sign-in carries it to
    // the next page in the address, and that goes on.
    let page = open(&mut h).await;
    let (went, _) = h
        .fill(json!({
            "item": login.item,
            "fields": [["username", [find(&page, "textbox", "Email")]]],
            "press": find(&page, "button", "Sign in"),
        }))
        .await;
    println!("a username alone: {went:?}");
    if let Err(failure) = &went {
        assert_ne!(failure.code, "get_form");
    }
    let seen = h.call("observe", json!({})).await.unwrap();
    assert!(
        seen["url"].as_str().unwrap().contains("/welcome?"),
        "{seen}"
    );
    let everything = h.shown.join("\n");
    for value in [PASSWORD, "second+line", "5d2e"] {
        assert!(!everything.contains(value), "{value} was shown");
    }
    h.call("close", json!({})).await.unwrap();
}

/// A fill that cannot clean up keeps its secret guarded after its call:
/// the page's own requests still go on, checked between calls, and the
/// guard stands until the session's tabs are closed.
/// A fill that cannot clean up keeps its secret guarded after its call:
/// the page's own requests still go on, checked between calls; and
/// however it ends, its session closed or its host stopped, no page sends
/// the secret it holds as it goes.
async fn requests_go_on_while_a_guard_outlives_its_call(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let card = vault
        .with(|s| {
            s.add(item(
                CARD,
                "Test Visa",
                &[],
                &[
                    ("number", NUMBER),
                    ("expiry month", "4"),
                    ("expiry year", "2031"),
                    ("code", CVV),
                ],
            ))
        })
        .unwrap();
    for host_stops in [false, true] {
        let port = site();
        let local = format!("http://127.0.0.1:{port}");
        let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
        browsers.test = Some((
            latch(),
            vec![(local.clone(), "https://login.shop.example".into())],
        ));
        let mut h = Harness::start(browsers).await;
        let page = h
            .call(
                "open",
                json!({"url": format!("{local}/trap-checkout.html?beacon")}),
            )
            .await
            .unwrap();
        let fields = json!([
            ["number", [find(&page, "textbox", "Card number")]],
            ["expiry", [find(&page, "textbox", "Expiry")]],
            ["cvv", [find(&page, "textbox", "CVV")]],
        ]);
        let started = std::time::Instant::now();
        let (before, _) = beacons(port);
        let (trapped, _) = h
            .fill(
                json!({"item": card.item, "fields": fields, "press": find(&page, "button", "Pay")}),
            )
            .await;
        assert_eq!(trapped.unwrap_err().code, "session_paused");
        if !host_stops {
            assert!(
                started.elapsed() < Duration::from_secs(19),
                "too slow to test"
            );
            // The page reaches /beacon 20 seconds after the number arrived,
            // with no call under way.
            tokio::time::sleep(Duration::from_secs(24).saturating_sub(started.elapsed())).await;
            let (after, _) = beacons(port);
            println!("beacons while the trap held: {}", after - before);
            assert!(after > before, "the page's request waited for a call");
        }
        // The page tries to send the card number away in an address as it
        // goes: still stopped.
        if host_stops {
            h.browsers.close_all().await;
        } else {
            h.call("close", json!({})).await.unwrap();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let how = if host_stops {
            "host stopped"
        } else {
            "session closed"
        };
        assert_eq!(beacons(port).1, 0, "the number left as the {how}");
    }
}

async fn a_login_after_an_edit_fills_the_new_password(engine: &str) {
    const OLD: &str = "made-up-old-q8zr";
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let vault = Vault::new(state.clone());
    let login = vault
        .with(|s| {
            s.add(item(
                LOGIN,
                "Shop",
                &["shop.example"],
                &[("username", "ada@shop.example"), ("password", OLD)],
            ))
        })
        .unwrap();
    let pages = site();
    let local = format!("http://127.0.0.1:{pages}");
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;

    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (before, _) = h.login(&login.item).await;
    assert_eq!(before.unwrap()["submitted"], true);
    let page = h.text().await;
    println!("before the edit: {page}");
    assert!(page.contains("password rejected"));

    let mut audit = Map::new();
    let edited = crate::vault::host::edit(
        &vault,
        &json!({"item": login.item, "revision": login.revision,
                "values": {"password": PASSWORD}}),
        &mut audit,
    )
    .unwrap();
    h.shown.push(seen(&edited));
    h.shown.push(seen(&Value::Object(audit.clone())));
    println!("edit audit: {}", Value::Object(audit));

    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (after, _) = h.login(&login.item).await;
    assert_eq!(after.unwrap()["submitted"], true);
    let page = h.text().await;
    println!("after the edit: {page}");
    assert!(page.contains("password accepted"));
    let everything = h.shown.join("\n");
    assert!(!everything.contains(PASSWORD) && !everything.contains(OLD));
    h.call("close", json!({})).await.ok();
}
