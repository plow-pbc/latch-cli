//! The vault: one per latch install, on the host, shared by every engine
//! and session.
//!
//! - `item.rs`: items, in Bitwarden's shape, with latch's sites and
//!   revisions.
//! - `classify.rs`: which fields are secret (the Latch app's classifier).
//! - `sites.rs`: where each kind may fill.
//! - `dates.rs`: reshaping dates and expiries to a field's format.
//! - `totp.rs`: authenticator codes from a login's key.
//! - `store.rs`: the backend interface, and latch's own sealed store.
//! - `sync.rs`: stable ids, change times and tombstones, and merging two
//!   hosts' vault states.
//! - `import.rs`: other password managers' exports, read into items.
//! - `host.rs`: the host's `vault` operation, for owner devices.
//! - `link.rs`: phone links, which add an item from the owner's phone.
//! - `client.rs`: `latch vault …`, the owner's commands.

pub mod classify;
pub mod client;
pub mod dates;
pub mod host;
pub mod import;
pub mod item;
pub mod link;
pub(crate) mod local;
pub mod sites;
pub mod store;
pub mod sync;
pub mod totp;
