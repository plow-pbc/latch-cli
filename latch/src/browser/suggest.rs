//! What a fill may use on a page, and where a value may go: the page's
//! offers, latch's suggested mappings, and the checks every mapping passes
//! before a value leaves the vault. Pure: the worker reads each element's
//! hints (`__latch.controls`), and this decides.
//!
//! Only standard evidence counts: `autocomplete` tokens (with their
//! section, `shipping`/`billing` and contact qualifiers parsed off),
//! native input types, the page's forms and their submit buttons. Never a
//! label or a button's words.
//!
//! - **Offers:** `password`, `username_only`, `code`, `passkey`, `card`,
//!   `address`.
//! - **Suggestions:** a login per form (the password field and the single
//!   text or email field before it; a lone `username` field; a password
//!   field alone), a card across the page, an identity per form. None on a
//!   registration-like form, none for `otp`, none where the map would be
//!   ambiguous or incomplete. The press button is the form's single
//!   submit button, or none.
//! - **Checks:** what the page's own markup contradicts is refused; a weak
//!   mismatch is let through and flagged.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::state::to_hex;
use crate::vault::item::Meta;

/// The field names of the HTML autofill grammar.
const FIELD_NAMES: [&str; 53] = [
    "name",
    "honorific-prefix",
    "given-name",
    "additional-name",
    "family-name",
    "honorific-suffix",
    "nickname",
    "username",
    "new-password",
    "current-password",
    "one-time-code",
    "organization-title",
    "organization",
    "street-address",
    "address-line1",
    "address-line2",
    "address-line3",
    "address-level4",
    "address-level3",
    "address-level2",
    "address-level1",
    "country",
    "country-name",
    "postal-code",
    "cc-name",
    "cc-given-name",
    "cc-additional-name",
    "cc-family-name",
    "cc-number",
    "cc-exp",
    "cc-exp-month",
    "cc-exp-year",
    "cc-csc",
    "cc-type",
    "transaction-currency",
    "transaction-amount",
    "language",
    "bday",
    "bday-day",
    "bday-month",
    "bday-year",
    "sex",
    "url",
    "photo",
    "tel",
    "tel-country-code",
    "tel-national",
    "tel-area-code",
    "tel-local",
    "tel-extension",
    "email",
    "impp",
    "fax",
];

/// A card's item fields by token.
const CARD_TOKENS: [(&str, &str); 7] = [
    ("cc-number", "number"),
    ("cc-csc", "code"),
    ("cc-exp", "expiry"),
    ("cc-exp-month", "expiry month"),
    ("cc-exp-year", "expiry year"),
    ("cc-name", "cardholder name"),
    ("cc-type", "brand"),
];

/// An identity's item fields by token.
const IDENTITY_TOKENS: [(&str, &str); 17] = [
    ("honorific-prefix", "title"),
    ("given-name", "first name"),
    ("additional-name", "middle name"),
    ("family-name", "last name"),
    ("name", "full name"),
    ("organization", "company"),
    ("email", "email"),
    ("tel", "phone"),
    ("address-line1", "address1"),
    ("address-line2", "address2"),
    ("address-line3", "address3"),
    ("address-level2", "city"),
    ("address-level1", "state"),
    ("postal-code", "postal code"),
    ("country", "country"),
    ("country-name", "country"),
    ("bday", "date of birth"),
];

/// Input types a value never goes into.
const NOT_FIELDS: [&str; 9] = [
    "checkbox", "radio", "button", "submit", "reset", "image", "file", "hidden", "range",
];

/// What an `autocomplete` attribute says, by the autofill grammar: its
/// field name (none for `on`, `off` or anything invalid), and whether it
/// ends in `webauthn`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hint {
    pub field: Option<String>,
    pub webauthn: bool,
}

