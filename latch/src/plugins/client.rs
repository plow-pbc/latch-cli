//! `latch plugins …`. `list` asks any host, as other commands do; `enable`
//! and `disable` are for this machine only, and only from the owner's own
//! login: managing another host's connectors from here is not a thing
//! latch does. They go to this machine's host over its private socket when
//! it runs; when none does, this command makes the same change itself, in
//! the state folder the next `latch serve` reads, and `list` with no host
//! named reads it there.

use serde_json::{Value, json};

use super::{Connector, Plugins, host};
use crate::cli::{PluginsArgs, PluginsCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::lifecycle;
use crate::plow::{Peer, Plow};
use crate::state::StateDir;
use crate::wire::Request;

pub async fn run(state: &StateDir, args: PluginsArgs) -> Result<Value, Failure> {
    if let PluginsCommand::List { host: None } = args.command
        && let Some(listed) = saved_list(state)?
    {
        return Ok(listed);
    }
    let (host, args) = match args.command {
        PluginsCommand::List { host } => (
            hosts::resolve(state, host).await?,
            json!({"command": "list"}),
        ),
        PluginsCommand::Enable { name } => (
            this_host(state)?,
            json!({"command": "enable", "name": name}),
        ),
        PluginsCommand::Disable { name } => (
            this_host(state)?,
            json!({"command": "disable", "name": name}),
        ),
    };
    let request = Request {
        op: "plugins".to_string(),
        args,
        chat: None,
    };
    match dial::call(state, &host, &request, ANSWER_WITHIN).await {
        Err(failure) if failure.code == "serve_required" && host.this_machine => {
            without_a_host(state, &request.args).await
        }
        answer => answer,
    }
}

/// This machine's connectors as last saved, when it is the owner's host
/// and none runs: what the next `latch serve` will have.
fn saved_list(state: &StateDir) -> Result<Option<Value>, Failure> {
    let owner = state
        .load_config()?
        .is_some_and(|config| config.peer.kind == "device");
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    if !owner || lifecycle::holds_lock(&dir) {
        return Ok(None);
    }
    let home = std::env::home_dir().unwrap_or_default();
    Ok(Some(Plugins::load(state, &home)?.list()))
}

/// `enable` or `disable` with no host running: the host's own setup and
/// save, made here.
async fn without_a_host(state: &StateDir, args: &Value) -> Result<Value, Failure> {
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    if let Some(pid) = lifecycle::now(&dir).pid {
        // Running, but not on this latch's socket: most likely an older latch.
        let message = format!(
            "a `latch serve` (pid {pid}) holds this state folder but does not answer this \
             latch, so it may be an older one: stop it, then run this again"
        );
        return Err(Failure::new(Exit::Unreachable, "host_unreachable", message));
    }
    let connector = Connector::named(args["name"].as_str().unwrap_or_default())?;
    let workers = crate::serve::workers(dir.clone())?;
    let plugins = Plugins::load(state, &workers.home)?;
    let result = if args["command"] == "enable" {
        let plow = Plow::from_state(state)?;
        host::enable(&plugins, &workers, &plow, connector, true).await
    } else {
        host::disable(&plugins, connector).await
    };
    if lifecycle::holds_lock(&dir) {
        // It may have read the connectors before this saved them.
        let message = format!(
            "a `latch serve` started on this state folder while this ran; run `latch plugins \
             {} {}` again",
            args["command"].as_str().unwrap_or_default(),
            connector.name()
        );
        return Err(Failure::failed("host_started", message));
    }
    result
}

/// This machine's own host, when the owner is the one asking. An agent's
/// login never sends the command at all.
fn this_host(state: &StateDir) -> Result<Peer, Failure> {
    let config = state.require_config()?;
    if config.peer.kind != "device" {
        let message = "plugins are turned on and off by the owner, on the host itself";
        return Err(Failure::new(Exit::Denied, "owner_only", message.into()));
    }
    Ok(Peer {
        this_machine: true,
        ..config.peer
    })
}
