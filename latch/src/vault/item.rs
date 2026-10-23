//! Vault items. The values are kept in Bitwarden's item shape (`Raw`), the
//! shape rbw and Bitwarden's own JSON export use, so field names, custom
//! fields and the classifier's rules carry over unchanged. Around it, what
//! only latch knows: the item's id and revision, and the sites it may fill
//! on.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::dates;
use super::sites;
use crate::failure::Failure;

/// Bitwarden's item types; latch keeps the first four.
pub const LOGIN: u8 = 1;
pub const NOTE: u8 = 2;
pub const CARD: u8 = 3;
pub const IDENTITY: u8 = 4;
pub const SSH_KEY: u8 = 5;

/// One item's values, in Bitwarden's JSON shape.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Raw {
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<Login>,
    /// Card, identity and SSH-key parts by Bitwarden's own keys
    /// (`number`, `expMonth`, `firstName`, …); a key it does not define is
    /// kept but never classified or released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Custom>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Login {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// An authenticator key: base32, or an `otpauth://` URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uris: Vec<Uri>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Uri {
    #[serde(default)]
    pub uri: Option<String>,
}

/// A custom field: type 0 text, 1 hidden, 2 boolean, 3 linked (to one of
/// the item's own slots, by Bitwarden's number for it).
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Custom {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_id: Option<u16>,
}

/// One stored item.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    /// Bumped by every change; an edit made against an older one is refused.
    pub revision: u64,
    /// A login's site, and any more it also signs in on; for a card or an
    /// identity, the optional "only on these sites" list. Each is a
    /// registrable domain (`example.com`), from what was given.
    pub sites: Vec<String>,
    /// A login's other top-level sites it may be filled under (a sign-in
    /// page embedded by another site).
    pub other_top_sites: Vec<String>,
    pub raw: Raw,
}

/// What `list` shows: a login's username, never a secret value.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Meta {
    pub item: String,
    pub revision: u64,
    pub kind: &'static str,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub sites: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_top_sites: Vec<String>,
    /// The item's fields by label, and whether each is secret.
    pub fields: Vec<super::classify::Field>,
    pub totp: bool,
}

pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        LOGIN => "login",
        NOTE => "note",
        CARD => "card",
        IDENTITY => "identity",
        SSH_KEY => "ssh_key",
        _ => "unknown",
    }
}

pub fn kind_code(name: &str) -> Option<u8> {
    match name {
        "login" => Some(LOGIN),
        "note" => Some(NOTE),
        "card" => Some(CARD),
        "identity" => Some(IDENTITY),
        _ => None,
    }
}

/// Bitwarden's keys for each item field an owner enters, by label.
const LABELS: &[(u8, &str, &str)] = &[
    (LOGIN, "username", "username"),
    (LOGIN, "password", "password"),
    (LOGIN, "totp", "totp"),
    (CARD, "cardholder name", "cardholderName"),
    (CARD, "brand", "brand"),
    (CARD, "number", "number"),
    (CARD, "expiry month", "expMonth"),
    (CARD, "expiry year", "expYear"),
    (CARD, "code", "code"),
    (IDENTITY, "title", "title"),
    (IDENTITY, "first name", "firstName"),
    (IDENTITY, "middle name", "middleName"),
    (IDENTITY, "last name", "lastName"),
    (IDENTITY, "username", "username"),
    (IDENTITY, "company", "company"),
    (IDENTITY, "ssn", "ssn"),
    (IDENTITY, "passport number", "passportNumber"),
    (IDENTITY, "license number", "licenseNumber"),
    (IDENTITY, "email", "email"),
    (IDENTITY, "phone", "phone"),
    (IDENTITY, "address1", "address1"),
    (IDENTITY, "address2", "address2"),
    (IDENTITY, "address3", "address3"),
    (IDENTITY, "city", "city"),
    (IDENTITY, "state", "state"),
    (IDENTITY, "postal code", "postalCode"),
    (IDENTITY, "country", "country"),
    (IDENTITY, "date of birth", "birthDate"),
];