pub fn hint(raw: &str) -> Hint {
    let mut tokens: Vec<String> = raw
        .split_ascii_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let webauthn = tokens.last().is_some_and(|t| t == "webauthn");
    if webauthn {
        tokens.pop();
    }
    if tokens.first().is_some_and(|t| t.starts_with("section-")) {
        tokens.remove(0);
    }
    if tokens
        .first()
        .is_some_and(|t| t == "shipping" || t == "billing")
    {
        tokens.remove(0);
    }
    let contact = ["home", "work", "mobile", "fax", "pager"];
    if tokens.len() == 2 && contact.contains(&tokens[0].as_str()) {
        tokens.remove(0);
    }
    let field = match tokens.as_slice() {
        [one] if FIELD_NAMES.contains(&one.as_str()) => Some(one.clone()),
        _ => None,
    };
    Hint { field, webauthn }
}

/// One element of a snapshot, as a fill sees it.
#[derive(Debug, Clone, Default)]
pub struct Control {
    pub reference: String,
    /// The frame it is in, and that frame's effective origin.
    pub frame: String,
    pub origin: String,
    /// `input`, `textarea`, `select`, `editable` (contenteditable), or the
    /// tag of anything else.
    pub tag: String,
    /// An input's type, lowercased (`text` when unset).
    pub kind: String,
    pub hint: Hint,
    /// Its form, within its frame; none outside a form.
    pub form: Option<String>,
    /// A button that submits its form.
    pub submit: bool,
    /// A button or link, for the page's alternatives.
    pub clickable: bool,
    pub role: String,
    pub name: String,
    /// Why a value can't go into it now: hidden (`aria-hidden`, inert, not
    /// rendered), disabled, read-only or covered. None when it can.
    pub unusable: Option<String>,
}

impl Control {
    /// From the worker's report of one element.
    pub fn of(found: &Value) -> Control {
        let text = |key: &str| found[key].as_str().unwrap_or_default().to_string();
        let form = found["form"]
            .as_i64()
            .filter(|f| *f >= 0)
            .map(|f| format!("{}#{f}", text("frame")));
        let unusable = if found["hidden"] == true {
            Some("it is hidden from the page's readers (hidden, aria-hidden or inert)".into())
        } else if found["disabled"] == true {
            Some("it is disabled".into())
        } else if found["readonly"] == true {
            Some("it is read-only".into())
        } else if found["covered"] == true {
            Some(match found.get("cover") {
                Some(cover) => format!("{} covers it", super::core::covering(cover)),
                None => "something covers it".into(),
            })
        } else {
            None
        };
        Control {
            reference: text("ref"),
            frame: text("frame"),
            origin: text("origin"),
            tag: text("tag"),
            kind: text("type"),
            hint: hint(found["autocomplete"].as_str().unwrap_or_default()),
            form,
            submit: found["submit"] == true,
            clickable: found["clickable"] == true,
            role: text("role"),
            name: text("name"),
            unusable,
        }
    }

    /// An element a value could go into at all: a text-like input, a
    /// textarea or a dropdown. Never an editable region: its text is its
    /// name and the page's text, which latch shows the agent.
    pub fn takes_value(&self) -> bool {
        match self.tag.as_str() {
            "input" => !NOT_FIELDS.contains(&self.kind.as_str()),
            "textarea" | "select" => true,
            _ => false,
        }
    }

    fn eligible(&self) -> bool {
        self.takes_value() && self.unusable.is_none()
    }

    fn password(&self) -> bool {
        self.tag == "input" && self.kind == "password"
    }

    fn token(&self) -> Option<&str> {
        self.hint.field.as_deref()
    }

    /// The group a login or identity suggestion is made in: its form, else
    /// its frame's document.
    fn group(&self) -> String {
        self.form
            .clone()
            .unwrap_or_else(|| format!("{}#doc", self.frame))
    }
}

/// What the page asks for, from standard hints only.
pub fn offers(controls: &[Control]) -> Vec<&'static str> {
    let fields: Vec<&Control> = controls.iter().filter(|c| c.eligible()).collect();
    let token =
        |wanted: &dyn Fn(&str) -> bool| fields.iter().any(|c| c.token().is_some_and(wanted));
    let password = fields.iter().any(|c| c.password());
    let username = token(&|t| t == "username");
    let mut offers = Vec::new();
    if password {
        offers.push("password");
    }
    if username && !password {
        offers.push("username_only");
    }
    if token(&|t| t == "one-time-code") {
        offers.push("code");
    }
    if fields.iter().any(|c| c.hint.webauthn) {
        offers.push("passkey");
    }
    if token(&|t| t.starts_with("cc-")) {
        offers.push("card");
    }
    if token(&|t| {
        t.starts_with("address-")
            || matches!(
                t,
                "street-address" | "postal-code" | "country" | "country-name"
            )
    }) {
        offers.push("address");
    }
    offers
}

