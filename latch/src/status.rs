//! `latch status`: who this peer is, and which hosts it can reach. With
//! `--local`, only what this machine knows, for an app (see `local`).

use std::path::PathBuf;

use serde::Serialize;

use crate::credential;
use crate::failure::Failure;
use crate::hosts;
use crate::keyart;
use crate::lifecycle::{self, Phase, Problem};
use crate::plow::{Peer, Plow};
use crate::state::StateDir;

#[derive(Serialize)]
pub struct StatusOutput {
    pub peer: Peer,
    pub api_url: String,
    pub hosts: Vec<Peer>,
    /// The key art (see `keyart.rs`) of this peer, then of each host: a
    /// person compares it with the art the other machine shows for the
    /// same key.
    pub art: Vec<KeyArt>,
}

#[derive(Serialize)]
pub struct KeyArt {
    pub name: String,
    pub key: String,
    /// The picture, a line each.
    pub lines: Vec<String>,
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
    let art = std::iter::once(&peer)
        .chain(&hosts)
        .filter_map(|peer| {
            Some(KeyArt {
                name: peer.name.clone(),
                key: peer.key.clone(),
                lines: keyart::art(&peer.key, &peer.name)?,
            })
        })
        .collect();
    Ok(StatusOutput {
        peer: Peer {
            this_machine: false,
            ..peer
        },
        api_url: config.api_url,
        hosts,
        art,
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
    /// Why the host last stopped, when it stopped by a failure.
    pub last_error: Option<Problem>,
    /// Whether a newer build is available, as the host last looked.
    pub update: crate::update::Status,
    /// An older latch's plaintext audit log in the state folder, which this
    /// latch never reads: it can be deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_audit_log: Option<PathBuf>,
    /// Said only where Chrome runs without its own sandbox
    /// (`sandbox::page_isolation_off`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_isolation: Option<&'static str>,
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
    /// It is online and accepting callers: this latch can reach it.
    pub ready: bool,
    /// No host runs; a `latch plugins` command is changing the connectors.
    pub configuring: bool,
    /// It runs as this user's service, as `latch serve` and `serve -d`
    /// start it, rather than in the foreground (`serve --foreground`).
    pub managed: bool,
    /// A managed host held by the `latch serve` in a terminal that started
    /// it, rather than one `serve -d` started.
    pub attached: bool,
    pub state: Phase,
    pub pid: Option<u32>,
    /// Why a host that runs cannot be used: one from another latch version.
    pub blocked: Option<Problem>,
    /// The agents refused `exec` here (`serve --deny-exec`): as the host
    /// runs, or as the last one ran; `null` if its saved flags are unreadable.
    pub deny_exec: Option<crate::serve::DenyExec>,
}

/// `status --local`: reads the state folder and asks no one, so it works
/// offline and before login. Not logged in and not serving are answers.
pub fn local(state: &StateDir) -> Result<LocalStatus, Failure> {
    let host = lifecycle::now(&state.path);
    let blocked = lifecycle::other_version(&state.path).map(|pid| {
        let failure = lifecycle::other_version_running(pid);
        Problem {
            code: failure.code,
            message: failure.message,
        }
    });
    let peer = state.load_config()?.map(|config| Me {
        name: config.peer.name,
        key: config.peer.key,
        kind: config.peer.kind,
    });
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    Ok(LocalStatus {
        state_dir: dir.clone(),
        version: crate::cli::version(),
        protocol: crate::wire::protocol(),
        peer,
        serve: Serve {
            running: host.running,
            ready: host.state == Phase::Serving && blocked.is_none(),
            configuring: host.configuring,
            managed: host.running && crate::managed::Service::new(&dir).registered(),
            attached: host.attached,
            state: host.state,
            pid: host.pid,
            blocked,
            deny_exec: crate::managed::saved_deny_exec(&dir),
        },
        last_error: host.error,
        update: crate::update::status(&dir),
        old_audit_log: crate::audit::plaintext(&dir),
        page_isolation: crate::sandbox::page_isolation_off()
            .then_some(crate::sandbox::PAGE_ISOLATION_OFF),
    })
}
