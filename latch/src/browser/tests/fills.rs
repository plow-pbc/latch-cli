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

use super::{CALLER, exec, latch, test_state, test_state_alone};
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
    an_identifier_first_login_fills_both_steps_in_one_pause,
    the_button_that_ends_a_step_is_picked_only_when_one_fits,
    a_fill_in_one_session_pauses_every_session_on_its_browser,
    agent_code_is_noted_per_site_and_kept_with_the_profile,
    a_login_after_an_edit_fills_the_new_password,
);

/// `text` without its sha256 digests (a screenshot's, in a confirmation):
/// 64 hex digits hold the three-digit CVV by chance, and are no value.
fn without_digests(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_hexdigit() {
            run.push(ch);
            continue;
        }
        if run.len() != 64 {
            out.push_str(&run);
        }
        run.clear();
        out.push(ch);
    }
    out.pop();
    out
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
    // The port is in every origin an agent and the audit see; one holding
    // the CVV's digits (`50319`) would read as a leaked value.
    let (listener, port) = loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if !port.to_string().contains(CVV) {
            break (listener, port);
        }
    };
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
        self.shown.push(Value::Object(audit).to_string());
        match result {
            Ok((value, _)) => {
                self.shown.push(value.to_string());
                Ok(value)
            }
            Err(failure) => {
                self.shown.push(format!(
                    "{} {} {}",
                    failure.code, failure.message, failure.details
                ));
                Err(failure)
            }
        }
    }

    async fn fill(
        &mut self,
        command: &str,
        args: Value,
    ) -> (Result<Value, crate::failure::Failure>, Map<String, Value>) {
        let mut audit = Map::new();
        let result =
            crate::browser::fill::run(&self.browsers, &self.entry, command, &args, &mut audit)
                .await;
        self.shown.push(Value::Object(audit.clone()).to_string());
        match &result {
            Ok(value) => self.shown.push(value.to_string()),
            Err(failure) => self.shown.push(format!(
                "{} {} {}",
                failure.code, failure.message, failure.details
            )),
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
            CALLER,
            "fill",
            &args,
            &mut audit,
        )
        .await;
        self.shown.push(Value::Object(audit.clone()).to_string());
        if let Err(failure) = &result {
            self.shown.push(format!(
                "{} {} {}",
                failure.code, failure.message, failure.details
            ));
        }
        (result, audit)
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

    // A login on a subdomain of its site: latch picks the fields itself.
    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (signed_in, audit) = h.fill("login", json!({"item": login.item})).await;
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
    assert_eq!(audit["fields"], json!(["username", "password"]));

    // Refused, with nothing written: a lookalike site, and a sandboxed frame.
    h.call("open", json!({"url": format!("{evil}/login.html")}))
        .await
        .unwrap();
    let (refused, _) = h.fill("login", json!({"item": login.item})).await;
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
    h.call(
        "open",
        json!({"url": format!("{local}/sandboxed-login.html")}),
    )
    .await
    .unwrap();
    let (refused, _) = h.fill("login", json!({"item": login.item})).await;
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
    let (browsers, entry, id) = (h.browsers.clone(), h.entry.clone(), login.item.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result =
            crate::browser::fill::run(&browsers, &entry, "login", &json!({"item": id}), &mut audit)
                .await;
        (result, audit)
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for command in ["observe", "screenshot", "eval"] {
        let paused = h.call(command, json!({"script": "1"})).await.unwrap_err();
        println!("{command} during the fill: {}", paused.code);
        assert_eq!(paused.code, "session_paused");
    }
    let (slow, audit) = filling.await.unwrap();
    h.shown.push(Value::Object(audit.clone()).to_string());
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
        &json!({"request": "r-card", "plan": {"steps": []}, "confirm": issued["id"]}),
        &mut audit,
    )
    .await
    .unwrap();
    h.shown.push(paid.to_string());
    h.shown.push(Value::Object(audit.clone()).to_string());
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
    let after = h.call("observe", json!({})).await.unwrap().to_string();
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
        &json!({"request": "r-reprice", "plan": {"steps": []},
                "confirm": stop.details["confirmation"]["id"]}),
        &mut audit,
    )
    .await
    .unwrap_err();
    h.shown.push(format!(
        "{} {} {}",
        repriced.code, repriced.message, repriced.details
    ));
    h.shown.push(Value::Object(audit.clone()).to_string());
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
        .fill("fill", json!({"item": identity.item, "fields": fields}))
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
    h.call("open", json!({"url": format!("{local}/totp.html")}))
        .await
        .unwrap();
    let (verified, audit) = h.fill("fill_code", json!({"item": login.item})).await;
    assert_eq!(verified.unwrap()["submitted"], true);
    assert_eq!(audit["fields"], json!(["code"]));
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
    let everything = without_digests(&h.shown.join("\n"));
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
        assert!(!ok && text.contains("Operation not permitted"));
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
        assert!(!out.status.success() && stderr.contains("sandbox_denied"));
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

    // Named fields without --submit: latch finds the form's own submit.
    let page = h
        .call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (named, _) = h
        .fill(
            "login",
            json!({
                "item": login.item,
                "username": find(&page, "textbox", "Email"),
                "password": find(&page, "textbox", "Password"),
            }),
        )
        .await;
    let named = named.unwrap();
    println!(
        "named fields, no --submit: submitted {}",
        named["submitted"]
    );
    assert_eq!(named["submitted"], true);
    assert!(h.text().await.contains("password accepted"));

    // No submit at all: refused before any value is released.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/no-submit-login.html")}),
        )
        .await
        .unwrap();
    let (refused, _) = h
        .fill(
            "login",
            json!({
                "item": login.item,
                "username": find(&page, "textbox", "Email"),
                "password": find(&page, "textbox", "Password"),
            }),
        )
        .await;
    let refused = refused.unwrap_err();
    println!("no submit button: {} ({})", refused.code, refused.message);
    assert_eq!(refused.code, "ambiguous_target");
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
        .fill(
            "fill",
            json!({"item": card.item, "fields": fields, "press": find(&page, "button", "Pay")}),
        )
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
            "login",
            json!({"item": login.item, "username": "e99", "password": "e98"}),
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
    let everything = without_digests(&h.shown.join("\n"));
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
        shown_b.push(format!("{} {result:?}", Value::Object(audit)));
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
    let (browsers, entry, id) = (h.browsers.clone(), h.entry.clone(), login.item.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result =
            crate::browser::fill::run(&browsers, &entry, "login", &json!({"item": id}), &mut audit)
                .await;
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
    h.shown.push(Value::Object(audit).to_string());
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
        .fill(
            "fill",
            json!({"item": card.item, "fields": fields, "press": find(&page, "button", "Pay")}),
        )
        .await;
    assert_eq!(trapped.unwrap_err().code, "session_paused");
    let held = call_b("observe", &b, json!({})).await.unwrap_err();
    println!("the other session while the trap holds: {}", held.code);
    assert_eq!(held.code, "session_paused");
    h.call("close", json!({})).await.unwrap();
    let seen = call_b("observe", &b, json!({})).await.unwrap();
    println!("the other session once it is closed: {}", seen["title"]);
    assert_eq!(seen["title"], "latch next page");

    call_b("close", &b, json!({})).await.unwrap();

    let everything = without_digests(&format!("{}\n{}", h.shown.join("\n"), shown_b.join("\n")));
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
        let (signed_in, audit) = h.fill("login", json!({"item": login.item})).await;
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
        "portal page embedding the login frame: vault {}",
        observed["vault"]
    );
    let listed: Vec<&str> = observed["vault"]
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

