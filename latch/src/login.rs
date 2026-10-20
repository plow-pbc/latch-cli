//! `latch login`: create this peer's key if missing and register it with
//! Plow. Safe to repeat: the key is reused and Plow treats a repeat as a
//! refresh, so a second login changes nothing.
//!
//! The credential, in order: `--token-file` (saved as this machine's owner
//! token); the owner token saved here by an earlier login; the agent's own
//! credential; and, only with a person at the terminal, a login by text.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;

use crate::cli::{BUILD_API_URL, LoginArgs};
use crate::credential;
use crate::devices;
use crate::failure::Failure;
use crate::heartbeat;
use crate::plow::{Peer, Plow, Registration};
use crate::state::{Config, StateDir};
use crate::term;

/// How often a login by text asks Plow whether the code has been texted.
const POLL: Duration = Duration::from_secs(3);

#[derive(Serialize)]
pub struct LoginOutput {
    pub peer: Peer,
    pub key_created: bool,
    /// The machine this login took the name from, if it replaced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<Peer>,
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
    // Flags win; on a repeat login, fall back to what the first one saved.
    // A cloud agent's `$PLOW_API_BASE` comes before `$PLOW_API_URL`: on
    // exe.dev it is the proxy that adds the agent's credential.
    let saved = state.load_config()?;
    let api_url = args
        .api_url
        .or_else(|| credential::env("PLOW_API_BASE"))
        .or_else(|| credential::env("PLOW_API_URL"))
        .or(BUILD_API_URL.map(String::from))
        .or(saved.as_ref().map(|config| config.api_url.clone()));
    let Some(api_url) = api_url else {
        return Err(missing("--api-url, $PLOW_API_BASE or $PLOW_API_URL"));
    };
    let source = if let Some(path) = &args.token_file {
        Source::Given(credential::read(path)?)
    } else if let Some(token) = state.load_token()? {
        Source::Saved(token)
    } else if let Some(token) = credential::agent_token_if_present()? {
        Source::Agent(token)
    } else if term::interactive() {
        Source::ByText
    } else {
        // Never wait on a text nobody will see.
        let message = "no Plow credential: pass --token-file FILE with the owner's Plow login, \
                       or run `latch login` at a terminal to log in by text"
            .to_string();
        return Err(Failure::usage("no_credential", message));
    };
    let owner = !matches!(source, Source::Agent(_));
    let save_token = matches!(source, Source::Given(_));

    let mut name = args
        .name
        .or(saved.as_ref().map(|config| config.peer.name.clone()));
    // A machine needs a name; an agent's defaults to its own.
    if owner && name.is_none() && term::interactive() {
        name = Some(term::ask("Name for this machine: ")).filter(|name| !name.is_empty());
    }