/// A code stage: a `one-time-code` field.
pub fn code_stage(offers: &[&str]) -> bool {
    offers.contains(&"code")
}

/// A passkey stage: a `webauthn` token, and neither a password field nor a
/// `username` token (a `username webauthn` field is an identifier stage).
pub fn passkey_stage(offers: &[&str]) -> bool {
    offers.contains(&"passkey")
        && !offers.contains(&"password")
        && !offers.contains(&"username_only")
}

/// The page shows sign-in fields: a password field, or an identifier stage.
pub fn sign_in(offers: &[&str]) -> bool {
    offers.contains(&"password") || offers.contains(&"username_only")
}

/// A mapping latch built from the page's hints.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub item: String,
    pub revision: u64,
    pub snapshot: String,
    /// Item field and the refs it goes into, in order.
    pub fields: Vec<(String, Vec<String>)>,
    pub press: Option<String>,
    /// Why each ref was chosen.
    pub evidence: Vec<(String, String)>,
    /// The origin of each frame a value would go into.
    pub frames: Vec<String>,
}

impl Suggestion {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "item": self.item,
            "revision": self.revision,
            "snapshot": self.snapshot,
            "fields": self.fields.iter().map(|(f, refs)| (f.clone(), json!(refs))).collect::<serde_json::Map<_, _>>(),
            "press": self.press,
            "evidence": self.evidence.iter().map(|(r, why)| (r.clone(), json!(why))).collect::<serde_json::Map<_, _>>(),
            "frames": self.frames,
        })
    }
}

/// Whether an item field is secret: the login's password and code, and any
/// field the vault marks hidden. A field it does not list counts as secret.
pub fn secret_field(item: &Meta, label: &str) -> bool {
    if item.kind == "login" && matches!(label, "password" | "otp") {
        return true;
    }
    item.fields
        .iter()
        .find(|f| f.label == label)
        .is_none_or(|f| f.hidden)
}

fn holds(item: &Meta, label: &str) -> bool {
    if item.kind == "login" && label == "otp" {
        return item.totp;
    }
    item.fields.iter().any(|f| f.label == label)
}

/// latch's suggestions for `item` on snapshot `snapshot`'s `controls`.
pub fn suggest(item: &Meta, snapshot: &str, controls: &[Control]) -> Vec<Suggestion> {
    let mut made = Vec::new();
    match item.kind {
        "login" => {
            for group in groups(controls) {
                if let Some((fields, evidence)) = login_map(item, controls, &group) {
                    made.push(build(
                        item,
                        snapshot,
                        controls,
                        fields,
                        evidence,
                        press_of(controls, &[group]),
                    ));
                }
            }
        }
        "card" => {
            if let Some((fields, evidence)) = token_map(item, controls, &CARD_TOKENS, None)
                && fields.iter().any(|(label, _)| label == "number")
            {
                let forms = forms_of(controls, &fields);
                let press = press_of(controls, &forms);
                made.push(build(item, snapshot, controls, fields, evidence, press));
            }
        }
        "identity" => {
            for group in groups(controls) {
                if let Some((fields, evidence)) =
                    token_map(item, controls, &IDENTITY_TOKENS, Some(&group))
                {
                    made.push(build(
                        item,
                        snapshot,
                        controls,
                        fields,
                        evidence,
                        press_of(controls, &[group]),
                    ));
                }
            }
        }
        _ => {}
    }
    made
}

type Mapped = (Vec<(String, Vec<String>)>, Vec<(String, String)>);

