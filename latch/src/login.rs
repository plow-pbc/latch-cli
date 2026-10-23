//! `latch login`: create this peer's key if missing and register it with
//! Plow. Safe to repeat: the key is reused and Plow treats a repeat as a
//! refresh, so a second login changes nothing.
//!
//! The credential, in order: `--token-file` (saved as this machine's owner
//! token); the owner token saved here by an earlier login; the agent's own
//! credential; and, only with a person at the terminal, a login by text.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{self, BUILD_API_URL, LoginArgs};
use crate::credential;
use crate::devices;
use crate::failure::Failure;
use crate::heartbeat;
use crate::plow::{self, Peer, Plow, Registration};
use crate::state::{Config, StateDir};
use crate::term;

/// How often a login by text asks Plow whether the code has been texted:
/// every second for the first two minutes, then every five.
const POLL: Duration = Duration::from_secs(1);
const POLL_FAST_FOR: Duration = Duration::from_secs(2 * 60);
const POLL_SLOW: Duration = Duration::from_secs(5);
/// How long a login by text waits for the text before it gives up.
const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Serialize)]
pub struct LoginOutput {
    pub peer: Peer,
    pub key_created: bool,
    /// The person logged in by texting a code at a terminal: a human login,
    /// which ends in a line for them rather than JSON.
    #[serde(skip)]
    pub by_text: bool,
}

/// Where this login's credential comes from.
enum Source {
    /// `--token-file`: the owner's token, saved once Plow accepts it.
    Given(String),
    /// The owner's token an earlier login saved here.
    Saved(String),
    /// The agent's own credential, read where it lives.
    Agent(String),
    /// Nothing yet: a person at the terminal logs in by text.
    ByText,
}

pub async fn run(state: &StateDir, args: LoginArgs) -> Result<LoginOutput, Failure> {
    let saved = state.load_config()?;
    let saved_url = match &saved {
        Some(config) => Some(config.api_url.clone()),
        None => state.load_plow_url()?,
    };
    let api_url = api_url(args.api_url, credential::env, saved_url, cli::BUILD_CHANNEL);
    let Some(api_url) = api_url else {
        let message = "latch doesn't know which Plow to use: pass --api-url https://api.plow.co \
                       (or set $PLOW_API_BASE or $PLOW_API_URL)";
        return Err(Failure::usage("not_configured", message.to_string()));
    };
    let source = if let Some(path) = &args.token_file {
        Source::Given(credential::read(path)?)
    } else if let Some(token) = state.load_token()? {
        Source::Saved(token)
    } else if let Some(token) = credential::agent_token_if_present()? {
        Source::Agent(token)
    } else if args.events || term::interactive() {
        Source::ByText
    } else {
        // Never wait on a text nobody will see: a login by text is a
        // person's, at a terminal; a program's is `--events`.
        let message = "no Plow credential: pass --token-file FILE with the owner's Plow login, \
                       or run `latch login` at a terminal to log in by text"
            .to_string();
        let mut failure = Failure::usage("no_credential", message);
        failure.next = Some("latch login --token-file PATH, or latch login --events".into());
        return Err(failure);
    };
    let owner = !matches!(source, Source::Agent(_));
    let save_token = matches!(source, Source::Given(_));

    let named = args.name.is_some();
    let mut name = args
        .name
        .or(saved.as_ref().map(|config| config.peer.name.clone()));
    // A machine is named after its hostname, without asking.
    if owner && name.is_none() {
        name = hostname();
    }

    let texted = matches!(source, Source::ByText) && !args.events;
    let token = match source {
        Source::Given(token) | Source::Saved(token) | Source::Agent(token) => token,
        Source::ByText => {
            let Some(name) = &name else {
                return Err(missing("--name"));
            };
            by_text(state, &api_url, name, args.events).await?
        }
    };
    // Once a person has texted, what goes wrong is theirs to read.
    let registered = async {
        state.create()?;
        let (key, key_created) = state.load_or_create_key()?;
        // Login never starts a listener, so it never learns a new relay address:
        // it re-sends the one this peer last registered (a running host's
        // heartbeat keeps that current in config.json), so logging in again
        // never unlists a running host. A host first becomes reachable when
        // `latch serve` registers its relay.
        let relay_url = saved
            .as_ref()
            .and_then(|config| config.peer.relay_url.clone());
        let registration = Registration {
            key: key.public().to_string(),
            name,
            relay_url,
        };
        let plow = Plow::new(&api_url, &token);
        // An agent is named after itself in Plow. Plow names one that is not
        // an agent's credential, as it always has.
        let me = if owner {
            None
        } else {
            plow.agent_me().await.ok()
        };
        let registration = Registration {
            name: registration.name.or(me.as_ref().map(|me| me.name.clone())),
            ..registration
        };
        let peer = match plow.register(&registration).await {
            // Two agents may share a name: unless one was asked for, try once
            // more as the name plus the start of this agent's uid.
            Err(failure) if failure.code == "conflict" && !owner && !named => {
                let me = match me {
                    Some(me) => me,
                    None => plow.agent_me().await.map_err(|_| failure)?,
                };
                let name = registration.name.as_deref().unwrap_or(&me.name);
                let suffixed = Registration {
                    name: Some(format!("{name}-{}", &me.uid[..6.min(me.uid.len())])),
                    ..registration.clone()
                };
                plow.register(&suffixed).await?
            }
            // Another of the owner's machines may hold the name.
            Err(failure) if failure.code == "conflict" && owner => {
                if registration.name.is_none() {
                    return Err(failure);
                }
                let rerun = if save_token {
                    "latch login --token-file FILE"
                } else {
                    "latch login"
                };
                clash(&plow, registration, rerun).await?
            }
            result => result?,
        };

        // Save only after Plow accepted us, so a failed first login leaves no
        // half-configured instance behind (apart from the key, and the token of
        // a login by text, which is saved as soon as it arrives).
        if save_token {
            state.save_token(&token, &api_url)?;
        }
        state.save_config(&Config {
            api_url,
            peer: peer.clone(),
        })?;
        Ok(LoginOutput {
            peer,
            key_created,
            by_text: texted,
        })
    }
    .await;
    registered.map_err(|failure| if texted { failure.told() } else { failure })
}

