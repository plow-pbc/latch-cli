//! `latch devices`: the owner's machines in Plow's directory, online or
//! not, and `latch devices remove`, which takes one out and signs out the
//! Plow login that registered it. Removing is for an owner's machines only:
//! an agent is refused here, and Plow refuses it too.

use serde::Serialize;

use crate::failure::{Exit, Failure};
use crate::plow::{Peer, Plow};
use crate::state::StateDir;
use crate::term;

#[derive(Serialize)]
pub struct DevicesOutput {
    pub devices: Vec<Peer>,
}

#[derive(Serialize)]
pub struct RemoveOutput {
    pub removed: Peer,
}

pub async fn list(state: &StateDir) -> Result<DevicesOutput, Failure> {
    let devices = machines(Plow::from_state(state)?.list_peers().await?);
    Ok(DevicesOutput { devices })
}

pub async fn remove(state: &StateDir, name: &str, yes: bool) -> Result<RemoveOutput, Failure> {
    let config = state.require_login()?;
    owner_only(&config.peer)?;
    let plow = Plow::from_state(state)?;
    let peers = plow.list_peers().await?;
    let Some(machine) = machines(peers).into_iter().find(|peer| peer.name == name) else {
        let message = format!("no machine named {name:?}; `latch devices` lists them");
        return Err(Failure::failed("not_found", message));
    };
    if machine.key == config.peer.key {
        return Err(own_machine(name));
    }
    if !yes {
        if !term::interactive() {
            let message = format!("removing {name:?} signs that machine out; pass --yes to do it");
            return Err(Failure::usage("confirmation_required", message));
        }
        let question = format!(
            "Remove {name}? This signs that machine out of Plow, and anything else using \
             the same login (for example the Plow Mac app) is signed out too."
        );
        if !term::confirm(&question) {
            let message = format!("kept the machine named {name:?}");
            return Err(Failure::usage("not_removed", message));
        }
    }
    let removed = remove_from_plow(&plow, &machine).await?;
    Ok(RemoveOutput { removed })
}

/// Removes the machine in Plow. Its only conflict for a machine is that it
/// is logged in with this same credential.
pub async fn remove_from_plow(plow: &Plow, machine: &Peer) -> Result<Peer, Failure> {
    match plow.remove_peer(&machine.key).await {
        Err(failure) if failure.code == "conflict" => Err(own_machine(&machine.name)),
        result => result,
    }
}

fn machines(peers: Vec<Peer>) -> Vec<Peer> {
    peers
        .into_iter()
        .filter(|peer| peer.kind == "device")
        .collect()
}

/// Machines are the owner's to remove; this peer must be one of them.
fn owner_only(me: &Peer) -> Result<(), Failure> {
    if me.kind == "device" {
        return Ok(());
    }
    let message = "only an owner's machine removes machines; this peer is an agent".to_string();
    Err(Failure::new(Exit::Denied, "owner_only", message))
}

fn own_machine(name: &str) -> Failure {
    let message = format!(
        "{name:?} is logged in with this machine's own login; removing it would sign you out"
    );
    Failure::usage("own_machine", message)
}
