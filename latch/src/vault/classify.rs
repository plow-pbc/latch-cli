//! Which fields an item has, which are secret, and what each label releases:
//! the Latch app's classifier, ported rule for rule with its truth table
//! (`tests/fixtures/maskClassification.json`) as the test.
//!
//! The concealment rules are the vault's own, Bitwarden's: a field is
//! secret exactly when the Bitwarden client renders it masked (a login's
//! password, a card's number and code, an identity's SSN and passport
//! number, an SSH private key, a hidden custom field), plus one deliberate
//! addition, the login's TOTP key. A linked custom field is as secret as
//! the slot it points at. Labels never carry a value; only `read`
//! returns one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::dates::{self, CARD_EXPIRY, DATE_OF_BIRTH};
use super::item::{CARD, Raw};

pub const PASSWORD: &str = "password";
pub const TOTP: &str = "totp";
pub const USERNAME: &str = "username";
/// The other names a login's username is released under.
const USERNAME_ALIASES: [&str; 3] = ["email", "login", "user"];

/// A card's parts by label, and the vault key behind each. `cvv` and
/// `security code` are what checkout forms say; the vault says `code`.
const CARD_FIELDS: [(&str, &str); 8] = [
    ("number", "number"),
    ("code", "code"),
    ("cvv", "code"),
    ("security code", "code"),
    ("expiry month", "expMonth"),
    ("expiry year", "expYear"),
    ("cardholder name", "cardholderName"),
    ("brand", "brand"),
];
/// The order a card's own slots are listed in.
const CARD_SLOTS: [(&str, &str); 6] = [
    ("number", "number"),
    ("code", "code"),
    ("expMonth", "expiry month"),
    ("expYear", "expiry year"),
    ("cardholderName", "cardholder name"),
    ("brand", "brand"),
];
const HIDDEN_CARD_LABELS: [&str; 2] = ["number", "code"];

/// An identity's parts by label, in the order they are listed. `birthDate`
/// is the Latch app's own addition, kept as ISO so a fill can reshape it.
const IDENTITY_FIELDS: [(&str, &str); 19] = [
    ("title", "title"),
    ("first name", "firstName"),
    ("middle name", "middleName"),
    ("last name", "lastName"),
    ("username", "username"),
    ("company", "company"),
    ("ssn", "ssn"),
    ("passport number", "passportNumber"),
    ("license number", "licenseNumber"),
    ("email", "email"),
    ("phone", "phone"),
    ("address1", "address1"),
    ("address2", "address2"),
    ("address3", "address3"),
    ("city", "city"),
    ("state", "state"),
    ("postal code", "postalCode"),
    ("country", "country"),
    (DATE_OF_BIRTH, "birthDate"),
];
const HIDDEN_IDENTITY_LABELS: [&str; 2] = ["ssn", "passport number"];
/// Composed from the title and name parts; releasable, never listed.
const FULL_NAME: &str = "full name";

const SSH_KEY_FIELDS: [(&str, &str); 3] = [
    ("private key", "privateKey"),
    ("public key", "publicKey"),
    ("fingerprint", "keyFingerprint"),
];
const HIDDEN_SSH_KEY_LABELS: [&str; 1] = ["private key"];

const CUSTOM_HIDDEN: u8 = 1;
const CUSTOM_LINKED: u8 = 3;
/// How a custom field is named when a fixed slot already owns its name.
const CUSTOM_PREFIX: &str = "custom:";

/// What a linked custom field points at, by Bitwarden's numbers.
fn linked_label(id: u16) -> Option<&'static str> {
    Some(match id {
        100 => USERNAME,
        101 => PASSWORD,
        300 => "cardholder name",
        301 => "expiry month",
        302 => "expiry year",
        303 => "code",
        304 => "brand",
        305 => "number",
        400 => "title",
        401 => "middle name",
        402 => "address1",
        403 => "address2",
        404 => "address3",
        405 => "city",
        406 => "state",
        407 => "postal code",
        408 => "country",
        409 => "company",
        410 => "email",
        411 => "phone",
        412 => "ssn",
        413 => USERNAME,
        414 => "passport number",
        415 => "license number",
        416 => "first name",
        417 => "last name",
        418 => FULL_NAME,
        _ => return None,
    })
}

/// One field an item has, by the label it is released under.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Field {
    pub label: String,
    /// Secret: the vault renders it masked, and it must leave the page
    /// before the agent resumes.
    pub hidden: bool,
    pub custom: bool,
    /// Another name for a slot already listed.
    pub alias: bool,
}

fn text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn part(map: &Option<Map<String, Value>>, key: &str) -> Option<String> {
    text(map.as_ref().and_then(|m| m.get(key)))
}