/// Registers this peer again as it last was, when Plow no longer lists it:
/// an agent whose credential was rotated, say, which Plow forgets until it
/// registers with the new one. Never takes a name over.
pub async fn register_again(state: &StateDir) -> Result<(), Failure> {
    let config = state.require_login()?;
    let registration = Registration {
        key: state.load_key()?.public().to_string(),
        name: Some(config.peer.name.clone()),
        relay_url: config.peer.relay_url.clone(),
    };
    let peer = Plow::from_state(state)?.register(&registration).await?;
    state.save_config(&Config {
        api_url: config.api_url,
        peer,
    })
}

/// Logs in by text: Plow gives a code, the person texts it from their
/// phone, and the phone's account gets a new owner login for this machine.
/// The token is saved at once, so a login that fails after this point
/// (a name already taken, say) does not need another text. The code is
/// shown once and never logged. With `events`, what to text is a JSON line
/// for an app to show instead.
async fn by_text(
    state: &StateDir,
    api_url: &str,
    name: &str,
    events: bool,
) -> Result<String, Failure> {
    let plow = Plow::anonymous(api_url);
    let activation = plow.activate(name).await?;
    let code = &activation.display_code;
    let link = sms_link(&activation.send_to, code);
    if events {
        event(
            json!({"type": "code", "code": code, "send_to": activation.send_to,
                     "send_to_display": for_people(&activation.send_to),
                     "smsto": smsto(&activation.send_to, code), "sms_link": link,
                     "expires_at": activation.expires_at}),
        );
        event(json!({"type": "waiting"}));
    } else {
        // Clicking the QR code or the link under it opens Messages on a Mac,
        // in a terminal that shows links; elsewhere they are just drawn.
        for line in term::qr(&smsto(&activation.send_to, code)).lines() {
            eprintln!("{}", term::link(&link, line));
        }
        eprintln!("{}", term::link(&link, &link));
        eprintln!("Text {code} to {}", for_people(&activation.send_to));
        eprintln!("Waiting for your text…");
    }
    let waited = wait_for_text(state, api_url, &plow, &activation.activation_secret).await;
    match events {
        true => waited,
        // Once it has talked to the person, what goes wrong is theirs to read.
        false => waited.map_err(Failure::told),
    }
}

