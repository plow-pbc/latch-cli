//! What the vault's hosts agree on: every item under a `uid` that names it
//! on each of them, when it last changed, and what was removed. The local
//! `vN` ids and revisions stay each host's own; sync keys on the uid.
//!
//! `merge` combines two hosts' states, uid by uid: the later change to an
//! item wins, and a removal as late as the edit wins. Items under
//! different uids stay separate, even with the same values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::item::Item;

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

/// `a` and `b` as one state, the same whichever comes first. Items come
/// out ordered by uid, each carrying the id and revision of the side it
/// came from; giving a foreign item a local id is the applying host's job.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "host-to-host sync (the next change) calls it")
)]
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
    State {
        items: items.into_values().cloned().collect(),
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
    use crate::vault::item::{LOGIN, Login, Raw};

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
    fn synced(state: &State) -> State {
        let mut state = state.clone();
        for s in &mut state.items {
            s.item.id.clear();
            s.item.revision = 0;
        }
        state
    }

    #[test]
    fn merging_is_the_same_either_way_the_latest_write_wins_and_removals_stick() {
        let a1 = || login("a1", 0, "example.com", "made-up-pass-1");
        type Case<'a> = (
            &'a str,
            State,
            State,
            Vec<(&'a str, &'a str)>,
            Vec<(&'a str, u64)>,
        );
        let cases: Vec<Case> = vec![
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
                "the same values under two uids stay two items",
                state(vec![a1()], vec![]),
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
    fn a_removal_holds_however_three_hosts_states_are_grouped() {
        let a = state(vec![login("a", 0, "example.com", "made-up-pass-1")], vec![]);
        let b = state(vec![login("b", 0, "example.com", "made-up-pass-1")], vec![]);
        let removed_a = state(vec![], vec![gone("a", 10)]);
        let left = merge(&merge(&a, &b), &removed_a);
        let right = merge(&a, &merge(&b, &removed_a));
        assert_eq!(synced(&left), synced(&right));
        let uids: Vec<&str> = left.items.iter().map(|s| s.uid.as_str()).collect();
        assert_eq!(uids, ["b"]);
    }
}