/// The labels an owner can give values for, per kind, and whether each is
/// secret, as the classifier says (so it is prompted for without echo).
pub fn entry_labels(kind: u8) -> Vec<(&'static str, bool)> {
    let mut labels: Vec<(&'static str, bool)> = LABELS
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .map(|(_, label, _)| {
            let probe = Raw::from_labels(kind, "probe", &[((*label).into(), "1".into())])
                .expect("a listed label");
            // The login's key is no form field, but it holds both factors.
            let secret = *label == "totp" || super::classify::is_secret(&probe, label);
            (*label, secret)
        })
        .collect();
    labels.push(("notes", false));
    labels
}

impl Raw {
    /// An item of `kind` from values by label (`cvv` for a card's `code`).
    pub fn from_labels(kind: u8, title: &str, values: &[(String, String)]) -> Result<Raw, String> {
        let mut raw = Raw {
            kind,
            name: title.to_string(),
            ..Raw::default()
        };
        for (label, value) in values {
            if !value.is_empty() {
                raw.set(label, value)?;
            }
        }
        Ok(raw)
    }

    /// Bitwarden's key for the field `label` of this item's kind (`cvv`
    /// for a card's `code`); `notes` is every kind's.
    fn key(&self, label: &str) -> Result<&'static str, String> {
        let label = if self.kind == CARD && (label == "cvv" || label == "security code") {
            "code"
        } else {
            label
        };
        if label == "notes" {
            return Ok("notes");
        }
        LABELS
            .iter()
            .find(|(k, l, _)| *k == self.kind && *l == label)
            .map(|(_, _, key)| *key)
            .ok_or_else(|| format!("a {} has no field {label:?}", kind_name(self.kind)))
    }

    /// Sets the field `label`; an empty value clears it.
    pub fn set(&mut self, label: &str, value: &str) -> Result<(), String> {
        let key = self.key(label)?;
        let text = (!value.is_empty()).then(|| value.to_string());
        if key == "notes" {
            self.notes = text;
            return Ok(());
        }
        let part = match self.kind {
            LOGIN => {
                let login = self.login.get_or_insert_with(Login::default);
                match key {
                    "username" => login.username = text,
                    "password" => login.password = text,
                    _ => login.totp = text,
                }
                return Ok(());
            }
            CARD => self.card.get_or_insert_with(Map::new),
            _ => self.identity.get_or_insert_with(Map::new),
        };
        match text {
            Some(text) => part.insert(key.into(), Value::String(text)),
            None => part.remove(key),
        };
        Ok(())
    }

    /// The field `label` as stored, if it holds anything; the vault's own,
    /// to tell what an edit changed.
    pub fn get(&self, label: &str) -> Option<String> {
        let key = self.key(label).ok()?;
        let login = self.login.as_ref();
        let value = match (key, self.kind) {
            ("notes", _) => self.notes.clone(),
            ("username", LOGIN) => login.and_then(|l| l.username.clone()),
            ("password", LOGIN) => login.and_then(|l| l.password.clone()),
            ("totp", LOGIN) => login.and_then(|l| l.totp.clone()),
            (key, CARD) => self.card.as_ref()?.get(key)?.as_str().map(str::to_string),
            (key, _) => self
                .identity
                .as_ref()?
                .get(key)?
                .as_str()
                .map(str::to_string),
        };
        value.filter(|v| !v.is_empty())
    }
}

impl Item {
    pub fn meta(&self) -> Meta {
        let login = self.raw.login.as_ref();
        Meta {
            item: self.id.clone(),
            revision: self.revision,
            kind: kind_name(self.raw.kind),
            title: self.raw.name.clone(),
            username: login
                .and_then(|l| l.username.clone())
                .filter(|u| !u.is_empty()),
            sites: self.sites.clone(),
            other_top_sites: self.other_top_sites.clone(),
            fields: super::classify::fields(&self.raw)
                .into_iter()
                .filter(|f| !f.alias)
                .collect(),
            totp: login
                .and_then(|l| l.totp.as_deref())
                .is_some_and(|t| !t.is_empty()),
        }
    }