/// Every form (or formless document) that holds a field, in page order.
fn groups(controls: &[Control]) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for control in controls.iter().filter(|c| c.eligible()) {
        let group = control.group();
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

/// A login in one form: the password field and the single text or email
/// field before it (a `username` token first); a lone `username` field; or
/// a password field alone. None on a registration-like form.
fn login_map(item: &Meta, controls: &[Control], group: &str) -> Option<Mapped> {
    let fields: Vec<&Control> = controls
        .iter()
        .filter(|c| c.eligible() && c.group() == group)
        .collect();
    if fields.iter().any(|c| c.token() == Some("new-password")) {
        return None;
    }
    let passwords: Vec<&&Control> = fields.iter().filter(|c| c.password()).collect();
    let usernames: Vec<&&Control> = fields
        .iter()
        .filter(|c| c.token() == Some("username") && !c.password())
        .collect();
    let mut map = Vec::new();
    let mut evidence = Vec::new();
    match passwords.as_slice() {
        [] => {
            let [user] = usernames.as_slice() else {
                return None;
            };
            map.push(("username".to_string(), vec![user.reference.clone()]));
            evidence.push((user.reference.clone(), "autocomplete=username".to_string()));
        }
        [password] => {
            let at = fields
                .iter()
                .position(|c| c.reference == password.reference)?;
            let before: Vec<&&Control> = fields[..at]
                .iter()
                .filter(|c| {
                    c.tag == "input"
                        && matches!(c.kind.as_str(), "text" | "email" | "tel")
                        && !c.token().is_some_and(|t| {
                            t.starts_with("cc-") || matches!(t, "one-time-code" | "new-password")
                        })
                })
                .collect();
            let user = match usernames.as_slice() {
                [user] => Some((*user, "autocomplete=username")),
                [] => match before.as_slice() {
                    [] => None,
                    [user] => Some((*user, "the text field before the password")),
                    _ => return None,
                },
                _ => return None,
            };
            if let Some((user, why)) = user {
                if !holds(item, "username") {
                    return None;
                }
                map.push(("username".to_string(), vec![user.reference.clone()]));
                evidence.push((user.reference.clone(), why.to_string()));
            }
            map.push(("password".to_string(), vec![password.reference.clone()]));
            let why = if password.token() == Some("current-password") {
                "autocomplete=current-password"
            } else {
                "type=password"
            };
            evidence.push((password.reference.clone(), why.to_string()));
        }
        // Two password fields: a registration or a password change.
        _ => return None,
    }
    if map.iter().any(|(label, _)| !holds(item, label)) {
        return None;
    }
    Some((map, evidence))
}

/// Fields by token: every eligible field whose token names an item field,
/// in `group` (or across the page), each token at most once and each item
/// field the item holds. None when a token repeats or the item lacks one.
fn token_map(
    item: &Meta,
    controls: &[Control],
    tokens: &[(&str, &str)],
    group: Option<&str>,
) -> Option<Mapped> {
    let mut map: Vec<(String, Vec<String>)> = Vec::new();
    let mut evidence = Vec::new();
    for control in controls
        .iter()
        .filter(|c| c.eligible() && group.is_none_or(|g| c.group() == g))
    {
        let Some(token) = control.token() else {
            continue;
        };
        let Some((_, label)) = tokens.iter().find(|(t, _)| *t == token) else {
            continue;
        };
        if map.iter().any(|(l, _)| l == label) || !holds(item, label) {
            return None;
        }
        map.push((label.to_string(), vec![control.reference.clone()]));
        evidence.push((control.reference.clone(), format!("autocomplete={token}")));
    }
    (!map.is_empty()).then_some((map, evidence))
}

/// The forms the mapped fields are in.
fn forms_of(controls: &[Control], fields: &[(String, Vec<String>)]) -> Vec<String> {
    let mut forms = Vec::new();
    for (_, refs) in fields {
        for reference in refs {
            if let Some(control) = controls.iter().find(|c| &c.reference == reference) {
                let group = control.group();
                if !forms.contains(&group) {
                    forms.push(group);
                }
            }
        }
    }
    forms
}

/// The single submit button of the one form the fields are in; none for
/// several forms, a formless page, or a form with none or several.
fn press_of(controls: &[Control], forms: &[String]) -> Option<String> {
    let [form] = forms else {
        return None;
    };
    let submits: Vec<&Control> = controls
        .iter()
        .filter(|c| c.submit && c.unusable.is_none() && c.form.as_deref() == Some(form))
        .collect();
    match submits.as_slice() {
        [one] => Some(one.reference.clone()),
        _ => None,
    }
}

fn build(
    item: &Meta,
    snapshot: &str,
    controls: &[Control],
    fields: Vec<(String, Vec<String>)>,
    evidence: Vec<(String, String)>,
    press: Option<String>,
) -> Suggestion {
    let mut frames: Vec<String> = Vec::new();
    for (_, refs) in &fields {
        for reference in refs {
            if let Some(control) = controls.iter().find(|c| &c.reference == reference)
                && !frames.contains(&control.origin)
            {
                frames.push(control.origin.clone());
            }
        }
    }
    let id = suggestion_id(
        &item.item,
        item.revision,
        snapshot,
        &fields,
        press.as_deref(),
    );
    Suggestion {
        id,
        item: item.item.clone(),
        revision: item.revision,
        snapshot: snapshot.to_string(),
        fields,
        press,
        evidence,
        frames,
    }
}

/// A suggestion's id: the same mapping on the same snapshot and item
/// revision always gets the same one, so a fill can recompute it.
fn suggestion_id(
    item: &str,
    revision: u64,
    snapshot: &str,
    fields: &[(String, Vec<String>)],
    press: Option<&str>,
) -> String {
    let made = json!([item, revision, snapshot, fields, press]).to_string();
    format!("s{}", &to_hex(&Sha256::digest(made.as_bytes()))[..10])
}

/// The page's forms' other buttons and links, unclassified: the ways out
/// of a sign-in the agent may read and click.
pub fn alternatives(controls: &[Control], presses: &[&str]) -> Vec<Value> {
    controls
        .iter()
        .filter(|c| {
            c.clickable
                && c.unusable.is_none()
                && !presses.contains(&c.reference.as_str())
                && c.form.is_some()
        })
        .map(|c| json!({"ref": c.reference, "role": c.role, "name": c.name}))
        .collect()
}

/// The page's fields a value could go into, as refs, for a `needs_fields`.
pub fn candidates(controls: &[Control]) -> Vec<Value> {
    controls
        .iter()
        .filter(|c| c.eligible() || (c.submit && c.unusable.is_none()))
        .map(|c| {
            let mut shown = json!({"ref": c.reference, "role": c.role, "name": c.name});
            if let Some(token) = c.token() {
                shown["autocomplete"] = json!(token);
            }
            shown
        })
        .collect()
}

/// What item field `label` of an item of `kind` is, for the checks.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Password,
    Username,
    Otp,
    /// A card's number or code: `cc-number` or `cc-csc`.
    CardSecret(&'static str),
    /// Anything else, with the tokens that fit it (none: any).
    Other(&'static [&'static str]),
}

