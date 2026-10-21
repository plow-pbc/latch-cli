//! Choosing which host a command goes to.
//!
//! Order: `--host NAME`, then `$LATCH_HOST` (clap folds both into one
//! value), then this machine if it has served, otherwise the only host
//! in the directory, if there is exactly one.
//! Anything else is a usage error (exit 2) that lists the hosts. Whether the
//! host is actually up is learned by dialing it, not from its `online` flag.

use crate::failure::Failure;
use crate::login;
use crate::plow::{Peer, Plow};
use crate::state::StateDir;

/// Owner configuration without a target, or naming this peer, stays local.
pub fn local_target(state: &StateDir, asked: Option<&str>) -> Result<bool, Failure> {
    Ok(match asked {
        None => true,
        Some(name) => state.require_login()?.peer.name == name,
    })
}

/// Resolves a saved local host without Plow; otherwise looks up the account's hosts. If Plow no longer
/// lists this machine (an agent whose credential was rotated, say), it
/// registers again first: a host refuses a caller Plow does not know.
pub async fn resolve(state: &StateDir, asked: Option<String>) -> Result<Peer, Failure> {
    let config = state.require_login()?;
    if asked.as_deref() == Some(config.peer.name.as_str())
        || (asked.is_none()
            && (config.peer.relay_url.is_some() || state.path.join("serve.sock").exists()))
    {
        return Ok(Peer {
            this_machine: true,
            ..config.peer
        });
    }
    let peers = Plow::from_state(state)?.list_peers().await?;
    let own_key = state.load_key()?.public().to_string();
    if !peers.iter().any(|peer| peer.key == own_key) {
        login::register_again(state).await?;
    }
    pick(asked.as_deref(), &only_hosts(peers, &own_key))
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
                Err(Failure::usage("unknown_host", message))
            }
        };
    }
    match hosts {
        [only] => Ok(only.clone()),
        [] => Err(Failure::usage(
            "no_host",
            "no hosts in this account; start `latch serve` on one".into(),
        )),
        _ => Err(Failure::usage(
            "host_required",
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
