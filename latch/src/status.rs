//! `latch status`: who this peer is, and which hosts it can reach. With
//! `--local`, only what this machine knows, for an app (see `local`).

use std::path::PathBuf;

use serde::Serialize;

use crate::credential;
use crate::failure::Failure;
use crate::hosts;
use crate::lifecycle::{self, Phase, Problem};
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

#[derive(Serialize)]
pub struct LocalStatus {
    pub state_dir: PathBuf,
    pub version: &'static str,
    pub protocol: u32,
    /// This machine's identity, or `null` before it logged in.
    pub peer: Option<Me>,
    pub serve: Serve,
    pub shares: Vec<PathBuf>,
    /// Why the host last stopped, when it stopped by a failure.
    pub last_error: Option<Problem>,
}

#[derive(Serialize)]
pub struct Me {
    pub name: String,
    pub key: String,
    pub kind: String,
}

#[derive(Serialize)]
pub struct Serve {
    /// A `latch serve` holds this state folder.
    pub running: bool,
    /// It is online and accepting callers.
    pub ready: bool,
    pub state: Phase,
    pub pid: Option<u32>,
}

/// `status --local`: reads the state folder and asks no one, so it works
/// offline and before login. Not logged in and not serving are answers.
pub fn local(state: &StateDir) -> Result<LocalStatus, Failure> {
    let host = lifecycle::now(&state.path);
    let peer = state.load_config()?.map(|config| Me {
        name: config.peer.name,
        key: config.peer.key,
        kind: config.peer.kind,
    });
    Ok(LocalStatus {
        state_dir: state.path.canonicalize().unwrap_or(state.path.clone()),
        version: crate::cli::version(),
        protocol: crate::wire::protocol(),
        peer,
        serve: Serve {
            running: host.running,
            ready: host.state == Phase::Serving,
            state: host.state,
            pid: host.pid,
        },
        shares: state.load_shares()?,
        last_error: host.error,
    })
}