fn aliases(label: &str, custom: bool) -> Vec<&'static str> {
    if custom {
        return Vec::new();
    }
    if label == USERNAME {
        let mut names = USERNAME_ALIASES.to_vec();
        names.sort();
        return names;
    }
    let Some((_, slot)) = CARD_FIELDS.iter().find(|(l, _)| *l == label) else {
        return Vec::new();
    };
    let mut names: Vec<&str> = CARD_FIELDS
        .iter()
        .filter(|(alias, key)| key == slot && *alias != label)
        .map(|(alias, _)| *alias)
        .collect();
    names.sort();
    names
}

/// A custom field's token: its own name, unless a fixed slot owns it.
fn custom_label(name: &str, so_far: &[Field]) -> String {
    if so_far.iter().any(|d| d.label == name && !d.custom) {
        format!("{CUSTOM_PREFIX}{name}")
    } else {
        name.to_string()
    }
}

/// What the item has, by label, each marked secret or not. Aliases are
/// listed after their slot. A token two custom fields share names neither,
/// so both are dropped rather than one picked by order.
pub fn fields(item: &Raw) -> Vec<Field> {
    let is_card = item.kind == CARD;
    let mut out: Vec<Field> = Vec::new();
    let add =
        |out: &mut Vec<Field>, label: &str, hidden: bool, custom: bool, with_aliases: bool| {
            out.push(Field {
                label: label.to_string(),
                hidden,
                custom,
                alias: false,
            });
            if with_aliases {
                for alias in aliases(label, custom) {
                    out.push(Field {
                        label: alias.to_string(),
                        hidden,
                        custom: false,
                        alias: true,
                    });
                }
            }
        };

    if let Some(login) = &item.login {
        if login.username.as_deref().is_some_and(|v| !v.is_empty()) {
            add(&mut out, USERNAME, false, false, true);
        }
        if login.password.as_deref().is_some_and(|v| !v.is_empty()) {
            add(&mut out, PASSWORD, true, false, true);
        }
        // The one place this masks what the client shows: an agent fills
        // a code and moves on; only a person needs to read it.
        if login.totp.as_deref().is_some_and(|v| !v.is_empty()) {
            add(&mut out, TOTP, true, false, true);
        }
    }
    for (key, label) in CARD_SLOTS {
        if part(&item.card, key).is_some() {
            let hidden = is_card && HIDDEN_CARD_LABELS.contains(&label);
            add(&mut out, label, hidden, false, true);
        }
    }
    if dates::expiry_iso(
        part(&item.card, "expMonth").as_deref(),
        part(&item.card, "expYear").as_deref(),
    )
    .is_some()
    {
        add(&mut out, CARD_EXPIRY, false, false, false);
    }
    if item.identity.is_some() {
        for (label, key) in IDENTITY_FIELDS {
            if part(&item.identity, key).is_some() {
                add(
                    &mut out,
                    label,
                    HIDDEN_IDENTITY_LABELS.contains(&label),
                    false,
                    false,
                );
            }
        }
    }
    for (label, key) in SSH_KEY_FIELDS {
        if part(&item.ssh_key, key).is_some() {
            add(
                &mut out,
                label,
                HIDDEN_SSH_KEY_LABELS.contains(&label),
                false,
                false,
            );
        }
    }
    for field in &item.fields {
        let Some(name) = field.name.as_deref().filter(|n| !n.is_empty()) else {
            continue;
        };
        if field.kind == CUSTOM_LINKED {
            // As secret as what it points at, and only listed if that is set.
            let target = field.linked_id.and_then(linked_label);
            if let Some(target) = target
                && slot(item, target).is_some()
            {
                let hidden = out
                    .iter()
                    .find(|d| d.label == target && !d.custom)
                    .is_some_and(|d| d.hidden);
                let label = custom_label(name, &out);
                add(&mut out, &label, hidden, true, false);
            }
            continue;
        }
        if field.value.is_some() || field.kind == CUSTOM_HIDDEN {
            let label = custom_label(name, &out);
            add(&mut out, &label, field.kind == CUSTOM_HIDDEN, true, true);
        }
    }
    if item.notes.as_deref().is_some_and(|n| !n.is_empty()) {
        add(&mut out, "notes", false, false, true);
    }
    let count = |label: &str| out.iter().filter(|d| d.label == label).count();
    let unique: Vec<bool> = out.iter().map(|d| count(&d.label) == 1).collect();
    out.into_iter()
        .zip(unique)
        .filter_map(|(d, keep)| keep.then_some(d))
        .collect()
}