    /// Checks an item before it is stored: one of latch's four kinds, a
    /// title, sites that read as registrable domains (a login needs one),
    /// a readable authenticator key and card expiry, valid profile names.
    pub fn validate(&mut self) -> Result<(), Failure> {
        // The vault keeps a card's expiry in its stored shape (04, 2031).
        if let Some(card) = self.raw.card.as_mut() {
            for part in ["expMonth", "expYear"] {
                if let Some(Value::String(given)) = card.get(part).cloned()
                    && let Ok(stored) = dates::expiry_part(part, &given)
                {
                    card.insert(part.into(), Value::String(stored));
                }
            }
        }
        let bad = |message: String| Failure::usage("bad_item", message);
        if ![LOGIN, NOTE, CARD, IDENTITY].contains(&self.raw.kind) {
            return Err(bad("an item is a login, card, identity or note".into()));
        }
        if self.raw.name.trim().is_empty() {
            return Err(bad("an item needs a title".into()));
        }
        let mut domains = Vec::new();
        for site in &self.sites {
            domains.push(sites::registrable(site).map_err(bad)?);
        }
        self.sites = domains;
        let mut tops = Vec::new();
        for site in &self.other_top_sites {
            tops.push(sites::registrable(site).map_err(bad)?);
        }
        self.other_top_sites = tops;
        match self.raw.kind {
            LOGIN => {
                if self.sites.is_empty() {
                    return Err(bad(
                        "a login needs its site, or it can never be filled".into()
                    ));
                }
                if let Some(totp) = self.raw.login.as_ref().and_then(|l| l.totp.as_deref())
                    && !totp.is_empty()
                {
                    super::totp::Params::parse(totp).map_err(bad)?;
                }
            }
            NOTE => {
                if !self.sites.is_empty() || !self.other_top_sites.is_empty() {
                    return Err(bad("a note fills on any site; it takes no site list".into()));
                }
            }
            _ => {
                if !self.other_top_sites.is_empty() {
                    return Err(bad("only a login takes other top-level sites".into()));
                }
            }
        }
        if self.raw.kind == CARD
            && let Some(card) = &self.raw.card
        {
            for part in ["expMonth", "expYear"] {
                if let Some(value) = card.get(part).and_then(Value::as_str)
                    && !value.is_empty()
                {
                    dates::expiry_part(part, value).map_err(bad)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_labels_are_secret_as_the_classifier_says() {
        let card = entry_labels(CARD);
        let secret: Vec<&str> = card.iter().filter(|(_, s)| *s).map(|(l, _)| *l).collect();
        assert_eq!(secret, ["number", "code"]);
        let identity = entry_labels(IDENTITY);
        let secret: Vec<&str> = identity
            .iter()
            .filter(|(_, s)| *s)
            .map(|(l, _)| *l)
            .collect();
        assert_eq!(secret, ["ssn", "passport number"]);
        let login = entry_labels(LOGIN);
        let secret: Vec<&str> = login.iter().filter(|(_, s)| *s).map(|(l, _)| *l).collect();
        assert_eq!(secret, ["password", "totp"]);
    }

    #[test]
    fn an_item_is_built_from_labels_and_checked() {
        let values = [
            ("number".to_string(), "4111111111111111".to_string()),
            ("cvv".to_string(), "737".to_string()),
            ("expiry month".to_string(), "4".to_string()),
            ("expiry year".to_string(), "31".to_string()),
        ];
        let raw = Raw::from_labels(CARD, "Visa", &values).unwrap();
        let mut item = Item {
            id: String::new(),
            revision: 0,
            sites: vec![],
            other_top_sites: vec![],
            raw,
        };
        item.validate().unwrap();
        assert_eq!(
            super::super::classify::read(&item.raw, "expiry").as_deref(),
            Some("2031-04")
        );
        assert_eq!(
            super::super::classify::read(&item.raw, "code").as_deref(),
            Some("737")
        );
        assert!(Raw::from_labels(CARD, "Visa", &[("ssn".into(), "x".into())]).is_err());
        let mut edited = item.raw.clone();
        edited.set("cvv", "").unwrap();
        edited.set("cardholder name", "A Lovelace").unwrap();
        assert_eq!(edited.get("code"), None);
        assert_eq!(edited.get("cardholder name").as_deref(), Some("A Lovelace"));
        assert_eq!(edited.get("number").as_deref(), Some("4111111111111111"));
        let mut siteless = Item {
            raw: Raw::from_labels(LOGIN, "Mail", &[]).unwrap(),
            ..item.clone()
        };
        assert!(
            siteless
                .validate()
                .unwrap_err()
                .message
                .contains("its site")
        );
    }
}
