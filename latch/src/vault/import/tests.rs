//! The Latch app's import tests, ported: CSV quirks, each format's rules,
//! 1PUX's bounds, the preview never carrying a value, and re-imports that
//! neither double the vault nor guess which password to write over.

use std::io::Write;

use super::*;
use crate::vault::store::OwnStore;

const TOTP_KEY: &str = "JBSWY3DPEHPK3PXP";

fn parse_text(text: &str) -> Result<Parsed, Failure> {
    parse(text.as_bytes())
}

fn login_of(row: &Imported) -> Login {
    row.item.raw.login.clone().unwrap_or_default()
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/import/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn csv_rows_read_rfc_4180() {
    let text = "\u{feff}a,b,c\r\n\"1,1\",\"say \"\"hi\"\"\",\"line\nbreak\"\r\n";
    assert_eq!(
        csv_rows(text).unwrap(),
        [
            vec!["a", "b", "c"],
            vec!["1,1", "say \"hi\"", "line\nbreak"]
        ]
    );
    assert_eq!(
        csv_rows("a,b\n\n1,2\n\n").unwrap(),
        [vec!["a", "b"], vec!["1", "2"]]
    );
}

const APPLE: &str = "Title,URL,Username,Password,Notes,OTPAuth\n\
    Notion,https://notion.so,jon@example.com,\"pw,with\"\"quote\",a note,\n\
    Dropbox,https://dropbox.com,so@example.com,drop-secret,,otpauth://totp/Dropbox:so%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Dropbox";

#[test]
fn apple_passwords_csv() {
    let parsed = parse_text(APPLE).unwrap();
    assert_eq!(parsed.source, "Apple Passwords");
    assert!(parsed.skipped.is_empty());
    let [notion, dropbox] = parsed.items.as_slice() else {
        panic!("two rows")
    };
    assert_eq!(notion.item.raw.name, "Notion");
    assert_eq!(notion.item.sites, ["notion.so"]);
    assert_eq!(login_of(notion).password.as_deref(), Some("pw,with\"quote"));
    assert_eq!(notion.item.raw.notes.as_deref(), Some("a note"));
    assert!(notion.warnings.is_empty());
    assert!(login_of(dropbox).totp.unwrap().contains(TOTP_KEY));

    let no_site =
        parse_text("Title,URL,Username,Password,Notes,OTPAuth\nWi-Fi,,'',hunter2,,").unwrap();
    assert!(no_site.items.is_empty());
    assert!(no_site.skipped[0].reason.contains("no website address"));
    assert!(!no_site.skipped[0].reason.contains("hunter2"));

    let bare = parse_text("Title,URL,Username,Password,Notes,OTPAuth\nOld site,example.com,jon,,,")
        .unwrap();
    assert_eq!(bare.items[0].url, "https://example.com");
    assert!(bare.items[0].warnings.join(" ").contains("no password"));

    let bad_key =
        parse_text("Title,URL,Username,Password,Notes,OTPAuth\nX,https://x.example,jon,pw,,123456")
            .unwrap();
    assert_eq!(login_of(&bad_key.items[0]).totp, None);
    let said = bad_key.items[0].warnings.join(" ");
    assert!(said.contains("one-time password key") && !said.contains("123456"));

    let untitled = parse_text(
        "Title,URL,Username,Password,Notes,OTPAuth\n,https://pizza.example/login,jon,pw,,",
    )
    .unwrap();
    assert_eq!(untitled.items[0].item.raw.name, "pizza.example");
}

#[test]
fn onepassword_csv_skips_archived_rows() {
    let text = format!(
        "Title,Url,Username,Password,OTPAuth,Favorite,Archived,Tags,Notes\n\
         GitHub,https://github.com,jon,gh-secret,{TOTP_KEY},true,false,work,ssh note\n\
         Dead account,https://dead.example,jon,old-secret,,false,true,,"
    );
    let parsed = parse_text(&text).unwrap();
    assert_eq!(parsed.source, "1Password");
    assert_eq!(parsed.items.len(), 1);
    let github = &parsed.items[0];
    assert_eq!(
        (
            github.item.raw.name.as_str(),
            login_of(github).totp.as_deref()
        ),
        ("GitHub", Some(TOTP_KEY))
    );
    assert_eq!(
        parsed.skipped,
        [Skipped {
            title: "Dead account".into(),
            reason: "archived in 1Password".into(),
            vault: None
        }]
    );
}

#[test]
fn chrome_csv_and_any_header_order() {
    let parsed = parse_text("name,url,username,password,note\ngoogle.com,https://accounts.google.com,jon,g-secret,work account").unwrap();
    assert_eq!(parsed.source, "Chrome");
    let row = &parsed.items[0];
    assert_eq!(row.item.raw.name, "google.com");
    assert_eq!(row.item.sites, ["google.com"]);
    assert_eq!(row.url, "https://accounts.google.com");
    assert_eq!(row.item.raw.notes.as_deref(), Some("work account"));

    let any = parse_text("Password,Username,Title,URL\npw,jon,Site,https://s.example").unwrap();
    assert_eq!(any.source, "CSV");
    assert_eq!(login_of(&any.items[0]).username.as_deref(), Some("jon"));
    let refused = parse_text("name,email\njon,jon@example.com").err().unwrap();
    assert!(
        refused
            .message
            .contains("Title, URL, Username and Password")
    );
}

#[test]
fn the_chrome_fixture_reads_with_its_siteless_row_set_aside() {
    let parsed = parse(&fixture("chrome.csv")).unwrap();
    assert_eq!(parsed.source, "Chrome");
    let titles: Vec<&str> = parsed
        .items
        .iter()
        .map(|r| r.item.raw.name.as_str())
        .collect();
    assert_eq!(titles, ["quotes.toscrape.com", "example login"]);
    assert_eq!(parsed.items[0].item.sites, ["toscrape.com"]);
    assert_eq!(parsed.skipped[0].title, "no site");
}

fn onepassword_item(overrides: Value) -> String {
    let mut item = json!({
        "overview": {
            "title": "Dropbox",
            "url": "https://www.dropbox.com",
            "URLs": [{"u": "https://www.dropbox.com", "l": "website", "m": "default"}],
        },
        "details": {
            "fields": [
                {"value": "drop-secret", "name": "password", "type": "P", "designation": "password"},
                {"value": "so@example.com", "name": "username", "type": "T", "designation": "username"},
            ],
            "sections": [{"name": "add more", "fields": [
                {"t": "one-time password", "n": "TOTP_varaero3oygcxrfk5bdco4kqjm", "k": "concealed", "v": TOTP_KEY}
            ]}],
        },
        "templateUuid": "001",
        "uuid": "rit7w7cpja6k6uzbupcokein3a",
    });
    for (k, v) in overrides.as_object().unwrap() {
        item[k] = v.clone();
    }
    item.to_string()
}

#[test]
fn onepassword_item_json() {
    let parsed = parse_text(&onepassword_item(json!({}))).unwrap();
    assert_eq!(parsed.source, "1Password item");
    let row = &parsed.items[0];
    assert_eq!(login_of(row).username.as_deref(), Some("so@example.com"));
    assert_eq!(login_of(row).totp.as_deref(), Some(TOTP_KEY));
    assert!(row.warnings.is_empty());

    let card = parse_text(&onepassword_item(json!({"templateUuid": "002"})))
        .err()
        .unwrap();
    assert!(card.message.contains("different type"));
    let mut no_password: Value = serde_json::from_str(&onepassword_item(json!({}))).unwrap();
    no_password["details"]["fields"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let warned = parse_text(&no_password.to_string()).unwrap();
    assert!(warned.items[0].warnings.join(" ").contains("no password"));
    let mut empty: Value = serde_json::from_str(&onepassword_item(json!({}))).unwrap();
    empty["details"]["fields"] = json!([]);
    assert!(
        parse_text(&empty.to_string())
            .err()
            .unwrap()
            .message
            .contains("does not look like a login")
    );
    assert!(
        parse_text("{not json")
            .err()
            .unwrap()
            .message
            .contains("Copy item JSON")
    );
    let mut siteless: Value = serde_json::from_str(&onepassword_item(json!({}))).unwrap();
    siteless["overview"] = json!({"title": "Dropbox", "URLs": []});
    let set_aside = parse_text(&siteless.to_string()).unwrap();
    assert!(
        set_aside.items.is_empty() && set_aside.skipped[0].reason.contains("no website address")
    );
}

#[test]
fn a_preview_never_carries_a_value() {
    let parsed = parse_text(&onepassword_item(json!({}))).unwrap();
    let shown = preview(&parsed);
    let text = shown.to_string();
    assert!(!text.contains("drop-secret") && !text.contains(TOTP_KEY));
    assert_eq!(shown["items"][0]["has_password"], true);
    assert_eq!(shown["items"][0]["has_totp"], true);
    assert_eq!(shown["items"][0]["duplicate"], false);
    assert_eq!(shown["items"][0]["pick"], 1);
}

/// A 1PUX as 1Password writes it; `stored` leaves export.data uncompressed.
fn onepux(vaults: Value, stored: bool) -> Vec<u8> {
    let data = json!({"accounts": [{"attrs": {"name": "Sam"}, "vaults": vaults}]}).to_string();
    zip_of(
        &[
            ("export.attributes", b"{\"version\":3}".to_vec()),
            ("export.data", data.into_bytes()),
        ],
        stored,
    )
}

fn zip_of(entries: &[(&str, Vec<u8>)], stored: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let method = if stored {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true);
    for (name, bytes) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn vault(name: &str, items: Value) -> Value {
    json!({"attrs": {"uuid": format!("v-{name}"), "name": name, "type": "P"}, "items": items})
}

fn pux_login(title: &str) -> Value {
    let lower = title.to_lowercase();
    json!({
        "uuid": lower, "categoryUuid": "001", "state": "active",
        "overview": {"title": title, "url": format!("https://{lower}.com"), "urls": [{"label": "website", "url": format!("https://{lower}.com")}]},
        "details": {"loginFields": [
            {"designation": "username", "value": format!("{lower}-user")},
            {"designation": "password", "value": format!("{title}-secret")}
        ], "notesPlain": "", "sections": []},
    })
}

#[test]
fn onepux_reads_logins_from_every_vault_and_sets_others_aside() {
    let mut dropbox = pux_login("Dropbox");
    dropbox["details"]["sections"] = json!([{"name": "s1", "fields": [
        {"title": "one-time password", "id": "TOTP_1", "value": {"totp": format!("otpauth://totp/D?secret={TOTP_KEY}")}}
    ]}]);
    let parsed = parse(&onepux(
        json!([
            vault("Personal", json!([dropbox])),
            vault("Family", json!([pux_login("Netflix")]))
        ]),
        false,
    ))
    .unwrap();
    assert_eq!(parsed.source, "1Password");
    let named: Vec<(&str, &str)> = parsed
        .items
        .iter()
        .map(|r| {
            (
                r.item.raw.name.as_str(),
                r.vault.as_ref().unwrap().name.as_str(),
            )
        })
        .collect();
    assert_eq!(named, [("Dropbox", "Personal"), ("Netflix", "Family")]);
    assert!(login_of(&parsed.items[0]).totp.unwrap().contains(TOTP_KEY));
    assert_eq!(
        preview(&parsed)["vaults"],
        json!([
            {"id": "v-Personal", "name": "Personal", "items": 1},
            {"id": "v-Family", "name": "Family", "items": 1}
        ])
    );

    let mut github = pux_login("GitHub");
    github["details"]["sections"] = json!([{"name": "s", "fields": [
        {"title": "2FA", "id": "TOTP_abc", "value": {"concealed": format!("otpauth://totp/G?secret={TOTP_KEY}")}}
    ]}]);
    let stored = parse(&onepux(json!([vault("Personal", json!([github]))]), true)).unwrap();
    assert!(login_of(&stored.items[0]).totp.unwrap().contains(TOTP_KEY));

    let mut mixed = pux_login("Mixed");
    mixed["overview"]["urls"] = json!([{"url": "https://good.example.com"}, {"url": "not a url"}]);
    let mut old = pux_login("Old");
    old["state"] = json!("archived");
    let mut router = pux_login("Router");
    router["overview"] = json!({"title": "Router"});
    let parsed = parse(&onepux(json!([vault("Personal", json!([
        old,
        {"uuid": "c", "categoryUuid": "002", "state": "active", "overview": {"title": "Visa"}, "details": {}},
        {"uuid": "k", "categoryUuid": "114", "state": "active", "overview": {"title": "deploy key"}, "details": {}},
        router, mixed, pux_login("Live"),
    ]))]), false)).unwrap();
    let titles: Vec<&str> = parsed
        .items
        .iter()
        .map(|r| r.item.raw.name.as_str())
        .collect();
    assert_eq!(titles, ["Visa", "Mixed", "Live"]);
    assert!(
        parsed.items[1].warnings.contains(
            &"one of its website addresses could not be read and was left off".to_string()
        )
    );
    let reasons: Vec<(&str, &str)> = parsed
        .skipped
        .iter()
        .map(|s| (s.title.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        reasons,
        [
            ("Old", "archived in 1Password"),
            ("deploy key", "an SSH key, not a login"),
            (
                "Router",
                "has no website address; add it by hand as a new item if you still use it"
            ),
        ]
    );
}

/// A 1PUX credit card, its fields as 1Password writes them.
fn pux_card(title: &str) -> Value {
    json!({
        "uuid": title.to_lowercase(), "categoryUuid": "002", "state": "active",
        "overview": {"title": title},
        "details": {"loginFields": [], "notesPlain": "company card", "sections": [{"title": "", "name": "", "fields": [
            {"title": "cardholder name", "id": "cardholder", "value": {"string": "Ada Lovelace"}},
            {"title": "type", "id": "type", "value": {"creditCardType": "mc"}},
            {"title": "number", "id": "ccnum", "value": {"creditCardNumber": "5555 5555 5555 4444"}},
            {"title": "verification number", "id": "cvv", "value": {"concealed": "737"}},
            {"title": "expiry date", "id": "expiry", "value": {"monthYear": 203104}},
            {"title": "valid from", "id": "validFrom", "value": {"monthYear": 202304}}
        ]}]},
    })
}

#[test]
fn onepux_credit_cards_come_along_as_cards() {
    let mut old = pux_card("Old card");
    old["state"] = json!("archived");
    let mut odd = pux_card("Odd expiry");
    odd["details"]["sections"][0]["fields"][4]["value"] = json!({"monthYear": 203113});
    let mut parsed = parse(&onepux(
        json!([vault(
            "Work",
            json!([pux_card("Plow card"), old, odd, pux_login("Shop")])
        )]),
        false,
    ))
    .unwrap();
    let rows: Vec<(&str, &str)> = parsed
        .items
        .iter()
        .map(|r| {
            (
                r.item.raw.name.as_str(),
                crate::vault::item::kind_name(r.item.raw.kind),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("Plow card", "card"),
            ("Odd expiry", "card"),
            ("Shop", "login")
        ]
    );
    assert_eq!(parsed.skipped[0].title, "Old card");
    let card = &parsed.items[0].item.raw;
    for (label, value) in [
        ("cardholder name", "Ada Lovelace"),
        ("brand", "Mastercard"),
        ("number", "5555555555554444"),
        ("code", "737"),
        ("expiry month", "4"),
        ("expiry year", "2031"),
    ] {
        assert_eq!(card.get(label).as_deref(), Some(value), "{label}");
    }
    assert_eq!(card.notes.as_deref(), Some("company card"));
    assert!(parsed.items[0].warnings.is_empty());
    assert_eq!(
        parsed.items[1].warnings,
        ["its expiry date could not be read and was left off"]
    );
    let shown = preview(&parsed);
    assert_eq!(shown["items"][0]["kind"], "card");
    let text = shown.to_string();
    assert!(
        !text.contains("5555555555554444") && !text.contains("737"),
        "{text}"
    );

    // Saved, it is a card the vault keeps its number and code secret in.
    let (_dir, mut vault) = store();
    mark_against(&vault, &mut parsed).unwrap();
    let outcome = commit(&mut vault, &parsed, &[1]);
    assert_eq!(outcome.saved, 1, "{:?}", outcome.failed);
    let meta = &vault.list()[0];
    assert_eq!((meta.kind, meta.title.as_str()), ("card", "Plow card"));
    let secret: Vec<&str> = meta
        .fields
        .iter()
        .filter(|f| f.hidden)
        .map(|f| f.label.as_str())
        .collect();
    assert_eq!(secret, ["number", "code"]);
    assert_eq!(
        vault.release(&meta.item, "number").unwrap(),
        "5555555555554444"
    );
}

#[test]
fn a_onepassword_csv_card_row_says_to_export_a_1pux() {
    let parsed = parse_text(
        "Title,Url,Username,Password,OTPAuth,Favorite,Archived,Tags,Notes
         Plow card,,,,,false,false,,company card
",
    )
    .unwrap();
    assert!(parsed.items.is_empty());
    assert_eq!(parsed.skipped[0].title, "Plow card");
    assert!(
        parsed.skipped[0].reason.contains("export a 1PUX"),
        "{:?}",
        parsed.skipped
    );
}

#[test]
fn onepux_refuses_what_it_is_not_and_what_its_size_does_not_bound() {
    assert!(
        parse(&zip_of(&[("readme.txt", b"hi".to_vec())], false))
            .err()
            .unwrap()
            .message
            .contains("1PUX")
    );
    assert!(
        parse(&zip_of(&[("export.data", b"{\"nope\":1}".to_vec())], true))
            .err()
            .unwrap()
            .message
            .contains("1PUX")
    );
    let huge = zip_of(&[("export.data", vec![b' '; 65 * 1024 * 1024])], false);
    assert!(parse(&huge).err().unwrap().message.contains("too large"));
    // Attachments far past the cap do not stop the items beside them.
    let data = json!({"accounts": [{"vaults": [vault("Personal", json!([pux_login("Acme")]))]}]})
        .to_string();
    let big = zip_of(
        &[
            ("export.data", data.into_bytes()),
            ("files/scan.pdf", vec![0; 65 * 1024 * 1024]),
        ],
        true,
    );
    assert_eq!(parse(&big).unwrap().items[0].item.raw.name, "Acme");
}

#[test]
fn the_onepux_fixture_previews_with_its_vaults_and_no_value() {
    let parsed = parse(&fixture("onepassword.1pux")).unwrap();
    let shown = preview(&parsed);
    let text = shown.to_string();
    for value in ["made-up-pass-1pux-1", "made-up-pass-1pux-3", TOTP_KEY] {
        assert!(!text.contains(value), "{value}");
    }
    let titles: Vec<&str> = parsed
        .items
        .iter()
        .map(|r| r.item.raw.name.as_str())
        .collect();
    assert_eq!(titles, ["Practice login", "Made-up Visa", "Shared wiki"]);
    assert_eq!(parsed.skipped.len(), 1);
    assert_eq!(shown["items"][0]["has_totp"], true);
}

#[test]
fn bitwarden_json_brings_every_kind_it_keeps() {
    let text = json!({"encrypted": false, "items": [
        {"type": 1, "name": "Mail", "login": {"username": "jon", "password": "mail-secret", "uris": [{"uri": "https://mail.example.com"}]}},
        {"type": 3, "name": "Visa", "card": {"cardholderName": "Jon", "number": "4111111111111111", "expMonth": "4", "expYear": "2031", "code": "737"}},
        {"type": 4, "name": "Home", "identity": {"firstName": "Jon", "address1": "1 Elm St"}},
        {"type": 2, "name": "Wi-Fi", "notes": "the code"},
        {"type": 5, "name": "deploy key", "sshKey": {"privateKey": "k"}},
    ]})
    .to_string();
    let parsed = parse_text(&text).unwrap();
    assert_eq!(parsed.source, "Bitwarden");
    let kinds: Vec<&str> = parsed
        .items
        .iter()
        .map(|r| crate::vault::item::kind_name(r.item.raw.kind))
        .collect();
    assert_eq!(kinds, ["login", "card", "identity", "note"]);
    assert_eq!(parsed.skipped[0].title, "deploy key");
    assert!(!preview(&parsed).to_string().contains("4111111111111111"));
    let encrypted = parse_text("{\"encrypted\": true, \"items\": []}")
        .err()
        .unwrap();
    assert!(encrypted.message.contains("unencrypted"));
}

fn store() -> (tempfile::TempDir, OwnStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = OwnStore::open(dir.path()).unwrap();
    (dir, store)
}

/// Parse, mark against the vault, and commit every row.
fn run(vault: &mut OwnStore, text: &str) -> (Parsed, Outcome) {
    let mut parsed = parse_text(text).unwrap();
    mark_against(vault, &mut parsed).unwrap();
    let picks: Vec<usize> = (1..=parsed.items.len()).collect();
    let outcome = commit(vault, &parsed, &picks);
    (parsed, outcome)
}

fn passwords(vault: &OwnStore) -> Vec<String> {
    let mut all: Vec<String> = vault
        .list()
        .iter()
        .map(|m| vault.release(&m.item, "password").unwrap())
        .collect();
    all.sort();
    all
}

#[test]
fn imports_land_and_reimports_double_nothing() {
    let (_dir, mut vault) = store();
    let (_, outcome) = run(&mut vault, APPLE);
    assert_eq!(
        outcome,
        Outcome {
            saved: 2,
            ..Outcome::default()
        }
    );
    let dropbox = vault
        .list()
        .into_iter()
        .find(|m| m.title == "Dropbox")
        .unwrap();
    assert_eq!(
        vault.release(&dropbox.item, "password").unwrap(),
        "drop-secret"
    );
    let (parsed, again) = run(&mut vault, APPLE);
    assert!(parsed.items.iter().all(|r| r.duplicate));
    assert_eq!(
        again,
        Outcome {
            duplicates: 2,
            ..Outcome::default()
        }
    );
    assert_eq!(vault.list().len(), 2);
}

#[test]
fn a_changed_password_or_key_updates_that_item_only() {
    let (_dir, mut vault) = store();
    run(&mut vault, APPLE);
    let (parsed, outcome) = run(&mut vault, &APPLE.replace("drop-secret", "drop-rotated"));
    let dropbox = parsed
        .items
        .iter()
        .find(|r| r.item.raw.name == "Dropbox")
        .unwrap();
    assert!(
        dropbox
            .update
            .as_ref()
            .is_some_and(|u| u.password && !u.totp)
    );
    assert_eq!(
        outcome,
        Outcome {
            updated: 1,
            duplicates: 1,
            ..Outcome::default()
        }
    );
    assert_eq!(vault.list().len(), 2);
    let stored = vault
        .list()
        .into_iter()
        .find(|m| m.title == "Dropbox")
        .unwrap();
    assert_eq!(stored.username.as_deref(), Some("so@example.com"));
    assert_eq!(
        vault.release(&stored.item, "password").unwrap(),
        "drop-rotated"
    );

    // One key in two spellings is no change; a different key updates only it.
    let respelled = APPLE
        .replace(
            &format!("otpauth://totp/Dropbox:so%40example.com?secret={TOTP_KEY}&issuer=Dropbox"),
            TOTP_KEY,
        )
        .replace("drop-secret", "drop-rotated");
    let (parsed, _) = run(&mut vault, &respelled);
    assert!(parsed.items.iter().all(|r| r.duplicate));
    let rotated = APPLE
        .replace(&format!("secret={TOTP_KEY}"), "secret=KRSXG5CTMVRXEZLU")
        .replace("drop-secret", "drop-rotated");
    let (parsed, outcome) = run(&mut vault, &rotated);
    let dropbox = parsed
        .items
        .iter()
        .find(|r| r.item.raw.name == "Dropbox")
        .unwrap();
    assert!(
        dropbox
            .update
            .as_ref()
            .is_some_and(|u| u.totp && !u.password)
    );
    assert_eq!(outcome.updated, 1);
    assert!(
        vault
            .release(&stored.item, "totp")
            .unwrap()
            .contains("KRSXG5CTMVRXEZLU")
    );

    // A source without a key does not read as removing the stored one.
    let (parsed, _) = run(
        &mut vault,
        "name,url,username,password,note\nDropbox,https://dropbox.com,so@example.com,drop-rotated,",
    );
    assert!(parsed.items[0].duplicate);
    assert!(vault.release(&stored.item, "totp").is_ok());
}

#[test]
fn an_update_read_before_a_hand_edit_is_refused() {
    let (_dir, mut vault) = store();
    run(&mut vault, APPLE);
    let mut parsed = parse_text(&APPLE.replace("drop-secret", "drop-rotated")).unwrap();
    mark_against(&vault, &mut parsed).unwrap();
    let stored = vault
        .list()
        .into_iter()
        .find(|m| m.title == "Dropbox")
        .unwrap();
    let mut edited = vault.item(&stored.item).unwrap();
    edited.raw.login.as_mut().unwrap().password = Some("typed-by-hand".into());
    vault.update(edited, stored.revision).unwrap();
    let outcome = commit(&mut vault, &parsed, &[1, 2]);
    assert_eq!(outcome.updated, 0);
    assert!(outcome.failed[0].reason.contains("changed somewhere else"));
    assert_eq!(
        vault.release(&stored.item, "password").unwrap(),
        "typed-by-hand"
    );
}

/// A pick given the preview's `expect` reads the vault again in the same
/// transaction as its save: an edit made since the preview, even after
/// this run read the export, refuses the whole import with nothing saved.
#[test]
fn a_pick_expecting_the_preview_refuses_a_vault_changed_since() {
    let (_dir, mut vault) = store();
    run(&mut vault, APPLE);
    let export = format!(
        "{}\nFresh,https://fresh.example,jo,fresh-secret,,",
        APPLE.replace("drop-secret", "drop-rotated")
    );
    let mut parsed = parse_text(&export).unwrap();
    mark_against(&vault, &mut parsed).unwrap();
    let previewed = preview(&parsed)["expect"].as_str().unwrap().to_string();
    assert_eq!(previewed.len(), 64);

    // Another device edits the item row 2 would update.
    let stored = vault
        .list()
        .into_iter()
        .find(|m| m.title == "Dropbox")
        .unwrap();
    let mut edited = vault.item(&stored.item).unwrap();
    edited.raw.login.as_mut().unwrap().password = Some("typed-by-hand".into());
    vault.update(edited, stored.revision).unwrap();

    let refused = commit_expecting(&mut vault, &mut parsed, &[1, 2, 3], &previewed).unwrap_err();
    assert_eq!(
        (refused.exit, refused.code.as_str()),
        (crate::failure::Exit::Denied, "stale_revision")
    );
    assert_ne!(refused.details["expect"], json!(previewed));
    assert!(!format!("{refused:?}").contains("drop-rotated"));
    assert_eq!(vault.list().len(), 2, "not even the new row is saved");
    assert_eq!(
        vault.release(&stored.item, "password").unwrap(),
        "typed-by-hand"
    );

    // With the token it gave, the pick goes ahead; an item no row touches
    // doesn't change it.
    let again = refused.details["expect"].as_str().unwrap().to_string();
    run(
        &mut vault,
        "Title,URL,Username,Password,Notes,OTPAuth\nOther,https://other.example,jo,other-secret,,",
    );
    let outcome = commit_expecting(&mut vault, &mut parsed, &[1, 2, 3], &again).unwrap();
    assert_eq!(
        outcome,
        Outcome {
            saved: 1,
            updated: 1,
            duplicates: 1,
            ..Outcome::default()
        }
    );
}

const TWINS: &str = "Title,URL,Username,Password,Notes,OTPAuth\n\
    Mail,https://mail.example,jon,first-secret,,\n\
    Mail,https://mail.example,jon,second-secret,,";

#[test]
fn twins_never_write_over_a_guessed_password() {
    let (_dir, mut vault) = store();
    run(&mut vault, TWINS);
    let (_, again) = run(&mut vault, TWINS);
    assert_eq!(
        again,
        Outcome {
            duplicates: 2,
            ..Outcome::default()
        }
    );
    assert_eq!(passwords(&vault), ["first-secret", "second-secret"]);

    let (parsed, third) = run(
        &mut vault,
        "Title,URL,Username,Password,Notes,OTPAuth\nMail,https://mail.example,jon,third-secret,,",
    );
    assert_eq!(
        third,
        Outcome {
            duplicates: 1,
            ..Outcome::default()
        }
    );
    assert!(
        parsed.items[0]
            .warnings
            .join(" ")
            .contains("left alone rather than guess")
    );
    assert_eq!(passwords(&vault), ["first-secret", "second-secret"]);

    let (parsed, rotated) = run(
        &mut vault,
        &TWINS.replace("second-secret", "rotated-secret"),
    );
    assert_eq!(
        rotated,
        Outcome {
            updated: 1,
            duplicates: 1,
            ..Outcome::default()
        }
    );
    assert!(parsed.items[1].update.as_ref().is_some_and(|u| u.password));
    assert_eq!(passwords(&vault), ["first-secret", "rotated-secret"]);
}

#[test]
fn twin_rows_over_one_item_update_nothing_and_extra_twins_are_new() {
    let (_dir, mut vault) = store();
    run(
        &mut vault,
        "Title,URL,Username,Password,Notes,OTPAuth\nMail,https://mail.example,jon,old-secret,,",
    );
    let (parsed, outcome) = run(&mut vault, TWINS);
    assert_eq!(
        outcome,
        Outcome {
            duplicates: 2,
            ..Outcome::default()
        }
    );
    assert!(parsed.items.iter().all(|r| {
        r.warnings
            .join(" ")
            .contains("left alone rather than guess")
    }));
    assert_eq!(passwords(&vault), ["old-secret"]);

    let (_dir, mut fresh) = store();
    run(
        &mut fresh,
        "Title,URL,Username,Password,Notes,OTPAuth\nMail,https://mail.example,jon,first-secret,,",
    );
    let (_, outcome) = run(&mut fresh, TWINS);
    assert_eq!(
        outcome,
        Outcome {
            saved: 1,
            duplicates: 1,
            ..Outcome::default()
        }
    );
    assert_eq!(passwords(&fresh), ["first-secret", "second-secret"]);
}

#[test]
fn only_picked_rows_are_saved_and_a_bad_row_does_not_sink_the_rest() {
    let (_dir, mut vault) = store();
    let mut parsed = parse_text(APPLE).unwrap();
    mark_against(&vault, &mut parsed).unwrap();
    // Nothing picked, nothing saved.
    assert_eq!(commit(&mut vault, &parsed, &[]), Outcome::default());
    assert!(vault.list().is_empty());
    parsed.items[0].item.raw.name = String::new();
    let outcome = commit(&mut vault, &parsed, &[1, 2, 9]);
    assert_eq!(outcome.saved, 1);
    assert_eq!(outcome.failed.len(), 2);
    assert!(outcome.failed[0].reason.contains("title"));
    assert!(outcome.failed[1].reason.contains("no such row"));
}
