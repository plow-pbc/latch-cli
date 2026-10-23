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

/// `refused` is a plain enable's `google_not_connected`, or its
/// `google_needs_sign_in` naming the accounts to sign in with again;
/// `enabled` is the result of one that succeeded before `--add-account`
/// adds an account. `enable` runs the enable again once a new account is
/// there, or every named one works again.
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
    let again: Option<Vec<String>> = refused
        .as_ref()
        .filter(|refused| refused.code == code::google_needs_sign_in.name)
        .map(|refused| crate::google::sign_in_accounts(&refused.details));
    // The public page, from the host's refusal until Plow says it again.
    let mut page = refused
        .as_ref()
        .and_then(|refused| refused.details["connect_url"].as_str())
        .map(String::from);
    let plow = Plow::from_state(state).map_err(|failure| with_page(failure, &page))?;
    // What ends the wait, once Plow said which accounts it had.
    let goal: RefCell<Option<Goal>> = RefCell::new(None);
    // Everything up to an account appearing, Plow's answers included,
    // within the one deadline.
    let connecting = async {
        let connectors = plow.connectors().await?;
        page = Some(connectors.connections_page());
        let wanted = match &again {
            Some(accounts) => Goal::Working(accounts.clone()),
            None => Goal::New(accounts(&connectors)),
        };
        *goal.borrow_mut() = Some(wanted.clone());
        let mut link = plow.google_connect_link().await?;
        if !events && !at_terminal {
            return Ok(Connecting::HandedOver(link));
        }
        let offer = |link: &ConnectLink, renewed| {
            offer(link, events, at_terminal, adding, again.as_deref(), renewed)
        };
        if !offer(&link, false) {
            return Ok(Connecting::Cancelled);
        }
        loop {
            tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
            // A mint or a look that fails ends the wait with its own failure.
            if reached(&plow, &wanted).await? {
                return Ok(Connecting::Appeared);
            }
            if expired(&link) {
                link = plow.google_connect_link().await?;
                if !offer(&link, true) {
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
                let Some(wanted) = goal.borrow().clone() else {
                    return Err(timed_out());
                };
                match tokio::time::timeout(LAST_LOOK, reached(&plow, &wanted)).await {
                    Ok(Ok(true)) => Ok(Connecting::Appeared),
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
            // Heard even while Plow is still answering a look or a link.
            () = crate::remote::stop_requested() => Ok(Connecting::Cancelled),
            () = reader_gone() => Ok(Connecting::Cancelled),
        }
    } else {
        bounded.await
    };
    match connected.map_err(|failure| with_page(failure, &page))? {
        Connecting::Cancelled => Err(cancelled(adding)),
        // Nobody reads the result any more: no enable on their behalf.
        Connecting::Appeared if stdout_gone() => Err(cancelled(adding)),
        // Once the enable is sent, a stop can no longer say what it did.
        Connecting::Appeared => tokio::select! {
            enabled = enable() => enabled,
            () = crate::remote::stop_requested() => Err(unknown()),
            () = reader_gone() => Err(unknown()),
        },
        // No one here to wait with: the link is the answer.
        Connecting::HandedOver(link) => match enabled {
            Some(mut result) => {
                result["sign_in_url"] = json!(link.url);
                result["expires_at"] = json!(link.expires_at);
                Ok(result)
            }
            None => {
                let mut failure = match &again {
                    Some(accounts) => crate::google::needs_sign_in(accounts.clone()),
                    None => Failure::new(
                        code::google_not_connected,
                        "No Google account is connected in Plow.".into(),
                    ),
                };
                failure.next =
                    Some(format!("open sign_in_url, finish Google sign-in, then {ENABLE}").into());
                failure.details["sign_in_url"] = json!(link.url);
                failure.details["expires_at"] = json!(link.expires_at);
                failure.details["connect_url"] = json!(page);
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

/// What a sign-in is waited for until.
#[derive(Clone)]
enum Goal {
    /// An account Plow did not have: these are the ones it had.
    New(Vec<String>),
    /// Each of these accounts working again, Google having asked to sign
    /// in with them again.
    Working(Vec<String>),
}

/// Whether Plow says the sign-in waited for has happened. Accounts to sign
/// in with again are known to work only by getting their credentials.
async fn reached(plow: &Plow, goal: &Goal) -> Result<bool, Failure> {
    match goal {
        Goal::New(had) => Ok(accounts(&plow.connectors().await?)
            .iter()
            .any(|account| !had.contains(account))),
        Goal::Working(wanted) => match plow.google_tokens().await {
            Ok(tokens) => Ok(wanted.iter().all(|wanted| {
                tokens
                    .accounts
                    .iter()
                    .any(|account| account.account == *wanted && account.access_token.is_some())
            })),
            Err(failure) if failure.code == code::google_needs_sign_in.name => Ok(false),
            Err(failure) => Err(failure),
        },
    }
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
fn offer(
    link: &ConnectLink,
    events: bool,
    at_terminal: bool,
    adding: bool,
    again: Option<&[String]>,
    renewed: bool,
) -> bool {
    if events {
        let line = json!({"type": "link", "url": link.url, "expires_at": link.expires_at});
        let mut out = std::io::stdout().lock();
        return writeln!(out, "{line}").and_then(|()| out.flush()).is_ok();
    }
    if at_terminal {
        if renewed {
            eprintln!("That link expired. If you haven't opened a link yet, use this new one.");
        } else if let Some(accounts) = again {
            eprintln!(
                "Google needs you to sign in again with {}.",
                accounts.join(", ")
            );
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

/// Finishes once stdout's reader has gone, with `--events` or at a
/// terminal alike (whose result goes there too); never for a terminal or a
/// file on stdout. The kernel has no wake-up for it, so it is looked at
/// often.
async fn reader_gone() {
    while !stdout_gone() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
