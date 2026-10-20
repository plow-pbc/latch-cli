//! The host's registration loop (§5.1): register with Plow, then again every
//! 10 minutes ±20% so hosts never synchronise. That repeat is the heartbeat
//! that keeps the host listed as reachable. After a successful registration,
//! a changed relay address triggers an immediate re-registration.
//!
//! When Plow fails, the loop waits longer and longer (doubling, with jitter,
//! capped at an hour, and never before Plow's `Retry-After`), and nothing
//! cuts that wait short, so it never calls Plow in a tight loop.

use std::future::Future;
use std::time::Duration;

use iroh::{Endpoint, Watcher};

use crate::dashboard::Dashboard;
use crate::failure::Failure;
use crate::log::log;
use crate::plow::{Plow, Registration};
use crate::state::{Config, StateDir};

const HEARTBEAT: Duration = Duration::from_secs(10 * 60);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);

/// Runs until Plow refuses this host's registration outright (409: its
/// name now belongs to another key), and returns why. `config` is what
/// `latch login` saved; each successful registration saves Plow's latest
/// record of this host back into it.
pub async fn run(
    plow: Plow,
    endpoint: Endpoint,
    state: StateDir,
    mut config: Config,
    dashboard: Option<Dashboard>,
) -> Failure {
    let mut addresses = endpoint.watch_addr();
    let mut failures = 0;
    loop {
        let relay_url = home_relay(&endpoint);
        let registration = Registration {
            key: endpoint.id().to_string(),
            name: Some(config.peer.name.clone()),
            relay_url: relay_url.clone(),
        };
        let (registered, wait) = match plow.register(&registration).await {
            Ok(peer) => {
                failures = 0;
                if let Some(dashboard) = &dashboard {
                    dashboard.heartbeat();
                }
                let wait = with_jitter(HEARTBEAT);
                let relay = relay_url.as_deref().unwrap_or("none");
                log(&format!(
                    "heartbeat: registered (relay {relay}); next in {:.1}s",
                    wait.as_secs_f64()
                ));
                // Keep config.json current, so a later `latch login` re-sends
                // this relay instead of unlisting a running host.
                config.peer = peer;
                if let Err(failure) = state.save_config(&config) {
                    log(&format!(
                        "heartbeat: could not save config: {}",
                        failure.message
                    ));
                }
                (true, wait)
            }
            // Retrying cannot help: someone ran `latch login` with this name
            // on another machine, which moved the name to that machine's key.
            Err(failure) if failure.code == "conflict" => {
                let name = &config.peer.name;
                let message = format!(
                    "Plow gave the name {name:?} to another machine (a `latch login` there took it over), \
                     so this host stops; run `latch login` here to take the name back"
                );
                log(&format!("heartbeat: refused: {message}"));
                return Failure::failed("name_moved", message);
            }
            Err(failure) => {
                failures += 1;
                let wait = backoff(failures, failure.retry_after);
                log(&format!(
                    "heartbeat: failed ({}: {}); retry {failures} in {:.1}s",
                    failure.code,
                    failure.message,
                    wait.as_secs_f64()
                ));
                (false, wait)
            }
        };

        // A relay change may cut the wait short only after a success; after a
        // failure, the backoff (and Plow's Retry-After) always runs out first.
        let relay_changed = relay_changes(&mut addresses, &relay_url);
        if pause(wait, registered, relay_changed).await {
            log("heartbeat: relay changed; registering again now");
        }
    }
}

/// Sleeps for `wait`. If `may_wake_early`, it returns as soon as
/// `relay_changed` finishes instead. Returns whether it woke early.
async fn pause(
    wait: Duration,
    may_wake_early: bool,
    relay_changed: impl Future<Output = ()>,
) -> bool {
    if !may_wake_early {
        tokio::time::sleep(wait).await;
        return false;
    }
    // `select!` runs both and continues with whichever finishes first.
    tokio::select! {
        () = tokio::time::sleep(wait) => false,
        () = relay_changed => true,
    }
}

/// The relay this endpoint is reachable through, as a URL string.
fn home_relay(endpoint: &Endpoint) -> Option<String> {
    endpoint
        .addr()
        .relay_urls()
        .next()
        .map(|url| url.to_string())
}

/// Returns once the endpoint's relay differs from `current`.
async fn relay_changes(
    addresses: &mut impl Watcher<Value = iroh::EndpointAddr>,
    current: &Option<String>,
) {
    loop {
        let Ok(address) = addresses.updated().await else {
            // The endpoint is shutting down; let the timer decide instead.
            return std::future::pending().await;
        };
        let relay = address.relay_urls().next().map(|url| url.to_string());
        if relay != *current {
            return;
        }
    }
}

/// How long to wait after `failures` failed attempts in a row: 2s, 4s, 8s,
/// ... with ±20% jitter, at most an hour; but never less than Plow's
/// `Retry-After`, even when Plow asks for more than an hour.
pub fn backoff(failures: u32, retry_after: Option<Duration>) -> Duration {
    let doubled = FIRST_RETRY.saturating_mul(2u32.saturating_pow(failures.saturating_sub(1)));
    let exponential = with_jitter(doubled).min(MAX_BACKOFF);
    exponential.max(retry_after.unwrap_or_default())
}

/// `base` scaled by a random factor between 0.8 and 1.2.
fn with_jitter(base: Duration) -> Duration {
    base.mul_f64(rand::random_range(0.8..1.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeats_are_ten_minutes_give_or_take_twenty_percent() {
        for _ in 0..100 {
            let wait = with_jitter(HEARTBEAT).as_secs();
            assert!((480..=720).contains(&wait), "{wait}");
        }
    }

    #[test]
    fn backoff_doubles_stops_at_an_hour_and_honours_a_later_retry_after() {
        let secs = |failures, retry| backoff(failures, retry).as_secs_f64();
        assert!((1.6..=2.4).contains(&secs(1, None)));
        assert!((12.8..=19.2).contains(&secs(4, None)));
        assert_eq!(secs(4, Some(Duration::from_secs(120))), 120.0);
        assert!((2880.0..=3600.0).contains(&secs(40, None)));
        // The hour caps our own doubling, not the server's deadline.
        assert_eq!(secs(1, Some(Duration::from_secs(7200))), 7200.0);
    }

    #[tokio::test]
    async fn a_relay_change_ends_the_wait_only_after_a_success() {
        let wait = Duration::from_millis(300);
        let started = std::time::Instant::now();
        assert!(pause(wait, true, std::future::ready(())).await);
        assert!(started.elapsed() < Duration::from_millis(100));

        let started = std::time::Instant::now();
        assert!(!pause(wait, false, std::future::ready(())).await);
        assert!(started.elapsed() >= wait);
    }
}
