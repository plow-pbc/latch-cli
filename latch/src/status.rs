//! `latch status`: who this peer is, and which hosts it can reach. With
//! `--local`, only what this machine knows, for an app (see `local`); with
//! `--follow` too, again each time that changes (see `follow`).

use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::credential;
use crate::failure::Failure;
use crate::hosts;
use crate::lifecycle::{self, Phase, Problem};
use crate::plow::{Peer, Plow};
use crate::state::StateDir;
use crate::watch::{Watch, Woke};

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
    /// Why the host last stopped, when it stopped by a failure.
    pub last_error: Option<Problem>,
    /// Whether a newer build is available, as the host last looked.
    pub update: crate::update::Status,
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
        },
        last_error: host.error,
        update: crate::update::status(&dir),
        page_isolation: crate::sandbox::page_isolation_off()
            .then_some(crate::sandbox::PAGE_ISOLATION_OFF),
    })
}

/// `status --local --follow`: `hello`, then this machine's `status --local`
/// as a `status` line, and again each time it changes, given to `send`
/// until it returns false (or `hangup` closes). It sleeps until the state
/// folder or the host changes, and never opens the audit. A host streams
/// its own to its owner's `status --host --follow` this way.
pub fn follow(
    dir: &Path,
    hangup: Option<RawFd>,
    mut send: impl FnMut(&Value) -> bool,
) -> Result<(), Failure> {
    let watching =
        |err: std::io::Error| Failure::failed("watch_failed", format!("{}: {err}", dir.display()));
    let state = StateDir {
        path: dir.to_path_buf(),
    };
    let lock = dir.join("serve.lock");
    let mut watch = Watch::new(dir, &[lock], hangup).map_err(watching)?;
    if !send(&json!({"type": "hello", "schema": 1})) {
        return Ok(());
    }
    let mut sent = None;
    loop {
        watch.arm().map_err(watching)?;
        let status = local(&state)?;
        watch.process(status.serve.pid);
        let status = serde_json::to_value(status).expect("a status serializes");
        if sent.as_ref() != Some(&status) {
            if !send(&json!({"type": "status", "status": status})) {
                return Ok(());
            }
            sent = Some(status);
        }
        match watch.wait().map_err(watching)? {
            Woke::HungUp => return Ok(()),
            Woke::Changed => {}
        }
    }
}
