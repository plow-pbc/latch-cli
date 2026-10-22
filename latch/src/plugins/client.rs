//! `latch plugins …`. `list` asks any host, as other commands do; `enable`
//! and `disable` go only to this machine's own running host, over its
//! private socket, and only from the owner's own login: managing another
//! host's connectors from here is not a thing latch does.

use serde_json::{Value, json};

use crate::cli::{PluginsArgs, PluginsCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::Peer;
use crate::state::StateDir;
use crate::wire::Request;

pub async fn run(state: &StateDir, args: PluginsArgs) -> Result<Value, Failure> {
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
    dial::call(state, &host, &request, ANSWER_WITHIN).await
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