    let token = match source {
        Source::Given(token) | Source::Saved(token) | Source::Agent(token) => token,
        Source::ByText => {
            let Some(name) = &name else {
                return Err(missing("--name"));
            };
            by_text(state, &api_url, name).await?
        }
    };

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
        take_over: false,
    };
    let plow = Plow::new(&api_url, &token);
    let (peer, replaced) = match plow.register(&registration).await {
        // Another of the owner's machines may hold the name.
        Err(failure) if failure.code == "conflict" && owner => {
            let Some(name) = &registration.name else {
                return Err(failure);
            };
            // If this same login holds it (this machine logged in again with
            // a new key, say), Plow moves the name here; nobody is asked.
            let take_over = Registration {
                take_over: true,
                ..registration.clone()
            };
            match plow.register(&take_over).await {
                Ok(peer) => {
                    eprintln!("The name {name} now points to this machine.");
                    (peer, None)
                }
                Err(failure) if failure.code == "conflict" => {
                    let replaced = replace_machine(&plow, name, &registration.key).await?;
                    (plow.register(&registration).await?, Some(replaced))
                }
                Err(failure) => return Err(failure),
            }
        }
        result => (result?, None),
    };

    // Save only after Plow accepted us, so a failed first login leaves no
    // half-configured instance behind (apart from the key, and the token of
    // a login by text, which is saved as soon as it arrives).
    if save_token {
        state.save_token(&token)?;
    }
    state.save_config(&Config {
        api_url,
        peer: peer.clone(),
    })?;
    Ok(LoginOutput {
        peer,
        key_created,
        replaced,
    })
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
        take_over: false,
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
/// shown once and never logged.
async fn by_text(state: &StateDir, api_url: &str, name: &str) -> Result<String, Failure> {
    let plow = Plow::anonymous(api_url);
    let activation = plow.activate(name).await?;
    let text = format!("Plow Activate: {}", activation.display_code);
    eprintln!("Text \"{text}\" to {}", activation.send_to);
    eprintln!("Whoever texts this code first gets the login. Waiting for your text...");
    open_messages(&activation.send_to, &text);

    let mut failures = 0;
    loop {
        let wait = match plow.redeem(&activation.activation_secret).await {
            Ok(answer) if answer.status == "verified" => {
                // Only the first `verified` answer carries the token.
                let Some(token) = answer.token else {
                    let message = "this login was already collected; run `latch login` again";
                    return Err(Failure::failed("login_collected", message.to_string()));
                };
                state.create()?;
                state.save_token(&token)?;
                eprintln!("Logged in.");
                return Ok(token);
            }
            Ok(_) => {
                failures = 0;
                POLL
            }
            Err(failure) if failure.code == "expired" => {
                let message = "the code expired before it was texted; run `latch login` again";
                return Err(Failure::failed("code_expired", message.to_string()));
            }
            Err(failure) if failure.code == "not_found" => {
                let message = "Plow no longer knows this login; run `latch login` again";
                return Err(Failure::failed("login_not_found", message.to_string()));
            }
            // Plow unreachable or failing: wait longer each time.
            Err(failure) => {
                failures += 1;
                heartbeat::backoff(failures, failure.retry_after).max(POLL)
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// On macOS, opens Messages with the text filled in, ready to send. Best
/// effort: the printed instruction is enough without it.
fn open_messages(send_to: &str, text: &str) {
    if cfg!(target_os = "macos") {
        let url = format!("sms:{send_to}?&body={}", escape(text));
        let _ = Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The name is taken. If another of the owner's machines holds it, a person
/// at the terminal may replace that machine, which signs it out; otherwise
/// the login stops and says what to do. A name an agent holds is never
/// offered for replacement.
async fn replace_machine(plow: &Plow, name: &str, key: &str) -> Result<Peer, Failure> {
    let peers = plow.list_peers().await?;
    let holder = peers.iter().find(|peer| peer.name == name);
    let Some(machine) = holder.filter(|peer| peer.kind == "device" && peer.key != key) else {
        let message = match holder {
            Some(peer) if peer.kind == "agent" => {
                format!("the name {name:?} belongs to an agent; choose another --name")
            }
            _ => format!(
                "this key or the name {name:?} is taken by another peer; choose another --name"
            ),
        };
        return Err(Failure::usage("conflict", message));
    };
    let last_seen = machine.last_seen_at.as_deref().unwrap_or("never");
    if !term::interactive() {
        let message = format!(
            "a machine named {name:?} already exists (last seen {last_seen}); \
             remove it with `latch devices remove {name}`, or choose another --name"
        );
        return Err(Failure::usage("conflict", message));
    }
    let question = format!(
        "A machine named {name} already exists (last seen {last_seen}). Replace it? This signs that machine out."
    );
    if !term::confirm(&question) {
        let message = format!("kept the machine named {name:?}; choose another --name");
        return Err(Failure::usage("conflict", message));
    }
    devices::remove_from_plow(plow, machine).await
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

fn missing(what: &str) -> Failure {
    Failure::usage("not_configured", format!("first login needs {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_leaves_only_unreserved_characters() {
        assert_eq!(escape("mac-studio_2.local~"), "mac-studio_2.local~");
        assert_eq!(escape("Plow Activate: AB12C"), "Plow%20Activate%3A%20AB12C");
        assert_eq!(escape("a/b?é"), "a%2Fb%3F%C3%A9");
    }
}
