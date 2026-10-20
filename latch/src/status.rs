//! `latch status`: who this peer is, and which hosts it can reach.

use serde::Serialize;

use crate::credential;
use crate::failure::Failure;
use crate::hosts;
use crate::plow::{Peer, Plow};
use crate::state::StateDir;

#[derive(Serialize)]
pub struct StatusOutput {
    pub peer: Peer,
    pub api_url: String,
    pub hosts: Vec<Peer>,
}

pub async fn run(state: &StateDir) -> Result<StatusOutput, Failure> {
    let config = state.require_login()?;
    let peers = Plow::new(&config.api_url, &credential::token(state)?)
        .list_peers()
        .await?;
    let hosts = hosts::only_hosts(peers, &config.peer.key);
    // Plow's record of this peer, just fetched, rather than the one `login`
    // or the last heartbeat saved, so `peer` and its row in `hosts` agree.
    let peer = hosts
        .iter()
        .find(|host| host.this_machine)
        .cloned()
        .unwrap_or(config.peer);
    Ok(StatusOutput {
        peer: Peer {
            this_machine: false,
            ..peer
        },
        api_url: config.api_url,
        hosts,
    })
}