fn role(kind: &str, label: &str) -> Role {
    match (kind, label) {
        ("login", "password") => Role::Password,
        ("login", "username" | "email" | "login" | "user") => Role::Username,
        ("login", "otp") => Role::Otp,
        ("card", "number") => Role::CardSecret("cc-number"),
        ("card", "code" | "cvv" | "security code") => Role::CardSecret("cc-csc"),
        ("card", "expiry") => Role::Other(&["cc-exp"]),
        ("card", "expiry month") => Role::Other(&["cc-exp-month", "cc-exp"]),
        ("card", "expiry year") => Role::Other(&["cc-exp-year", "cc-exp"]),
        ("card", "cardholder name") => Role::Other(&["cc-name"]),
        ("card", "brand") => Role::Other(&["cc-type"]),
        ("identity", "username") => Role::Other(&["username", "email"]),
        ("identity", "first name") => Role::Other(&["given-name"]),
        ("identity", "middle name") => Role::Other(&["additional-name"]),
        ("identity", "last name") => Role::Other(&["family-name"]),
        ("identity", "full name") => Role::Other(&["name"]),
        ("identity", "title") => Role::Other(&["honorific-prefix"]),
        ("identity", "company") => Role::Other(&["organization"]),
        ("identity", "email") => Role::Other(&["email", "username"]),
        ("identity", "phone") => {
            Role::Other(&["tel", "tel-national", "tel-local", "tel-country-code"])
        }
        ("identity", "address1") => Role::Other(&["address-line1", "street-address"]),
        ("identity", "address2") => Role::Other(&["address-line2"]),
        ("identity", "address3") => Role::Other(&["address-line3"]),
        ("identity", "city") => Role::Other(&["address-level2"]),
        ("identity", "state") => Role::Other(&["address-level1"]),
        ("identity", "postal code") => Role::Other(&["postal-code"]),
        ("identity", "country") => Role::Other(&["country", "country-name"]),
        ("identity", "date of birth") => Role::Other(&["bday"]),
        _ => Role::Other(&[]),
    }
}

