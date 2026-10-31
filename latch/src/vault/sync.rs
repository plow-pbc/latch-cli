//! What the vault's hosts agree on: every item under a `uid` that names it
//! on each of them, when it last changed, and what was removed. The local
//! `vN` ids and revisions stay each host's own; sync keys on the uid.
//!
//! `merge` combines two hosts' states: the later change to an item wins,
//! a removal as late as the edit wins, and on the first sync (items from
//! before sync, with no order yet) a login both hosts hold with the same
//! site, username and secrets becomes one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::item::{Item, LOGIN};
use super::totp::same_key;

/// An item and what sync orders it by.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Synced {
    /// Random, 128 bits in hex; the same on every host.
    pub uid: String,
    /// When it was last added or edited, in unix milliseconds, on the host
    /// that did it; 0 for an item from before sync, which has no order yet.
    pub modified: u64,
    pub item: Item,
}

/// A removed item, kept so the removal reaches every host and a host that
/// missed it cannot bring the item back. No title, no secret.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tombstone {
    pub uid: String,
    pub modified: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct State {
    pub items: Vec<Synced>,
    pub tombstones: Vec<Tombstone>,
}

pub fn new_uid() -> String {
    format!("{:032x}", rand::random::<u128>())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_millis() as u64
}

/// What every host holds of an item: it without this host's id and revision.
fn content(item: &Item) -> Vec<u8> {
    let mut item = item.clone();
    item.id.clear();
    item.revision = 0;
    serde_json::to_vec(&item).expect("items serialize")
}

/// Whether two items from before sync are one: a login by its first site,
/// username and secrets (password, authenticator key), anything else only
/// with every field the same.
fn same_item(a: &Item, b: &Item) -> bool {
    if a.raw.kind != b.raw.kind {
        return false;
    }
    if a.raw.kind != LOGIN {
        return a.raw == b.raw && a.sites == b.sites && a.other_top_sites == b.other_top_sites;
    }
    let (x, y) = (
        a.raw.login.clone().unwrap_or_default(),
        b.raw.login.clone().unwrap_or_default(),
    );
    a.sites.first() == b.sites.first()
        && x.username.unwrap_or_default() == y.username.unwrap_or_default()
        && x.password.unwrap_or_default() == y.password.unwrap_or_default()
        && same_key(&x.totp.unwrap_or_default(), &y.totp.unwrap_or_default())
}