async fn an_identifier_first_login_fills_both_steps_in_one_pause(engine: &str) {
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

    // One document, no form, no submit button, "Continue with Google"
    // first, the password shown only after Continue, and cross-site frames
    // coming and going throughout.
    h.call(
        "open",
        json!({"url": format!("{local}/id-first.html?cross={provider}")}),
    )
    .await
    .unwrap();
    let (signed_in, audit) = h.fill("login", json!({"item": login.item})).await;
    println!(
        "same document: {:?}; audit {}",
        signed_in.as_ref().map(|r| (&r["filled"], &r["submitted"])),
        Value::Object(audit.clone())
    );
    let signed_in = signed_in.unwrap();
    assert_eq!(signed_in["filled"], json!(["username", "password"]));
    assert!(h.text().await.contains("password accepted"));
    assert_eq!(
        audit["password_step"]["fields"],
        json!(["username", "password"])
    );

    // Two pages, the second on another site: a cross-process navigation
    // between the steps, with the agent kept out throughout.
    h.call(
        "open",
        json!({"url": format!("{local}/id-first-email.html?next={provider}")}),
    )
    .await
    .unwrap();
    let (browsers, entry, id) = (h.browsers.clone(), h.entry.clone(), login.item.clone());
    let filling = tokio::spawn(async move {
        let mut audit = Map::new();
        let result =
            crate::browser::fill::run(&browsers, &entry, "login", &json!({"item": id}), &mut audit)
                .await;
        (result, audit)
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let paused = h.call("observe", json!({})).await.unwrap_err();
    println!("observe between the steps: {}", paused.code);
    assert_eq!(paused.code, "session_paused");
    let (signed_in, audit) = filling.await.unwrap();
    h.shown.push(Value::Object(audit.clone()).to_string());
    println!(
        "two pages: {:?}; audit {}",
        signed_in.as_ref().map(|r| (&r["filled"], &r["submitted"])),
        Value::Object(audit.clone())
    );
    let signed_in = signed_in.unwrap();
    h.shown.push(signed_in.to_string());
    assert_eq!(signed_in["filled"], json!(["username", "password"]));
    assert!(h.text().await.contains("password accepted"));
    assert_eq!(audit["top_origin"], json!(local));
    assert_eq!(audit["password_step"]["top_origin"], json!(provider_origin));

    // The same, with the second page on a lookalike: nothing goes there,
    // and the agent is let back in.
    h.call(
        "open",
        json!({"url": format!("{local}/id-first-email.html?next={lookalike}")}),
    )
    .await
    .unwrap();
    let (refused, _) = h.fill("login", json!({"item": login.item})).await;
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

    let everything = h.shown.join("\n");
    assert!(
        !everything.contains(PASSWORD),
        "the password reached a result or the audit"
    );
    h.call("close", json!({})).await.ok();
}

async fn the_button_that_ends_a_step_is_picked_only_when_one_fits(engine: &str) {
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
    let mut browsers = Browsers::new(state.clone(), vault.clone(), super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://login.shop.example".into())],
    ));
    let mut h = Harness::start(browsers).await;

    // "Sign in via LinkedIn" comes first, but only "Continue" is named
    // plainly for the step's end.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/login-choices.html")}),
        )
        .await
        .unwrap();
    let (signed_in, audit) = h.fill("login", json!({"item": login.item})).await;
    println!(
        "another way in first: {:?}; pressed {} then {}",
        signed_in.as_ref().map(|r| &r["filled"]),
        audit["pressed"],
        audit["password_step"]["pressed"]
    );
    assert_eq!(audit["pressed"], json!(find(&page, "button", "Continue")));
    assert_eq!(
        signed_in.unwrap()["filled"],
        json!(["username", "password"])
    );
    assert!(h.text().await.contains("password accepted"));

    // A form whose own submit button is another way in ("Sign in · Yahoo"):
    // it does not count, whatever its type; the plain "Continue" does.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/login-form-choices.html")}),
        )
        .await
        .unwrap();
    let (signed_in, audit) = h.fill("login", json!({"item": login.item})).await;
    println!(
        "the form's submit is another way in: {:?}; pressed {} then {}",
        signed_in.as_ref().map(|r| &r["filled"]),
        audit["pressed"],
        audit["password_step"]["pressed"]
    );
    assert_eq!(audit["pressed"], json!(find(&page, "button", "Continue")));
    assert_eq!(
        signed_in.unwrap()["filled"],
        json!(["username", "password"])
    );
    assert!(h.text().await.contains("password accepted"));

    // Two plain buttons ("Continue", "Next"): latch cannot tell, names both,
    // and releases nothing.
    let page = h
        .call(
            "open",
            json!({"url": format!("{local}/login-choices.html?two=1")}),
        )
        .await
        .unwrap();
    let (refused, _) = h.fill("login", json!({"item": login.item})).await;
    let refused = refused.unwrap_err();
    println!(
        "two plain buttons: {} ({}) {}",
        refused.code, refused.message, refused.details
    );
    assert_eq!(refused.code, "ambiguous_target");
    assert_eq!(
        refused.details["candidates"],
        json!([
            find(&page, "button", "Continue"),
            find(&page, "button", "Next")
        ])
    );
    assert_eq!(refused.details["snapshot"], page["snapshot"]);
    assert!(
        h.call("eval", json!({"script": "1"})).await.is_ok(),
        "nothing was left on the page, and the agent is back"
    );
    assert!(h.text().await.contains("no other way in taken"));
    let everything = h.shown.join("\n");
    assert!(!everything.contains(PASSWORD));
    h.call("close", json!({})).await.ok();
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
    let (before, _) = h.fill("login", json!({"item": login.item})).await;
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
    h.shown.push(edited.to_string());
    h.shown.push(Value::Object(audit.clone()).to_string());
    println!("edit audit: {}", Value::Object(audit));

    h.call("open", json!({"url": format!("{local}/login.html")}))
        .await
        .unwrap();
    let (after, _) = h.fill("login", json!({"item": login.item})).await;
    assert_eq!(after.unwrap()["submitted"], true);
    let page = h.text().await;
    println!("after the edit: {page}");
    assert!(page.contains("password accepted"));
    let everything = h.shown.join("\n");
    assert!(!everything.contains(PASSWORD) && !everything.contains(OLD));
    h.call("close", json!({})).await.ok();
}
