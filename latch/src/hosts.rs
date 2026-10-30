//! Choosing which host a command goes to.
//!
//! Order: `--host NAME`, then `$LATCH_HOST` (clap folds both into one
//! value), then this machine if it has served, otherwise the only host
//! in the directory, if there is exactly one. The name `default` is the
//! host `latch hosts default` saved in this state folder: a choice of this
//! machine or agent, which reaches no host it couldn't name itself.
//! Anything else is a usage error (exit 2) that lists the hosts. Whether the
//! host is up isn't checked here: `dial::connect` refuses one Plow lists as
//! offline without dialing, and learns about any other by dialing it.

use crate::failure::{Failure, code};
use crate::login;
use crate::plow::{Peer, Plow};
use crate::state::{Config, StateDir};

/// What `--host default` names.
pub const DEFAULT: &str = "default";
/// The saved default host's name, in the state folder.
const DEFAULT_FILE: &str = "default-host";

/// The host `latch hosts default` saved, if one is.
pub fn saved_default(state: &StateDir) -> Result<Option<String>, Failure> {
    let path = state.path.join(DEFAULT_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_string()).filter(|name| !name.is_empty())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Failure::new(
            code::bad_state,
            format!("{}: {err}", path.display()),
        )),
    }
}

/// `asked` with `default` read now as the saved host's name, for a command
/// that calls its host more than once: each call then goes to the host the
/// first went to, whatever is saved meanwhile. With none saved it stays
/// `default`, which each call refuses as `no_default_host`.
pub fn pinned(state: &StateDir, asked: Option<String>) -> Result<Option<String>, Failure> {
    Ok(match named(state, asked.as_deref())? {
        Named::Host(name) => Some(name),
        Named::Bare | Named::NoDefault => asked,
    })
}

/// What a command's `--host` (or `LATCH_HOST`) names.
pub(crate) enum Named {
    /// No host named.
    Bare,
    /// A host by name, `default` read as the saved default host.
    Host(String),
    /// `default`, with no default host saved.
    NoDefault,
}

impl Named {
    /// The name, if one is.
    pub(crate) fn host(&self) -> Option<&str> {
        match self {
            Named::Host(name) => Some(name),
            _ => None,
        }
    }
}

/// What `asked` names, reading the saved default host once, so a choice
/// changed meanwhile can't send one call two ways.
pub(crate) fn named(state: &StateDir, asked: Option<&str>) -> Result<Named, Failure> {
    Ok(match asked {
        None => Named::Bare,
        Some(DEFAULT) => match saved_default(state)? {
            Some(name) => Named::Host(name),
            None => Named::NoDefault,
        },
        Some(name) => Named::Host(name.to_string()),
    })
}

/// `--host default` with no default saved: how to save one, and the hosts.
fn no_default(hosts: &[Peer]) -> Failure {
    Failure::new(
        code::no_default_host,
        format!(
            "no default host set; `latch hosts default NAME` sets one. Hosts: {}",
            names(hosts)
        ),
    )
}

/// Owner configuration without a target, or naming this peer, stays local.
pub fn local_target(state: &StateDir, asked: Option<&str>) -> Result<bool, Failure> {
    Ok(match named(state, asked)? {
        Named::Bare => true,
        // No default saved: `resolve` says so, with the hosts.
        Named::NoDefault => false,
        Named::Host(name) => state.require_config()?.peer.name == name,
    })
}

/// Resolves a saved local host without Plow or any secret; otherwise looks
/// up the account's hosts. If Plow no longer
/// lists this machine (an agent whose credential was rotated, say), it
/// registers again first: a host refuses a caller Plow does not know.
pub async fn resolve(state: &StateDir, asked: Option<String>) -> Result<Peer, Failure> {
    let config = state.require_config()?;
    let named = named(state, asked.as_deref())?;
    let asked = named.host();
    if !matches!(named, Named::NoDefault) && this_machine_chosen(state, &config, asked) {
        return Ok(Peer {
            this_machine: true,
            ..config.peer
        });
    }
    let peers = Plow::from_state(state)?.list_peers().await?;
    let own_key = state.public_key()?.to_string();
    if !peers.iter().any(|peer| peer.key == own_key) {
        login::register_again(state).await?;
    }
    let hosts = only_hosts(peers, &own_key);
    if let Named::NoDefault = named {
        return Err(no_default(&hosts));
    }
    pick(asked, &hosts)
}