/// `a` and `b` as one state, the same whichever comes first. Items come
/// out ordered by uid, each carrying the id and revision of the side it
/// came from; giving a foreign item a local id is the applying host's job.
pub fn merge(a: &State, b: &State) -> State {
    let mut items: BTreeMap<&str, &Synced> = BTreeMap::new();
    for s in a.items.iter().chain(&b.items) {
        let held = items.entry(&s.uid).or_insert(s);
        // The later edit; at the same time, the larger content, so both
        // hosts pick the same one.
        if (s.modified, content(&s.item)) > (held.modified, content(&held.item)) {
            *held = s;
        }
    }
    let mut tombstones: BTreeMap<&str, u64> = BTreeMap::new();
    for t in a.tombstones.iter().chain(&b.tombstones) {
        let held = tombstones.entry(&t.uid).or_insert(t.modified);
        *held = (*held).max(t.modified);
    }
    items.retain(|uid, s| tombstones.get(uid).is_none_or(|&at| at < s.modified));
    let mut kept: Vec<Synced> = Vec::new();
    for (uid, s) in items {
        let twin = s.modified == 0
            && kept
                .iter()
                .any(|k| k.modified == 0 && same_item(&k.item, &s.item));
        if twin {
            // The smaller uid stays; the other is removed everywhere.
            tombstones.insert(uid, 0);
        } else {
            kept.push(s.clone());
        }
    }
    State {
        items: kept,
        tombstones: tombstones
            .into_iter()
            .map(|(uid, modified)| Tombstone {
                uid: uid.to_string(),
                modified,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::item::{CARD, Login, Raw};

    fn login(uid: &str, modified: u64, site: &str, password: &str) -> Synced {
        Synced {
            uid: uid.into(),
            modified,
            item: Item {
                // Each host's own; merging never depends on them.
                id: format!("v{}", modified % 7),
                revision: modified % 5,
                sites: vec![site.into()],
                other_top_sites: vec![],
                raw: Raw {
                    kind: LOGIN,
                    name: format!("{site} {password}"),
                    login: Some(Login {
                        username: Some("lovelace".into()),
                        password: Some(password.into()),
                        ..Login::default()
                    }),
                    ..Raw::default()
                },
            },
        }
    }

    fn card(uid: &str, number: &str) -> Synced {
        let mut item = login(uid, 0, "example.com", "");
        item.item.raw =
            Raw::from_labels(CARD, "Visa", &[("number".into(), number.into())]).unwrap();
        item
    }

    fn gone(uid: &str, modified: u64) -> Tombstone {
        Tombstone {
            uid: uid.into(),
            modified,
        }
    }

    fn state(items: Vec<Synced>, tombstones: Vec<Tombstone>) -> State {
        State { items, tombstones }
    }

    /// What hosts agree on: uid, order and content, not local ids.
    fn synced(state: &State) -> (Vec<(String, u64, Vec<u8>)>, Vec<Tombstone>) {
        let items = state
            .items
            .iter()
            .map(|s| (s.uid.clone(), s.modified, content(&s.item)))
            .collect();
        (items, state.tombstones.clone())
    }

    #[test]
    fn merging_is_the_same_either_way_the_latest_write_wins_and_removals_stick() {
        let a1 = || login("a1", 0, "example.com", "made-up-pass-1");
        let cases: Vec<(&str, State, State, Vec<(&str, &str)>, Vec<(&str, u64)>)> = vec![
            (
                "the later edit wins",
                state(
                    vec![login("u", 10, "example.com", "made-up-pass-old")],
                    vec![],
                ),
                state(
                    vec![login("u", 20, "example.com", "made-up-pass-new")],
                    vec![],
                ),
                vec![("u", "made-up-pass-new")],
                vec![],
            ),
            (
                "edits at the same time settle on one",
                state(
                    vec![login("u", 10, "example.com", "made-up-pass-x")],
                    vec![],
                ),
                state(
                    vec![login("u", 10, "example.com", "made-up-pass-y")],
                    vec![],
                ),
                vec![("u", "made-up-pass-y")],
                vec![],
            ),
            (
                "a removal after the edit removes it",
                state(
                    vec![login("u", 10, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                state(vec![], vec![gone("u", 11)]),
                vec![],
                vec![("u", 11)],
            ),
            (
                "a removal at the same time as the edit removes it",
                state(
                    vec![login("u", 10, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                state(vec![], vec![gone("u", 10)]),
                vec![],
                vec![("u", 10)],
            ),
            (
                "an edit after the removal brings it back, the tombstone kept",
                state(
                    vec![login("u", 12, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                state(vec![], vec![gone("u", 11)]),
                vec![("u", "made-up-pass-1")],
                vec![("u", 11)],
            ),
            (
                "first sync: the same login on both hosts becomes one",
                state(vec![a1()], vec![]),
                state(
                    vec![login("b1", 0, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                vec![("a1", "made-up-pass-1")],
                vec![("b1", 0)],
            ),
            (
                "first sync: one login with two passwords stays two",
                state(vec![a1()], vec![]),
                state(
                    vec![login("b1", 0, "example.com", "made-up-pass-2")],
                    vec![],
                ),
                vec![("a1", "made-up-pass-1"), ("b1", "made-up-pass-2")],
                vec![],
            ),
            (
                "first sync: logins on different sites stay two",
                state(vec![a1()], vec![]),
                state(
                    vec![login("b1", 0, "example.org", "made-up-pass-1")],
                    vec![],
                ),
                vec![("a1", "made-up-pass-1"), ("b1", "made-up-pass-1")],
                vec![],
            ),
            (
                "after the first sync, the same values are two items",
                state(
                    vec![login("a1", 5, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                state(
                    vec![login("b1", 0, "example.com", "made-up-pass-1")],
                    vec![],
                ),
                vec![("a1", "made-up-pass-1"), ("b1", "made-up-pass-1")],
                vec![],
            ),
        ];
        for (name, a, b, items, tombstones) in cases {
            let merged = merge(&a, &b);
            assert_eq!(synced(&merged), synced(&merge(&b, &a)), "{name}: order");
            assert_eq!(
                synced(&merge(&merged, &merged)),
                synced(&merged),
                "{name}: again"
            );
            assert_eq!(
                synced(&merge(&merged, &a)),
                synced(&merged),
                "{name}: stale a"
            );
            assert_eq!(
                synced(&merge(&merged, &b)),
                synced(&merged),
                "{name}: stale b"
            );
            let got: Vec<(&str, &str)> = merged
                .items
                .iter()
                .map(|s| {
                    let login = s.item.raw.login.as_ref().unwrap();
                    (s.uid.as_str(), login.password.as_deref().unwrap())
                })
                .collect();
            assert_eq!(got, items, "{name}: items");
            let got: Vec<(&str, u64)> = merged
                .tombstones
                .iter()
                .map(|t| (t.uid.as_str(), t.modified))
                .collect();
            assert_eq!(got, tombstones, "{name}: tombstones");
        }
    }

    #[test]
    fn on_the_first_sync_cards_are_one_only_when_every_field_is_the_same() {
        for (other, merged) in [
            ("4111111111111111", vec!["a"]),
            ("4000056655665556", vec!["a", "b"]),
        ] {
            let a = state(vec![card("a", "4111111111111111")], vec![]);
            let b = state(vec![card("b", other)], vec![]);
            let uids = |s: &State| s.items.iter().map(|s| s.uid.clone()).collect::<Vec<_>>();
            assert_eq!(uids(&merge(&a, &b)), merged, "{other}");
            assert_eq!(uids(&merge(&b, &a)), merged, "{other}");
        }
    }
}
