//! Keeps this host's vault in sync with the owner's other hosts (see
//! `vault/sync.rs`). With each that is online, in turn: ask for its vault
//! state, apply it here, then send this host's back for it to apply. A
//! round runs when the host starts, when the vault file changes (a write
//! by serve or by a local `latch vault` command), and every few minutes,
//! so a host that was asleep or offline catches up, and a change made
//! while no other host was online waits here until one is. A host whose
//! vault does not sync (`vault::host::syncs`) runs no rounds, and refuses
//! another's.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::{Host, Stopping};
use crate::audit;
use crate::dial;
use crate::failure::{Failure, code};
use crate::log::log;
use crate::plow::Peer;
use crate::transport::Connection;
use crate::vault::store::Applied;
use crate::vault::sync::State;
use crate::wire::Request;

/// How often the vault file is checked for a change.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How often a round runs with no change here, to catch up with the
/// other hosts' changes and to reach a host that was away. Measured on the
/// wall clock, so a Mac that slept runs one as it wakes.
const ROUND_EVERY: Duration = Duration::from_secs(10 * 60);
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// Starts this host's rounds, if its vault syncs. They end as the host
/// stops.
pub(super) fn start(host: &Arc<Host>) {
    if !crate::vault::host::syncs(&host.workers.state_dir) {
        return;
    }
    log("vault: syncs with the owner's other Macs");
    let host = host.clone();
    tokio::spawn(async move {
        let file = host.workers.state_dir.join("vault.json");
        let modified = || std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let mut stopping = host.stopping.subscribe();
        let mut synced: Option<(Option<SystemTime>, SystemTime)> = None;
        loop {
            let now = modified();
            let due = |at: SystemTime| at.elapsed().unwrap_or_default() >= ROUND_EVERY;
            if synced.is_none_or(|(seen, at)| seen != now || due(at)) {
                round(&host).await;
                synced = Some((now, SystemTime::now()));
            }
            tokio::select! {
                _ = stopping.wait_for(|stopping| *stopping != Stopping::No) => return,
                () = tokio::time::sleep(CHECK_EVERY) => {}
            }
        }
    });
}

/// One round: every other host of the owner's that Plow lists online. A
/// host whose vault does not sync answers `sync_off`, having been sent
/// nothing, and is asked again next round.
async fn round(host: &Host) {
    let peers = match host.plow.list_peers().await {
        Ok(peers) => peers,
        Err(failure) => {
            log(&format!("vault sync: no peer list: {}", failure.code));
            return;
        }
    };
    let own = host.endpoint.id().to_string();
    let others = crate::hosts::only_hosts(peers, &own)
        .into_iter()
        .filter(|peer| !peer.this_machine && peer.kind == "device" && peer.online != Some(false));
    for peer in others {
        if let Err(failure) = with_peer(host, &peer).await
            && failure.code != code::sync_off.name
        {
            log(&format!(
                "vault sync with {:?}: {}",
                peer.name, failure.code
            ));
        }
    }
}

async fn with_peer(host: &Host, peer: &Peer) -> Result<(), Failure> {
    let connection = dial::connect_on(&host.endpoint, peer, dial::CONNECT_TIMEOUT).await?;
    let connection = Connection::Remote(connection);
    let exchanged = exchange(host, peer, &connection).await;
    connection.close(0u32.into(), b"done");
    exchanged
}

async fn exchange(host: &Host, peer: &Peer, connection: &Connection) -> Result<(), Failure> {
    let ask = |state: Option<&State>| Request {
        op: "vault".into(),
        args: json!({"command": "sync", "state": state}),
        chat: None,
    };
    let mut answer = dial::one_exchange(connection, &ask(None), ANSWER_WITHIN).await?;
    let theirs: State = serde_json::from_value(answer["state"].take()).map_err(|_| {
        Failure::new(
            code::bad_response,
            "the host's vault state is not valid".into(),
        )
    })?;
    let (applied, ours) = host.vault.with(|store| {
        let applied = store.apply(theirs)?;
        Ok((applied, store.state().clone()))
    })?;
    host.synced_from(&peer.key, &peer.name, &applied).await?;
    dial::one_exchange(connection, &ask(Some(&ours)), ANSWER_WITHIN).await?;
    Ok(())
}

impl Host {
    /// Audits and logs what this vault took from the owner's host `name`
    /// (key `key`), if anything: counts only, never an item. The record's
    /// caller is the host the changes came from, whichever of the two asked.
    pub(super) async fn synced_from(
        &self,
        key: &str,
        name: &str,
        applied: &Applied,
    ) -> Result<(), Failure> {
        if !applied.any() {
            return Ok(());
        }
        let mut record = audit::record(key, None, "vault", "allow", "ok");
        record.caller_name = Some(name.to_string());
        record.args = json!({"command": "sync", "added": applied.added,
                             "updated": applied.updated, "removed": applied.removed});
        self.append(record)
            .await
            .map_err(|message| Failure::new(code::audit_failed, message))?;
        log(&format!(
            "vault sync: took {} added, {} updated, {} removed from {name:?}",
            applied.added, applied.updated, applied.removed
        ));
        Ok(())
    }
}
