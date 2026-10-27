//! The host says in its log when its relay link changes: when it loses
//! the relay and has it back, and when another endpoint with this host's
//! key takes the relay over and when it gives it back. Clients reach a
//! host through its relay, so either way they cannot connect meanwhile,
//! though Plow still lists the host online.
//!
//! A takeover leaves the link connected: the relay keeps the displaced
//! connection open, sends it `SameEndpointIdConnected`, and sends it
//! `Healthy` once it is the one the relay delivers to again. iroh passes
//! those on only as tracing events, so a tracing layer (`layer`) watches
//! for them, whatever `LATCH_TRACE` says.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use iroh::{Endpoint, Watcher};
use iroh_relay::protos::relay::Status;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::log::log;

/// Where iroh's relay connection logs, and the span it names its relay in.
const ACTOR: &str = "iroh::socket::transports::relay::actor";
const RELAY_SPAN: &str = "active-relay";

/// Logs each time the endpoint loses its relay connection and each time it
/// has it back.
pub fn watch(endpoint: &Endpoint) {
    let mut statuses = endpoint.home_relay_status();
    tokio::spawn(async move {
        let mut lost: HashMap<String, Instant> = HashMap::new();
        let mut connected = connections(&statuses.get());
        // It ends once the endpoint is gone.
        while let Ok(now) = statuses.updated().await {
            let now = connections(&now);
            for (url, change) in connection_changes(&connected, &now) {
                match change {
                    Change::Lost(error) => {
                        lost.insert(url.clone(), Instant::now());
                        let why = error.map(|error| format!(": {error}")).unwrap_or_default();
                        log(&format!(
                            "relay: lost {url}{why}; clients cannot connect until it is back"
                        ));
                    }
                    Change::Back => {
                        log(&format!(
                            "relay: connected again to {url}{}",
                            after(lost.remove(&url))
                        ));
                    }
                }
            }
            connected = now;
        }
    });
}

/// Each home relay's URL, whether it is connected, and its latest error.
type Connections = Vec<(String, bool, Option<String>)>;

fn connections(statuses: &[iroh::endpoint::RelayStatus]) -> Connections {
    statuses
        .iter()
        .map(|status| {
            let error = status.last_error().map(|error| error.to_string());
            (status.url().to_string(), status.is_connected(), error)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Change {
    Lost(Option<String>),
    Back,
}

/// The relays whose connection went down or came up between `was` and
/// `now`. A relay that is new, or gone, is the heartbeat's to report.
fn connection_changes(was: &Connections, now: &Connections) -> Vec<(String, Change)> {
    now.iter()
        .filter_map(|(url, up, error)| {
            let (_, was_up, _) = was.iter().find(|(before, _, _)| before == url)?;
            match (was_up, up) {
                (true, false) => Some((url.clone(), Change::Lost(error.clone()))),
                (false, true) => Some((url.clone(), Change::Back)),
                _ => None,
            }
        })
        .collect()
}

/// " after 1.2s", since `since`, if known.
fn after(since: Option<Instant>) -> String {
    since
        .map(|at| format!(" after {:.1}s", at.elapsed().as_secs_f64()))
        .unwrap_or_default()
}

/// The tracing layer that logs a takeover of this host's relay link and
/// its end. It sees iroh's relay connection events, and no others.
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    Takeovers::default().with_filter(Targets::new().with_target(ACTOR, Level::INFO))
}

#[derive(Default)]
struct Takeovers {
    /// When each relay began delivering to another endpoint, by URL.
    taken: Mutex<HashMap<String, Instant>>,
}

/// The relay a span of iroh's relay connection is for.
struct RelayUrl(String);

impl<S> Layer<S> for Takeovers
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != RELAY_SPAN {
            return;
        }
        let mut url = Text("url", None);
        attrs.record(&mut url);
        if let (Some(url), Some(span)) = (url.1, ctx.span(id)) {
            span.extensions_mut().insert(RelayUrl(url));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut message = Text("message", None);
        event.record(&mut message);
        let Some(message) = message.1 else { return };
        let taken = message.ends_with(&Status::SameEndpointIdConnected.to_string());
        if !taken && !message.ends_with(&Status::Healthy.to_string()) {
            return;
        }
        let url = ctx
            .event_scope(event)
            .into_iter()
            .flatten()
            .find_map(|span| span.extensions().get::<RelayUrl>().map(|url| url.0.clone()))
            .unwrap_or_else(|| "its relay".to_string());
        let mut since = self.taken.lock().expect("relay takeover lock");
        if taken {
            since.insert(url.clone(), Instant::now());
            log(&format!(
                "relay: another endpoint with this host's key connected to {url}, which now \
                 delivers to it instead; clients cannot connect until it goes"
            ));
        } else {
            log(&format!(
                "relay: {url} delivers to this host again{}",
                after(since.remove(&url))
            ));
        }
    }
}

/// One field of a span or event, as text.
struct Text(&'static str, Option<String>);

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == self.0 {
            self.1 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == self.0 {
            self.1 = Some(format!("{value:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_relay_connection_going_down_or_coming_up_is_a_change() {
        let up = |url: &str| (url.to_string(), true, None);
        let down = |url: &str, error: &str| (url.to_string(), false, Some(error.to_string()));
        let was = vec![up("a"), down("b", "refused")];
        let now = vec![down("a", "timed out"), up("b"), up("new")];
        assert_eq!(
            connection_changes(&was, &now),
            [
                ("a".to_string(), Change::Lost(Some("timed out".into()))),
                ("b".to_string(), Change::Back),
            ]
        );
        // A new error on a relay that is still down is no change.
        assert!(connection_changes(&now, &vec![down("a", "refused"), up("b")]).is_empty());
    }
}
