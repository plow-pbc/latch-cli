//! `latch google`: run one allowed gog command on a host, with the owner's
//! Google token, and bring back its JSON and any files it wrote.
//!
//! - `client.rs`: the agent's side; sends the request and any files gog reads.
//! - `host.rs`: the host's side; checks the request, picks accounts, starts workers.
//! - `process.rs`: inside the sandboxed worker; runs the pinned gog binary.
//! - `policy.rs`: the grammar that decides which commands and flags are allowed.
//! - `merge.rs`: combines one read's results from several accounts.
//! - `parity.rs` (tests): the same argv through latch and plow-gog, the
//!   planner of the old Latch app and Plow's Google route.

pub mod client;
pub mod host;
pub mod merge;
#[cfg(test)]
mod parity;
pub mod policy;
pub mod process;

use std::sync::LazyLock;

use crate::failure::{Exit, Failure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most gog JSON one call may return. Larger output fails the call
/// rather than being cut, because a cut read looks complete.
pub const MAX_OUTPUT: usize = 1024 * 1024;
/// The reviewed gog release, from `pin.json`: its version and the sha256 of
/// its binary for each OS and architecture. install.sh reads the same file.
#[derive(Deserialize)]
pub struct Pin {
    pub version: String,
    pub hashes: Vec<String>,
}
pub static PIN: LazyLock<Pin> =
    LazyLock::new(|| serde_json::from_str(include_str!("pin.json")).expect("gog pin"));

/// A connected Google account and its access token. `None` when Plow could
/// not mint one; a call on that account then fails as `needs_reauth`.
// Credentials deliberately have no Debug or Serialize implementation.
#[derive(Deserialize)]
pub struct Account {
    pub account: String,
    pub access_token: Option<String>,
    /// What the owner let Plow read on this account. A Plow that does not
    /// say grants everything, as before.
    #[serde(default)]
    pub capabilities: Access,
}

/// The reads the owner granted on one account, from its Google scopes:
/// an account connected for Gmail only cannot read its calendar.
#[derive(Deserialize, Clone, Copy)]
pub struct Access {
    #[serde(default = "granted")]
    pub mail_read: bool,
    #[serde(default = "granted")]
    pub calendar_read: bool,
}

impl Default for Access {
    fn default() -> Self {
        Access {
            mail_read: true,
            calendar_read: true,
        }
    }
}

fn granted() -> bool {
    true
}

/// Every connected account, as Plow's token answer lists them.
#[derive(Deserialize)]
#[serde(from = "Mint")]
pub struct Tokens {
    pub accounts: Vec<Account>,
}

/// Plow's answer as sent: accounts with a token, and `degraded` accounts
/// without one. `#[serde(from = "Mint")]` above reads this shape and then
/// converts it, so the rest of the code sees one list.
#[derive(Deserialize)]
struct Mint {
    accounts: Vec<Account>,
    #[serde(default)]
    degraded: Vec<Unavailable>,
}
#[derive(Deserialize)]
struct Unavailable {
    account: String,
}
impl From<Mint> for Tokens {
    fn from(mint: Mint) -> Self {
        let mut accounts = mint.accounts;
        accounts.extend(mint.degraded.into_iter().map(|entry| Account {
            account: entry.account,
            access_token: None,
            capabilities: Access::default(),
        }));
        Self { accounts }
    }
}

/// A client file sent with the request: which gog argument it fills, how
/// many bytes follow, and its file name (never its client path).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub index: usize,
    pub size: u64,
    pub name: String,
}
/// The `google` operation's arguments, as the client sends them.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub gog_args: Vec<String>,
    pub account: Vec<String>,
    pub confirm_conflict: bool,
    pub inputs: Vec<Input>,
}
/// A file gog wrote, as reported to the client: never its contents.
#[derive(Serialize, Deserialize, Clone)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
/// What the worker reports after gog exits: gog's JSON and the files it wrote.
#[derive(Deserialize, Serialize)]
pub struct Output {
    pub result: Value,
    pub files: Vec<File>,
}

/// A failure whose message is its code in words, with machine-readable
/// `details` for the agent.
pub fn problem(exit: Exit, code: &str, details: Value) -> Failure {
    let mut failure = Failure::new(exit, code, code.replace('_', " "));
    failure.details = details;
    failure
}
/// gog exited with `code`: a `google_failed` failure with a fixed reason from
/// gog's published exit codes. gog's own error text is never passed on.
pub fn gog_failure(code: i32) -> Failure {
    let reason = match code {
        4 => "needs_reauth",
        6 => "permission_denied",
        7 => "rate_limited",
        2 => "invalid_request",
        5 => "not_found",
        8 => "retryable",
        _ => "operation_failed",
    };
    problem(
        Exit::Failed,
        "google_failed",
        json!({ "gog_exit_code": code, "reason": reason }),
    )
}