/// The value of one of the item's own fixed slots, never a custom field.
pub fn slot(item: &Raw, label: &str) -> Option<String> {
    let login = item.login.clone().unwrap_or_default();
    let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty());
    if label == PASSWORD {
        return nonempty(login.password);
    }
    if (label == USERNAME || USERNAME_ALIASES.contains(&label))
        && let Some(username) = nonempty(login.username.clone())
    {
        return Some(username);
    }
    if let Some((_, key)) = CARD_FIELDS.iter().find(|(l, _)| *l == label) {
        return part(&item.card, key);
    }
    if label == CARD_EXPIRY {
        return dates::expiry_iso(
            part(&item.card, "expMonth").as_deref(),
            part(&item.card, "expYear").as_deref(),
        );
    }
    if let Some((_, key)) = IDENTITY_FIELDS.iter().find(|(l, _)| *l == label) {
        return part(&item.identity, key);
    }
    if label == FULL_NAME {
        let joined: Vec<String> = ["title", "firstName", "middleName", "lastName"]
            .iter()
            .filter_map(|k| part(&item.identity, k))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        return (!joined.is_empty()).then(|| joined.join(" "));
    }
    if let Some((_, key)) = SSH_KEY_FIELDS.iter().find(|(l, _)| *l == label) {
        return part(&item.ssh_key, key);
    }
    None
}

/// The value behind one label: a slot first, then the note body, then a
/// custom field (a name two custom fields share releases neither).
#[allow(dead_code)] // the browser's fills call it
pub fn read(item: &Raw, label: &str) -> Option<String> {
    if let Some(value) = slot(item, label) {
        return Some(value);
    }
    if label == "notes" {
        return item.notes.clone().filter(|n| !n.is_empty());
    }
    let wanted = [Some(label), label.strip_prefix(CUSTOM_PREFIX)];
    for wanted in wanted.into_iter().flatten() {
        let matches: Vec<_> = item
            .fields
            .iter()
            .filter(|c| c.name.as_deref() == Some(wanted))
            .collect();
        match matches.as_slice() {
            [] => continue,
            [custom] => {
                if custom.kind == CUSTOM_LINKED {
                    return custom
                        .linked_id
                        .and_then(linked_label)
                        .and_then(|target| slot(item, target));
                }
                return custom.value.clone().filter(|v| !v.is_empty());
            }
            _ => return None,
        }
    }
    None
}

/// Whether `label` is one of the item's secret fields. A label the item
/// does not list counts as secret: nothing unclassified is treated as safe.
pub fn is_secret(item: &Raw, label: &str) -> bool {
    fields(item)
        .iter()
        .find(|d| d.label == label)
        .is_none_or(|d| d.hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        bitwarden: Raw,
        expect: Vec<Field>,
    }

    #[derive(Deserialize)]
    struct Table {
        cases: Vec<Case>,
    }

    fn table() -> Table {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/maskClassification.json"
        );
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn the_classifier_truth_table_holds() {
        let table = table();
        assert_eq!(table.cases.len(), 10);
        for case in table.cases {
            assert_eq!(fields(&case.bitwarden), case.expect, "{}", case.name);
        }
    }

    #[test]
    fn labels_never_carry_values_and_reads_resolve_links_and_collisions() {
        let table = table();
        let planted = [
            "hunter2",
            "recovery-code-9f2a",
            "4111111111111111",
            "737",
            "078-05-1120",
            "X1234567",
        ];
        for case in &table.cases {
            let listed = serde_json::to_string(&fields(&case.bitwarden)).unwrap();
            for secret in planted {
                assert!(!listed.contains(secret), "{}: {secret}", case.name);
            }
        }
        let by_name = |name: &str| {
            &table
                .cases
                .iter()
                .find(|c| c.name == name)
                .unwrap()
                .bitwarden
        };
        let card = by_name("card with a colliding custom field");
        // The slot answers first; the custom field has its own token.
        assert_eq!(read(card, "cardholder name").as_deref(), Some("Jon Doe"));
        assert_eq!(
            read(card, "custom:cardholder name").as_deref(),
            Some("shipping only")
        );
        assert_eq!(read(card, "cvv").as_deref(), Some("737"));
        assert_eq!(read(card, "expiry").as_deref(), Some("2031-04"));
        assert!(is_secret(card, "number") && is_secret(card, "cvv"));
        assert!(!is_secret(card, "expiry") && !is_secret(card, "cardholder name"));
        assert!(is_secret(card, "no such field"));
        let linked = by_name("identity with linked custom fields");
        assert_eq!(read(linked, "tax id").as_deref(), Some("078-05-1120"));
        assert_eq!(read(linked, "who").as_deref(), Some("Mx Jon Q Doe"));
        assert!(is_secret(linked, "tax id"));
        let twins = by_name("card with two custom fields of the same name");
        // Both are called "number", which the card's slot owns: the slot still
        // answers, and the qualified token names neither twin.
        assert_eq!(twins.fields[0].name.as_deref(), Some("number"));
        assert_eq!(read(twins, "custom:number"), None);
    }
}
