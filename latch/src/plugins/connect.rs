//! Connecting Google from `latch plugins enable google`: when Plow has no
//! Google account this host can use, the command asks Plow for a single-use
//! sign-in link and offers it. A person at a terminal gets the link and a
//! QR code, and the command waits for the account, then enables Google. An
//! app (`--events`) gets the link as a line first and the result once the
//! account appears. Anything else fails as before, the link in the error.
//! With Google already connected, `--add-account` offers the same link to
//! add another account; without a terminal or `--events`, the link is the
//! answer.
//!
//! The link carries a credential until it is used or expires, so it goes to
//! the person or the app only: never into the audit, a log, or a message.

use std::future::Future;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::failure::{Exit, Failure};
use crate::plow::{self, Plow};
use crate::state::StateDir;
use crate::{login, term};

/// How often Plow is asked whether the account has appeared, and for how
/// long, as the Mac app before latch did.
const POLL: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(5 * 60);

const AGAIN: &str = "latch plugins enable google";

/// `refused` is the enable's `google_not_connected`, or none when the
/// owner asked to add another account (`--add-account`); `enable` runs the
/// enable (again). Without a sign-in link from Plow, `refused` stands as it
/// is, pointing at Plow's Connections page.
pub(super) async fn google<F, Fut>(
    state: &StateDir,
    refused: Option<Failure>,
    events: bool,
    enable: F,
) -> Result<Value, Failure>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, Failure>>,
{
    let adding = refused.is_none();
    let asked = async {
        let plow = Plow::from_state(state)?;
        let before = accounts(&plow.connectors().await?);
        let link = plow.google_connect_link().await?;
        Ok::<_, Failure>((plow, before, link))
    };
    let (plow, before, link) = match (asked.await, refused) {
        (Ok(asked), _) => asked,
        (Err(_), Some(refused)) => return Err(refused),
        (Err(failure), None) => return Err(failure),
    };
    if events {
        login::event(json!({"type": "connect", "connect_url": link.url,
                            "expires_at": link.expires_at}));
        login::event(json!({"type": "waiting"}));
    } else if term::interactive() {
        let what = if adding {
            "Sign in with another Google account to add it to Plow."
        } else {
            "Sign in with Google to connect it to Plow."
        };
        eprintln!("{what} {}", works_once(&link.expires_at));
        for line in term::qr(&link.url).lines() {
            eprintln!("{}", term::link(&link.url, line));
        }
        eprintln!("{}", term::link(&link.url, &link.url));
        eprintln!("Waiting for the account… (Ctrl-C stops waiting)");
    } else if adding {
        // What was asked for, done: the link, for the caller to open.
        return Ok(json!({"plugin": "google", "connect_url": link.url,
                         "expires_at": link.expires_at}));
    } else {
        let mut failure = Failure::failed(
            "google_not_connected",
            format!(
                "Plow has no Google account this host can use; open `connect_url` to sign in \
                 with Google before it expires, then run `{AGAIN}` again"
            ),
        );
        failure.details = json!({"connect_url": link.url, "expires_at": link.expires_at,
                                 "next": AGAIN});
        return Err(failure);
    }
    tokio::select! {
        appeared = new_account(&plow, &before) => appeared?,
        _ = tokio::signal::ctrl_c() => {
            let mut stopped = Failure::new(
                Exit::Unknown,
                "connect_stopped",
                format!("stopped waiting for Google; if you signed in, run `{AGAIN}` again"),
            );
            stopped.details = json!({"next": AGAIN});
            return Err(stopped);
        }
    }
    enable().await
}

fn accounts(connectors: &plow::Connectors) -> Vec<String> {
    connectors
        .gmail
        .accounts
        .iter()
        .map(|account| account.account.clone())
        .collect()
}

/// "It works once, for about 2 minutes." from Plow's `expires_at`.
fn works_once(expires_at: &str) -> String {
    let left = expires_at
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|at| at.duration_since(jiff::Timestamp::now()).as_secs().max(0));
    match left {
        Some(secs) if secs >= 90 => format!(
            "The link works once, for about {} minutes.",
            (secs + 30) / 60
        ),
        Some(secs) => format!("The link works once, for about {secs} seconds."),
        None => "The link works once.".to_string(),
    }
}

/// Waits for an account `before` doesn't have. A Plow that can't be
/// reached is asked again; one that refuses ends the wait.
async fn new_account(plow: &Plow, before: &[String]) -> Result<(), Failure> {
    let within = give_up_after();
    let started = Instant::now();
    loop {
        tokio::time::sleep(POLL.min(within.saturating_sub(started.elapsed()))).await;
        match plow.connectors().await {
            Ok(connectors) if accounts(&connectors).iter().any(|a| !before.contains(a)) => {
                return Ok(());
            }
            Ok(_) => {}
            Err(failure) if plow::refused(&failure) => return Err(failure),
            Err(_) => {}
        }
        if started.elapsed() >= within {
            let mut timeout = Failure::new(
                Exit::Unknown,
                "connect_timeout",
                format!(
                    "no Google account appeared in Plow within {}; if you're still signing \
                     in, finish, then run `{AGAIN}` again",
                    minutes(within)
                ),
            );
            timeout.details = json!({"next": AGAIN});
            return Err(timeout);
        }
    }
}

/// Tests shorten the wait (`LATCH_TEST_CONNECT_WITHIN`, in seconds).
fn give_up_after() -> Duration {
    std::env::var("LATCH_TEST_CONNECT_WITHIN")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(GIVE_UP_AFTER, Duration::from_secs)
}

fn minutes(wait: Duration) -> String {
    match wait.as_secs() {
        60 => "a minute".to_string(),
        secs if secs % 60 == 0 => format!("{} minutes", secs / 60),
        secs => format!("{secs} seconds"),
    }
}
