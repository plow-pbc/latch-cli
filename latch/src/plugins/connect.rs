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

use std::cell::RefCell;
use std::future::Future;
use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::failure::{Failure, code};
use crate::plow::{self, ConnectLink, Plow};
use crate::state::StateDir;
use crate::term;

/// How often Plow is asked whether the account has appeared, and for how
/// long in all, as the Mac app before latch did.
const POLL: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(5 * 60);
/// How long the look at the deadline may take.
const LAST_LOOK: Duration = Duration::from_secs(3);

const ENABLE: &str = "latch plugins enable google";

/// `refused` is a plain enable's `google_not_connected`; `enabled` is the
/// result of one that succeeded before `--add-account` adds an account.
/// `enable` runs the enable again once a new account is there.
pub(super) async fn google<F, Fut>(
    state: &StateDir,
    refused: Option<Failure>,
    enabled: Option<Value>,
    events: bool,
    enable: F,
) -> Result<Value, Failure>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, Failure>>,
{
    let deadline = Instant::now() + give_up_after();
    let at_terminal = !events && term::interactive();
    let adding = enabled.is_some();
    // The public page, from the host's refusal until Plow says it again.
    let mut page = refused
        .as_ref()
        .and_then(|refused| refused.details["connect_url"].as_str())
        .map(String::from);
    let plow = Plow::from_state(state).map_err(|failure| with_page(failure, &page))?;
    // The accounts Plow had before, once it said.
    let before: RefCell<Option<Vec<String>>> = RefCell::new(None);
    // Everything up to an account appearing, Plow's answers included,
    // within the one deadline.
    let connecting = async {
        let connectors = plow.connectors().await?;
        page = Some(connectors.connections_page());
        let had = accounts(&connectors);
        *before.borrow_mut() = Some(had.clone());
        let mut link = plow.google_connect_link().await?;
        if !events && !at_terminal {
            return Ok(Connecting::HandedOver(link));
        }
        if !offer(&link, events, at_terminal, adding, false) {
            return Ok(Connecting::Cancelled);
        }
        loop {
            tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
            if events && stdout_gone() {
                return Ok(Connecting::Cancelled);
            }
            // A mint or a look that fails ends the wait with its own failure.
            if appeared(&plow.connectors().await?, &had) {
                return Ok(Connecting::Appeared);
            }
            if expired(&link) {
                link = plow.google_connect_link().await?;
                if !offer(&link, events, at_terminal, adding, true) {
                    return Ok(Connecting::Cancelled);
                }
            }
        }
    };
    let bounded = async {
        match tokio::time::timeout_at(deadline.into(), connecting).await {
            Ok(connected) => connected,
            // The deadline: one last look, so a sign-in finished in its
            // final second still counts; nothing is renewed or waited for.
            Err(_) => {
                let Some(had) = before.borrow().clone() else {
                    return Err(timed_out());
                };
                match tokio::time::timeout(LAST_LOOK, plow.connectors()).await {
                    Ok(Ok(now)) if appeared(&now, &had) => Ok(Connecting::Appeared),
                    _ => Err(timed_out()),
                }
            }
        }
    };
    // Only a wait can be stopped: a pipe's answer comes at once.
    let waits = events || at_terminal;
    let connected = if waits {
        tokio::select! {
            connected = bounded => connected,
            () = crate::remote::stop_requested() => Ok(Connecting::Cancelled),
        }
    } else {
        bounded.await
    };
    match connected.map_err(|failure| with_page(failure, &page))? {
        Connecting::Cancelled => Err(cancelled(adding)),
        // Nobody reads the result any more: no enable on their behalf.
        Connecting::Appeared if events && stdout_gone() => Err(cancelled(adding)),
        // Once the enable is sent, a stop can no longer say what it did.
        Connecting::Appeared => tokio::select! {
            enabled = enable() => enabled,
            () = crate::remote::stop_requested() => Err(unknown()),
        },
        // No one here to wait with: the link is the answer.
        Connecting::HandedOver(link) => match enabled {
            Some(mut result) => {
                result["sign_in_url"] = json!(link.url);
                result["expires_at"] = json!(link.expires_at);
                Ok(result)
            }
            None => {
                let mut failure = Failure::new(
                    code::google_not_connected,
                    "No Google account is connected in Plow.".into(),
                );
                failure.next =
                    Some(format!("open sign_in_url, finish Google sign-in, then {ENABLE}").into());
                failure.details = json!({
                    "sign_in_url": link.url,
                    "expires_at": link.expires_at,
                    "connect_url": page,
                });
                Err(failure)
            }
        },
    }
}

/// How the connecting phase ended.
enum Connecting {
    /// An account Plow did not have before is there.
    Appeared,
    /// The caller asked the wait to stop, or stopped reading.
    Cancelled,
    /// No one waits here: this link is the answer.
    HandedOver(ConnectLink),
}

/// Whether Plow now has an account it did not have before.
fn appeared(now: &plow::Connectors, had: &[String]) -> bool {
    accounts(now).iter().any(|account| !had.contains(account))
}

fn timed_out() -> Failure {
    let mut timeout = Failure::new(
        code::sign_in_timeout,
        "No new Google account was observed before the wait ended.".into(),
    );
    timeout.next = Some("latch setup --status".into());
    timeout
}

/// Stopped before the final enable was sent: the wait turned nothing on.
/// With `--add-account`, the enable before the wait may already have turned
/// Google on for this host. A sign-in already started in a browser is left
/// to finish or not.
fn cancelled(adding: bool) -> Failure {
    let message = if adding {
        "Stopped waiting for Google sign-in. Google may already be on for this host, from the \
         enable before the wait."
    } else {
        "Stopped waiting for Google sign-in."
    };
    let mut cancelled = Failure::new(code::google_connect_cancelled, message.into());
    let again = if adding { " --add-account" } else { "" };
    cancelled.next = Some(format!("{ENABLE}{again}").into());
    cancelled
}

/// Stopped after the enable was sent: whether it took is not known.
fn unknown() -> Failure {
    let mut unknown = Failure::new(
        code::outcome_unknown,
        "Stopped after Google's enable was sent; whether it is on for this host is unknown.".into(),
    );
    unknown.next = Some("latch plugins list".into());
    unknown
}

/// `failure`, with the public Connections page as `connect_url` when known.
fn with_page(mut failure: Failure, page: &Option<String>) -> Failure {
    if let Some(page) = page {
        failure.details["connect_url"] = json!(page);
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

/// Whether stdout is a pipe or socket whose reader has gone: the kernel
/// says so without a write (POLLHUP on macOS, POLLERR on Linux).
fn stdout_gone() -> bool {
    let mut out = libc::pollfd {
        fd: libc::STDOUT_FILENO,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one pollfd, writable for the call; a zero timeout never blocks.
    let ready = unsafe { libc::poll(&mut out, 1, 0) };
    ready > 0 && out.revents & (libc::POLLERR | libc::POLLHUP) != 0
}

/// Tests shorten the wait (`LATCH_TEST_CONNECT_WITHIN`, in seconds).
fn give_up_after() -> Duration {
    std::env::var("LATCH_TEST_CONNECT_WITHIN")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(GIVE_UP_AFTER, Duration::from_secs)
}