/// Whether item field `label` of an item of `kind` may go into `control`:
/// `Err` with the reason when the page's own markup contradicts it, else
/// the token it carries when that is a weak mismatch (`hint_mismatch`).
pub fn check(kind: &str, label: &str, control: &Control) -> Result<Option<String>, String> {
    if !control.takes_value() {
        return Err(format!(
            "{} is not a field a value goes into",
            control.reference
        ));
    }
    if let Some(why) = &control.unusable {
        return Err(format!("{}: {why}", control.reference));
    }
    let token = control.token();
    let marked = |t: &str| format!("{} is marked autocomplete={t}", control.reference);
    let card_token = token.is_some_and(|t| t.starts_with("cc-"));
    let search = control.kind == "search";
    match role(kind, label) {
        Role::Password => {
            if let Some(t) = token
                .filter(|t| matches!(*t, "new-password" | "one-time-code") || t.starts_with("cc-"))
            {
                return Err(marked(t));
            }
            if search {
                return Err(format!("{} is a search field", control.reference));
            }
            if !control.password() && !(control.tag == "input" && token == Some("current-password"))
            {
                return Err(format!(
                    "{} is not a password field (neither type=password nor autocomplete=current-password)",
                    control.reference
                ));
            }
            Ok(None)
        }
        Role::Username => {
            if control.password() {
                return Err(format!("{} is a password field", control.reference));
            }
            if let Some(t) = token.filter(|t| {
                matches!(*t, "one-time-code" | "new-password" | "current-password")
                    || t.starts_with("cc-")
            }) {
                return Err(marked(t));
            }
            if search {
                return Err(format!("{} is a search field", control.reference));
            }
            Ok(token
                .filter(|t| !matches!(*t, "username" | "email"))
                .map(str::to_string))
        }
        Role::Otp => {
            if control.password() && token == Some("current-password") {
                return Err(format!(
                    "{} is the sign-in's password field",
                    control.reference
                ));
            }
            if let Some(t) =
                token.filter(|t| matches!(*t, "username" | "email") || t.starts_with("cc-"))
            {
                return Err(marked(t));
            }
            Ok(token.filter(|t| *t != "one-time-code").map(str::to_string))
        }
        Role::CardSecret(wanted) => {
            let shape_ok = control.tag == "input"
                && matches!(control.kind.as_str(), "text" | "tel" | "number");
            if !shape_ok {
                return Err(format!(
                    "{} is a {}, and a card's {label} goes only into a text, tel or number input",
                    control.reference,
                    if control.tag == "input" {
                        format!("{} input", control.kind)
                    } else {
                        control.tag.clone()
                    }
                ));
            }
            if let Some(t) = token.filter(|t| *t != wanted) {
                return Err(marked(t));
            }
            Ok(None)
        }
        Role::Other(fits) => {
            if control.password() {
                return Err(format!("{} is a password field", control.reference));
            }
            if card_token && kind != "card" {
                return Err(marked(token.unwrap_or_default()));
            }
            Ok(token
                .filter(|t| !fits.is_empty() && !fits.contains(t))
                .map(str::to_string))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::classify::Field;

    fn field(label: &str, hidden: bool) -> Field {
        Field {
            label: label.into(),
            hidden,
            custom: false,
            alias: false,
        }
    }

    fn meta(kind: &'static str, fields: &[(&str, bool)], totp: bool) -> Meta {
        Meta {
            item: "v1".into(),
            revision: 3,
            kind,
            title: "Shop".into(),
            username: None,
            sites: vec!["shop.example".into()],
            other_top_sites: vec![],
            fields: fields.iter().map(|(l, h)| field(l, *h)).collect(),
            totp,
        }
    }

    fn login() -> Meta {
        meta("login", &[("username", false), ("password", true)], true)
    }

    /// An input in form 0 of frame f0.
    fn input(reference: &str, kind: &str, autocomplete: &str) -> Control {
        Control {
            reference: reference.into(),
            frame: "f0".into(),
            origin: "https://shop.example".into(),
            tag: "input".into(),
            kind: kind.into(),
            hint: hint(autocomplete),
            form: Some("f0#0".into()),
            role: "textbox".into(),
            name: reference.into(),
            ..Control::default()
        }
    }

    fn submit(reference: &str) -> Control {
        Control {
            reference: reference.into(),
            frame: "f0".into(),
            tag: "button".into(),
            form: Some("f0#0".into()),
            submit: true,
            clickable: true,
            role: "button".into(),
            ..Control::default()
        }
    }

    fn in_form(mut control: Control, form: &str) -> Control {
        control.form = Some(form.into());
        control
    }

    #[test]
    fn autocomplete_tokens_parse_with_their_qualifiers() {
        assert_eq!(hint("username").field.as_deref(), Some("username"));
        assert_eq!(
            hint("section-a shipping address-line1").field.as_deref(),
            Some("address-line1")
        );
        assert_eq!(hint("billing work email").field.as_deref(), Some("email"));
        let both = hint("username webauthn");
        assert_eq!(
            (both.field.as_deref(), both.webauthn),
            (Some("username"), true)
        );
        for ignored in ["on", "off", "password", "", "username email"] {
            assert_eq!(hint(ignored).field, None, "{ignored:?}");
        }
    }

    #[test]
    fn a_login_is_suggested_from_tokens_and_from_the_field_before_the_password() {
        let tokened = [
            input("e1", "email", "username"),
            input("e2", "password", "current-password"),
            submit("e3"),
        ];
        let [made] = suggest(&login(), "o1", &tokened).try_into().unwrap();
        assert_eq!(
            made.fields,
            [
                ("username".into(), vec!["e1".into()]),
                ("password".into(), vec!["e2".into()])
            ]
        );
        assert_eq!(made.press.as_deref(), Some("e3"));
        assert_eq!(made.evidence[1].1, "autocomplete=current-password");
        // The same mapping again gets the same id; another snapshot does not.
        assert_eq!(suggest(&login(), "o1", &tokened)[0].id, made.id);
        assert_ne!(suggest(&login(), "o2", &tokened)[0].id, made.id);

        let bare = [
            input("e1", "text", ""),
            input("e2", "password", "off"),
            submit("e3"),
        ];
        let [made] = suggest(&login(), "o1", &bare).try_into().unwrap();
        assert_eq!(made.evidence[0].1, "the text field before the password");
        assert_eq!(made.evidence[1].1, "type=password");
    }

    #[test]
    fn an_identifier_stage_needs_a_username_token() {
        let lone = [input("e1", "email", "username webauthn"), submit("e2")];
        let [made] = suggest(&login(), "o1", &lone).try_into().unwrap();
        assert_eq!(made.fields, [("username".into(), vec!["e1".into()])]);
        assert_eq!(offers(&lone), ["username_only", "passkey"]);
        assert!(!passkey_stage(&offers(&lone)));
        // A newsletter box: an email token alone is no sign-in.
        let newsletter = [input("e1", "email", "email"), submit("e2")];
        assert!(suggest(&login(), "o1", &newsletter).is_empty());
        assert!(!sign_in(&offers(&newsletter)));
    }

    #[test]
    fn no_suggestion_on_registration_codes_or_ambiguity() {
        let signup = [
            input("e1", "email", "username"),
            input("e2", "password", "new-password"),
            submit("e3"),
        ];
        assert!(suggest(&login(), "o1", &signup).is_empty());
        let twice = [
            input("e1", "email", ""),
            input("e2", "password", ""),
            input("e3", "password", ""),
            submit("e4"),
        ];
        assert!(suggest(&login(), "o1", &twice).is_empty());
        let code = [input("e1", "text", "one-time-code"), submit("e2")];
        assert!(suggest(&login(), "o1", &code).is_empty());
        assert!(code_stage(&offers(&code)));
        let two_before = [
            input("e1", "text", ""),
            input("e2", "text", ""),
            input("e3", "password", ""),
        ];
        assert!(suggest(&login(), "o1", &two_before).is_empty());
        // Two submit buttons: the mapping, with no press.
        let two_buttons = [
            input("e1", "email", "username"),
            input("e2", "password", ""),
            submit("e3"),
            submit("e4"),
        ];
        assert_eq!(suggest(&login(), "o1", &two_buttons)[0].press, None);
    }

    #[test]
    fn hidden_duplicates_are_never_suggested_and_two_forms_get_one_each() {
        let card = meta(
            "card",
            &[
                ("number", true),
                ("code", true),
                ("expiry", false),
                ("cardholder name", false),
            ],
            false,
        );
        let mut hidden = input("e9", "text", "cc-number");
        hidden.unusable = Some("hidden".into());
        let page = [
            hidden,
            input("e1", "text", "cc-number"),
            input("e2", "text", "cc-exp"),
            input("e3", "text", "cc-csc"),
            submit("e4"),
        ];
        let [made] = suggest(&card, "o1", &page).try_into().unwrap();
        assert_eq!(made.fields[0], ("number".into(), vec!["e1".into()]));
        assert_eq!(made.press.as_deref(), Some("e4"));
        assert!(
            made.fields
                .iter()
                .any(|(label, _)| secret_field(&card, label))
        );

        let two = [
            input("e1", "email", "username"),
            input("e2", "password", "current-password"),
            submit("e3"),
            in_form(input("e4", "email", "username"), "f0#1"),
            in_form(input("e5", "password", "current-password"), "f0#1"),
            in_form(submit("e6"), "f0#1"),
        ];
        let made = suggest(&login(), "o1", &two);
        assert_eq!(made.len(), 2);
        assert_eq!(made[1].press.as_deref(), Some("e6"));
    }

    #[test]
    fn a_card_without_the_field_the_page_asks_for_gets_no_suggestion() {
        let card = meta("card", &[("number", true), ("expiry", false)], false);
        let page = [
            input("e1", "text", "cc-number"),
            input("e2", "text", "cc-csc"),
        ];
        assert!(suggest(&card, "o1", &page).is_empty());
    }

    #[test]
    fn the_checks_refuse_what_the_markup_contradicts() {
        let refused =
            |kind: &str, label: &str, control: Control| check(kind, label, &control).unwrap_err();
        refused("login", "password", input("e1", "text", ""));
        assert!(
            refused("login", "password", input("e1", "password", "new-password"))
                .contains("new-password")
        );
        refused("login", "username", input("e1", "password", ""));
        refused("login", "otp", input("e1", "password", "current-password"));
        let mut area = input("e1", "", "");
        area.tag = "textarea".into();
        assert!(refused("card", "number", area).contains("textarea"));
        refused("card", "number", input("e1", "search", ""));
        refused("card", "number", input("e1", "text", "email"));
        refused("identity", "first name", input("e1", "text", "cc-number"));
        let mut covered = input("e1", "text", "");
        covered.unusable = Some("something covers it".into());
        refused("identity", "city", covered);

        // Allowed: a revealed password, an unhinted card field, and a weak
        // mismatch, flagged.
        assert_eq!(
            check(
                "login",
                "password",
                &input("e1", "text", "current-password")
            ),
            Ok(None)
        );
        assert_eq!(check("card", "number", &input("e1", "text", "")), Ok(None));
        assert_eq!(
            check("identity", "email", &input("e1", "tel", "tel-national")),
            Ok(Some("tel-national".into()))
        );
        assert_eq!(
            check("login", "username", &input("e1", "tel", "tel-national")),
            Ok(Some("tel-national".into()))
        );
    }
}
