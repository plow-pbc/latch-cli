//! Choosing which host a command goes to.
//!
//! Order: `--host NAME`, then `$LATCH_HOST` (clap folds both into one
//! value), then the only host in the directory other than this machine, if
//! there is exactly one.
//! Anything else is a usage error (exit 2) that lists the hosts. Whether the
//! host is actually up is learned by dialing it, not from its `online` flag.

use crate::failure::Failure;
use crate::plow::{Peer, Plow};
use crate::state::StateDir;

/// Looks up the account's hosts in Plow and picks one.
pub async fn resolve(state: &StateDir, asked: Option<String>) -> Result<Peer, Failure> {
    let peers = Plow::from_state(state)?.list_peers().await?;
    let own_key = state.load_key()?.public().to_string();
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
    // This machine is never picked for you: it cannot call itself.
    let others: Vec<Peer> = hosts
        .iter()
        .filter(|host| !host.this_machine)
        .cloned()
        .collect();
    match others.as_slice() {
        [only] => Ok(only.clone()),
        [] if hosts.is_empty() => {
            let message = "no hosts in this account; start `latch serve` on one".to_string();
            Err(Failure::usage("no_host", message))
        }
        [] => {
            let message = "the only host is this machine, which cannot call itself; \
                           call it from another machine or state directory"
                .to_string();
            Err(Failure::usage("no_host", message))
        }
        _ => {
            let message = format!(
                "several hosts; pass --host or set LATCH_HOST: {}",
                names(&others)
            );
            Err(Failure::usage("host_required", message))
        }
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
    fn this_machine_is_never_the_default_host() {
        let hosts = only_hosts(vec![host("me"), host("pi")], "key-of-me");
        assert_eq!(pick(None, &hosts).unwrap().name, "pi");
        // Named, it is still found, so dialing it says why it cannot work.
        assert_eq!(pick(Some("me"), &hosts).unwrap().name, "me");
        let alone = only_hosts(vec![host("me")], "key-of-me");
        let failure = pick(None, &alone).unwrap_err();
        assert!(
            failure.message.contains("this machine"),
            "{}",
            failure.message
        );
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
