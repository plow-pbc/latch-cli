//! Signing in with Google from `latch plugins enable google`. When Plow has
//! no Google account (`google_not_connected`), or the owner asks to add
//! another (`--add-account`), this command, never the host, asks Plow for
//! a single-use sign-in link and offers it. A person at a terminal gets the
//! link and a QR code; an app (`--events`) gets it as a `link` line. Either
//! way the command waits, up to five minutes, for an account Plow did not
//! have before, renewing the link when it expires unopened, then runs the
//! enable again. Anything else gets the link in its answer and no wait.
//!
//! The link is a credential until it is used or expires. It appears only in
//! the caller's own output (`sign_in_url`, a `link` line, the terminal's
//! link and QR code): never in a message, the host's arguments or answers,
//! saved state, the audit or a log. `connect_url` stays Plow's public
//! Connections page.

use std::future::Future;
use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::failure::Failure;
use crate::plow::{self, ConnectLink, Plow};
use crate::state::StateDir;
use crate::term;

/// How often Plow is asked whether the account has appeared, and for how
/// long in all, as the Mac app before latch did.
const POLL: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(5 * 60);

const ENABLE: &str = "latch plugins enable google";

/// `refused` is a plain enable's `google_not_connected`; `enabled` is the
/// result of one that succeeded before `--add-account` adds an account.
/// `enable` runs the enable again once a new account is there. `Ok(None)`:
/// the caller asked the wait to stop.
pub(super) async fn google<F, Fut>(
    state: &StateDir,
    refused: Option<Failure>,
    enabled: Option<Value>,
    events: bool,
    enable: F,
) -> Result<Option<Value>, Failure>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, Failure>>,
{
    let deadline = Instant::now() + give_up_after();
    // The public page, from the host's refusal until Plow says it again.
    let mut page = refused
        .as_ref()
        .and_then(|refused| refused.details["connect_url"].as_str())
        .map(String::from);
    let plow = Plow::from_state(state).map_err(|failure| with_page(failure, &page))?;
    let connectors = plow
        .connectors()
        .await
        .map_err(|failure| with_page(failure, &page))?;
    page = Some(connectors.connections_page());
    let before = accounts(&connectors);
    let mut link = plow
        .google_connect_link()
        .await
        .map_err(|failure| with_page(failure, &page))?;

    let at_terminal = !events && term::interactive();
    if !events && !at_terminal {
        // No one here to wait with: the link is the answer.
        let handoff = json!({"sign_in_url": link.url, "expires_at": link.expires_at});
        return match enabled {
            Some(mut result) => {
                merge(&mut result, handoff);
                Ok(Some(result))
            }
            None => {
                let mut failure = Failure::failed(
                    "google_not_connected",
                    "No Google account is connected in Plow.".into(),
                );
                failure.details = handoff;
                merge(
                    &mut failure.details,
                    json!({"next": format!("Open sign_in_url, finish Google sign-in, then run {ENABLE}."),
                           "connect_url": page}),
                );
                Err(failure)
            }
        };
    }

    let adding = enabled.is_some();
    if !offer(&link, events, at_terminal, adding, false) {
        return Ok(None);
    }
    let waited = async {
        loop {
            tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
            // A mint or a look that fails ends the wait with its own failure.
            let now = plow.connectors().await?;
            if accounts(&now)
                .iter()
                .any(|account| !before.contains(account))
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                let mut timeout = Failure::failed(
                    "sign_in_timeout",
                    "No new Google account was observed before the wait ended.".into(),
                );
                timeout.details = json!({"next": "latch setup --status"});
                return Err(timeout);
            }
            if expired(&link) {
                link = plow.google_connect_link().await?;
                if !offer(&link, events, at_terminal, adding, true) {
                    return Ok(false);
                }
            }
        }
    };
    let appeared = tokio::select! {
        appeared = waited => appeared.map_err(|failure| with_page(failure, &page))?,
        () = crate::remote::stop_requested() => false,
    };
    if !appeared {
        return Ok(None);
    }
    enable().await.map(Some)
}

/// `failure`, with the public Connections page as `connect_url` when known.
fn with_page(mut failure: Failure, page: &Option<String>) -> Failure {
    if let Some(page) = page {
        merge(&mut failure.details, json!({"connect_url": page}));
    }
    failure
}

/// Shows `link`: a `link` line, or at a terminal the link and its QR code.
/// False once nobody reads the lines.
fn offer(link: &ConnectLink, events: bool, at_terminal: bool, adding: bool, renewed: bool) -> bool {
    if events {
        let line = json!({"type": "link", "url": link.url, "expires_at": link.expires_at});
        let mut out = std::io::stdout().lock();
        return writeln!(out, "{line}").and_then(|()| out.flush()).is_ok();
    }
    if at_terminal {
        if renewed {
            eprintln!("That link expired. If you haven't opened a link yet, use this new one.");
        } else if adding {
            eprintln!("Sign in with another Google account to add it to Plow.");
        } else {
            eprintln!("Sign in with Google to connect it to Plow.");
        }
        for line in term::qr(&link.url).lines() {
            eprintln!("{}", term::link(&link.url, line));
        }
        eprintln!("{}", term::link(&link.url, &link.url));
        if !renewed {
            eprintln!("Waiting for the account, up to 5 minutes… (Ctrl-C stops waiting)");
        }
    }
    true
}

/// Whether `link` can no longer start a sign-in. A link whose expiry Plow
/// wrote unreadably is kept for the whole wait.
fn expired(link: &ConnectLink) -> bool {
    link.expires_at
        .parse::<jiff::Timestamp>()
        .is_ok_and(|at| at <= jiff::Timestamp::now())
}

fn accounts(connectors: &plow::Connectors) -> Vec<String> {
    connectors
        .gmail
        .accounts
        .iter()
        .map(|account| account.account.clone())
        .collect()
}

fn merge(into: &mut Value, fields: Value) {
    if into.is_null() {
        *into = json!({});
    }
    if let (Some(into), Value::Object(fields)) = (into.as_object_mut(), fields) {
        into.extend(fields);
    }
}

/// Tests shorten the wait (`LATCH_TEST_CONNECT_WITHIN`, in seconds).
fn give_up_after() -> Duration {
    std::env::var("LATCH_TEST_CONNECT_WITHIN")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(GIVE_UP_AFTER, Duration::from_secs)
}