/// Asks Plow until the code was texted: the login's token, saved.
async fn wait_for_text(
    state: &StateDir,
    api_url: &str,
    plow: &Plow,
    secret: &str,
) -> Result<String, Failure> {
    let started = Instant::now();
    let mut failures = 0;
    loop {
        let poll = poll_every(started.elapsed());
        let wait = match plow.redeem(secret).await {
            Ok(answer) if answer.status == "verified" => {
                // Only the first `verified` answer carries the token.
                let Some(token) = answer.token else {
                    let message = "this login was already collected; run `latch login` again";
                    return Err(Failure::failed("login_collected", message.to_string()));
                };
                state.create()?;
                state.save_token(&token, api_url)?;
                return Ok(token);
            }
            Ok(_) => {
                failures = 0;
                poll
            }
            Err(failure) if failure.code == "expired" => {
                let message = "the code expired before it was texted; run `latch login` again";
                return Err(Failure::failed("code_expired", message.to_string()));
            }
            Err(failure) if failure.code == "not_found" => {
                let message = "Plow no longer knows this login; run `latch login` again";
                return Err(Failure::failed("login_not_found", message.to_string()));
            }
            // Asking again would get the same refusal.
            Err(failure) if plow::refused(&failure) => return Err(failure),
            // Plow unreachable, failing or asking us to slow down: wait
            // longer each time.
            Err(failure) => {
                failures += 1;
                heartbeat::backoff(failures, failure.retry_after).max(poll)
            }
        };
        let Some(wait) = before_giving_up(started.elapsed(), wait) else {
            let message = format!(
                "no text arrived within {} minutes; run `latch login` again",
                GIVE_UP_AFTER.as_secs() / 60
            );
            return Err(Failure::failed("code_expired", message));
        };
        tokio::time::sleep(wait).await;
    }
}

/// `wait`, cut short so the last ask comes when the login gives up; none
/// once it has, `waited` into the login.
fn before_giving_up(waited: Duration, wait: Duration) -> Option<Duration> {
    let left = GIVE_UP_AFTER.saturating_sub(waited);
    (!left.is_zero()).then(|| wait.min(left))
}

/// How long to wait between asks, `waited` into the login.
fn poll_every(waited: Duration) -> Duration {
    if waited < POLL_FAST_FOR {
        POLL
    } else {
        POLL_SLOW
    }
}

/// One line of `login --events`, printed at once.
pub fn event(line: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// What the QR code holds: a phone's camera opens its messaging app with
/// `text` (the code) ready to send to `send_to`. SMSTO takes the text as it is,
/// unencoded.
fn smsto(send_to: &str, text: &str) -> String {
    format!("SMSTO:{send_to}:{text}")
}

/// The same as a link, for Messages on a Mac, which reads `sms:` links.
fn sms_link(send_to: &str, text: &str) -> String {
    format!("sms:{send_to}?&body={}", escape(text))
}

/// A North American number as people write it, `+1 415-555-0100`; any
/// other number as Plow gave it.
fn for_people(number: &str) -> String {
    match number.strip_prefix("+1") {
        Some(digits) if digits.len() == 10 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            format!("+1 {}-{}-{}", &digits[..3], &digits[3..6], &digits[6..])
        }
        _ => number.to_string(),
    }
}

/// The name is taken by another peer. A person at the terminal may replace
/// another of the owner's machines (removed in Plow, which signs it out) or
/// pick a new name, and the login goes on with the login already in hand.
/// Anyone else is told what to run next. An agent's name is never taken
/// over.
async fn clash(plow: &Plow, mut registration: Registration, rerun: &str) -> Result<Peer, Failure> {
    let name = registration.name.clone().expect("a clash is over a name");
    let peers = plow.list_peers().await.unwrap_or_default();
    let machine = match peers.iter().find(|peer| peer.name == name) {
        Some(peer) if peer.kind == "agent" => {
            let message = format!("the name {name:?} belongs to an agent; choose another --name");
            return Err(Failure::usage("conflict", message));
        }
        Some(machine) if machine.key != registration.key => machine,
        _ => {
            let message = format!(
                "this key or the name {name:?} is taken by another peer; choose another --name"
            );
            return Err(Failure::usage("conflict", message));
        }
    };
    let last_seen = machine.last_seen_at.as_deref().unwrap_or("never");
    let taken = || {
        let message = format!(
            "a machine named {name:?} already exists (last seen {last_seen}); run \
             `{rerun} --name OTHER` to use another name, or `{rerun} --name {}` again at a \
             terminal to replace it (no second text needed)",
            shell_quoted(&name)
        );
        Failure::usage("conflict", message)
    };
    if !term::interactive() {
        return Err(taken());
    }
    if term::confirm(&format!(
        "A machine named {name:?} already exists (last seen {last_seen}). Replace it (this signs it out of Plow)?"
    )) {
        devices::remove_from_plow(plow, machine).await?;
        return plow.register(&registration).await;
    }
    loop {
        let other = term::ask("New name for this machine: ");
        if other.is_empty() {
            return Err(taken());
        }
        registration.name = Some(other.clone());
        match plow.register(&registration).await {
            Err(failure) if failure.code == "conflict" => eprintln!("{other:?} is taken too."),
            result => return result,
        }
    }
}

