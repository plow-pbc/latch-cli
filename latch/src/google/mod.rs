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
#[cfg(feature = "fake-gog")]
pub mod fake;
pub mod host;
pub mod merge;
#[cfg(test)]
mod parity;
pub mod policy;
pub mod process;

use std::sync::LazyLock;

use crate::failure::{Code, Failure, code};
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
/// not mint one: a call on that account then fails, as
/// `google_needs_sign_in` when that is why.
// Credentials deliberately have no Debug or Serialize implementation.
#[derive(Deserialize)]
pub struct Account {
    pub account: String,
    pub access_token: Option<String>,
    /// What the owner let Plow read on this account.
    pub capabilities: Access,
    /// Google wants the owner to sign in with this account again.
    #[serde(default)]
    pub needs_sign_in: bool,
}

/// The reads the owner granted on one account, from its Google scopes:
/// an account connected for Gmail only cannot read its calendar.
#[derive(Deserialize, Clone, Copy)]
pub struct Access {
    pub mail_read: bool,
    pub calendar_read: bool,
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
    #[serde(default)]
    reason: String,
}
impl From<Mint> for Tokens {
    fn from(mint: Mint) -> Self {
        let mut accounts = mint.accounts;
        accounts.extend(mint.degraded.into_iter().map(|entry| Account {
            account: entry.account,
            access_token: None,
            // Plow sends no grants for an account it could not mint for.
            // Its calls fail, saying why, so it is never skipped.
            capabilities: Access {
                mail_read: true,
                calendar_read: true,
            },
            needs_sign_in: entry.reason == "needs_reauth",
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
pub fn problem(code: Code, details: Value) -> Failure {
    let mut failure = Failure::new(code, code.name.replace('_', " "));
    failure.details = details;
    failure
}
impl Tokens {
    /// The accounts Google wants the owner to sign in with again.
    pub fn needs_sign_in(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter(|account| account.needs_sign_in)
            .map(|account| account.account.clone())
            .collect()
    }
}

/// Google wants the owner to sign in again with `accounts`, and no
/// connected account works: `latch plugins enable google` signs them in.
pub fn needs_sign_in(accounts: Vec<String>) -> Failure {
    let mut failure = sign_in_again(accounts);
    failure.next = Some("latch plugins enable google".into());
    failure
}

/// Plow has no Google account connected: `latch plugins enable google`
/// signs one in.
pub fn not_connected() -> Failure {
    let mut failure = Failure::new(
        code::google_not_connected,
        "No Google account is connected in Plow.".into(),
    );
    failure.next = Some("latch plugins enable google".into());
    failure
}

/// Google wants the owner to sign in again with `accounts`, which a call
/// needed, while other accounts may still work: then `plugins enable
/// google` succeeds without a sign-in, so the owner reconnects these in
/// Plow. One code for a call on such an account and gog's own auth error.
pub fn needs_sign_in_on(accounts: Vec<String>) -> Failure {
    let mut failure = sign_in_again(accounts);
    failure.next = Some("reconnect these accounts on Plow's Connections page".into());
    failure
}

fn sign_in_again(accounts: Vec<String>) -> Failure {
    let named = if accounts.is_empty() {
        "a Google account".to_string()
    } else {
        accounts.join(", ")
    };
    let mut failure = Failure::new(
        code::google_needs_sign_in,
        format!("Google needs the owner to sign in again with {named}."),
    );
    failure.details = json!({"accounts": accounts});
    failure
}

/// The `accounts` a `google_needs_sign_in` failure's details (or Plow's
/// `needs_reauth` refusal) name.
pub fn sign_in_accounts(details: &Value) -> Vec<String> {
    details["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|account| account.as_str().map(String::from))
        .collect()
}

/// gog exited with `code` for `account`: a `google_failed` failure with a
/// fixed reason from gog's published exit codes, or for its auth error
/// (exit 4) `google_needs_sign_in`. gog's own error text is never passed on.
pub fn gog_failure(code: i32, account: &str) -> Failure {
    let reason = match code {
        4 => return needs_sign_in_on(vec![account.to_string()]),
        6 => "permission_denied",
        7 => "rate_limited",
        2 => "invalid_request",
        5 => "not_found",
        8 => "retryable",
        _ => "operation_failed",
    };
    problem(
        code::google_failed,
        json!({ "gog_exit_code": code, "reason": reason }),
    )
}