/// `latch hosts default`: the saved default host; with `name`, saves that
/// host (one Plow lists for this account), with `clear`, removes it.
pub async fn default(
    state: &StateDir,
    name: Option<String>,
    clear: bool,
) -> Result<serde_json::Value, Failure> {
    let path = state.path.join(DEFAULT_FILE);
    let io =
        |err: std::io::Error| Failure::new(code::bad_state, format!("{}: {err}", path.display()));
    if clear {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)),
        }
        return Ok(serde_json::json!({"default_host": null}));
    }
    let Some(name) = name else {
        return Ok(serde_json::json!({"default_host": saved_default(state)?}));
    };
    let config = state.require_login()?;
    let peers = Plow::from_state(state)?.list_peers().await?;
    let hosts = only_hosts(peers, &config.peer.key);
    if name == DEFAULT || !hosts.iter().any(|host| host.name == name) {
        let message = format!("no host named {name:?}; hosts: {}", names(&hosts));
        return Err(Failure::new(code::unknown_host, message));
    }
    crate::state::write_private(&path, format!("{name}\n").as_bytes()).map_err(io)?;
    Ok(serde_json::json!({"default_host": name}))
}

/// Whether `asked` (or, naming none, the default) is this machine itself:
/// it names it, or names nothing and this machine has served or serves now.
pub fn this_machine_chosen(state: &StateDir, config: &Config, asked: Option<&str>) -> bool {
    match asked {
        Some(name) => name == config.peer.name,
        None => {
            config.peer.relay_url.is_some()
                || crate::transport::local_socket_path(&state.path).exists()
        }
    }
}

/// The peers that accept connections (those with a relay URL), with this
/// machine (the one whose key is `own_key`) marked.
pub fn only_hosts(peers: Vec<Peer>, own_key: &str) -> Vec<Peer> {
    peers
        .into_iter()
        .filter(|peer| peer.relay_url.is_some())
        .map(|peer| Peer {
            this_machine: peer.key == own_key,
            ..peer
        })
        .collect()
}

pub fn pick(asked: Option<&str>, hosts: &[Peer]) -> Result<Peer, Failure> {
    if let Some(name) = asked {
        return match hosts.iter().find(|host| host.name == name) {
            Some(host) => Ok(host.clone()),
            None => {
                let message = format!("no host named {name:?}; hosts: {}", names(hosts));
                Err(Failure::new(code::unknown_host, message))
            }
        };
    }
    match hosts {
        [only] => Ok(only.clone()),
        [] => Err(Failure::new(
            code::no_host,
            "no hosts in this account; start `latch serve` on one".into(),
        )),
        _ => Err(Failure::new(
            code::host_required,
            format!(
                "several hosts; pass --host or set LATCH_HOST: {}",
                names(hosts)
            ),
        )),
    }
}

fn names(hosts: &[Peer]) -> String {
    let names: Vec<&str> = hosts.iter().map(|host| host.name.as_str()).collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;

    fn host(name: &str) -> Peer {
        Peer {
            key: format!("key-of-{name}"),
            kind: "device".to_string(),
            name: name.to_string(),
            relay_url: Some("https://relay.example/".to_string()),
            last_seen_at: None,
            online: Some(true),
            this_machine: false,
        }
    }

    #[test]
    fn only_peers_with_a_relay_are_hosts() {
        let mut agent = host("agent");
        agent.relay_url = None;
        let hosts = only_hosts(vec![host("a"), agent], "key-of-a");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "a");
        // This machine is listed, marked, not dropped.
        assert!(hosts[0].this_machine);
    }

    #[test]
    fn this_machine_is_selectable_and_is_the_only_host_default() {
        let hosts = only_hosts(vec![host("me"), host("pi")], "key-of-me");
        assert_eq!(pick(None, &hosts).unwrap_err().code, "host_required");
        assert_eq!(pick(Some("me"), &hosts).unwrap().name, "me");
        let alone = only_hosts(vec![host("me")], "key-of-me");
        assert_eq!(pick(None, &alone).unwrap().name, "me");
    }

    #[test]
    fn named_host_wins_even_with_several() {
        let hosts = [host("a"), host("b")];
        assert_eq!(pick(Some("b"), &hosts).unwrap().name, "b");
    }

    #[test]
    fn single_host_is_the_default() {
        assert_eq!(pick(None, &[host("a")]).unwrap().name, "a");
    }

    #[test]
    fn several_hosts_without_a_name_is_a_usage_error_listing_them() {
        let failure = pick(None, &[host("a"), host("b")]).unwrap_err();
        assert_eq!(failure.exit, Exit::Usage);
        assert_eq!(failure.code, "host_required");
        assert!(failure.message.ends_with("a, b"));
    }

    #[test]
    fn unknown_name_and_empty_directory_are_usage_errors() {
        assert_eq!(
            pick(Some("c"), &[host("a")]).unwrap_err().code,
            "unknown_host"
        );
        assert_eq!(pick(None, &[]).unwrap_err().code, "no_host");
    }
}