/// `word` as a shell reads it back: as it is when it holds only safe
/// characters, else in single quotes (a quote inside written `'\''`).
pub(crate) fn shell_quoted(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-._/:@%+=,".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// `text` percent-encoded for a URL: letters, digits and `-._~` stay as
/// they are, every other byte becomes `%XX`.
fn escape(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// This machine's hostname, up to its first dot (`mac-studio`, not
/// `mac-studio.local`), if it has one.
fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: std has no hostname API. `buffer` is writable for the supplied
    // length, and we search for the terminator without reading past that length.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let full = String::from_utf8_lossy(&buffer[..end]);
    let short = full.split('.').next().unwrap_or_default().trim();
    Some(short.to_string()).filter(|name| !name.is_empty())
}

/// Which Plow to log in to. The flag wins; then a cloud agent's
/// `$PLOW_API_BASE` (on exe.dev, the proxy that adds the agent's
/// credential), then `$PLOW_API_URL`, then this build's baked-in URL; on a
/// repeat login, the Plow the first one saved (with its config, or with its
/// token if it went no further); last, a release build's default.
fn api_url(
    flag: Option<String>,
    env: impl Fn(&str) -> Option<String>,
    saved: Option<String>,
    channel: Option<&str>,
) -> Option<String> {
    flag.or_else(|| env("PLOW_API_BASE"))
        .or_else(|| env("PLOW_API_URL"))
        .or(BUILD_API_URL.map(String::from))
        .or(saved)
        .or(cli::default_api_url(channel).map(String::from))
}

fn missing(what: &str) -> Failure {
    Failure::usage("not_configured", format!("first login needs {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_logs_in_to_plow_unless_something_names_another() {
        let release = Some("standalone tag v0.2.0");
        let none = |_: &str| None;
        let plow = Some("https://api.plow.co".to_string());
        assert_eq!(api_url(None, none, None, release), plow);
        // A local build has no default: it points at a dev stack.
        assert_eq!(api_url(None, none, None, None), None);

        let local = "http://127.0.0.1:8000".to_string();
        let flag = Some(local.clone());
        assert_eq!(api_url(flag, none, None, release), Some(local.clone()));
        let env = |name: &str| (name == "PLOW_API_URL").then(|| local.clone());
        assert_eq!(api_url(None, env, None, release), Some(local.clone()));
        assert_eq!(
            api_url(None, none, Some(local.clone()), release),
            Some(local)
        );
    }

    #[test]
    fn the_qr_code_and_the_link_carry_the_number_and_the_text() {
        assert_eq!(smsto("+14155550100", "AB12C"), "SMSTO:+14155550100:AB12C");
        assert_eq!(
            sms_link("+14155550100", "AB12C"),
            "sms:+14155550100?&body=AB12C"
        );
    }

    #[test]
    fn a_north_american_number_is_written_as_people_write_it() {
        assert_eq!(for_people("+14155550100"), "+1 415-555-0100");
        assert_eq!(for_people("+442071838750"), "+442071838750");
        assert_eq!(for_people("+1415555010"), "+1415555010");
    }

    #[test]
    fn a_login_asks_every_second_for_two_minutes_then_every_five() {
        assert_eq!(poll_every(Duration::ZERO), Duration::from_secs(1));
        assert_eq!(poll_every(Duration::from_secs(119)), Duration::from_secs(1));
        assert_eq!(poll_every(Duration::from_secs(120)), Duration::from_secs(5));
        assert_eq!(poll_every(Duration::from_secs(599)), Duration::from_secs(5));
    }

    #[test]
    fn a_login_gives_up_after_ten_minutes() {
        let second = Duration::from_secs(1);
        assert_eq!(before_giving_up(Duration::ZERO, second), Some(second));
        assert_eq!(
            before_giving_up(Duration::from_millis(599_500), 5 * second),
            Some(Duration::from_millis(500))
        );
        assert_eq!(before_giving_up(Duration::from_secs(600), second), None);
    }

    #[test]
    fn a_name_is_quoted_for_the_shell_only_when_it_needs_it() {
        assert_eq!(shell_quoted("mac-2"), "mac-2");
        assert_eq!(shell_quoted("Ada's Mac"), r"'Ada'\''s Mac'");
        assert_eq!(shell_quoted(""), "''");
    }

    #[test]
    fn escape_leaves_only_unreserved_characters() {
        assert_eq!(escape("mac-studio_2.local~"), "mac-studio_2.local~");
        assert_eq!(escape("a b: c"), "a%20b%3A%20c");
        assert_eq!(escape("a/b?é"), "a%2Fb%3F%C3%A9");
    }
}
