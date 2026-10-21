//! `latch status --pretty`: the same facts as `latch status`'s JSON, laid
//! out for a person. Its point is the key art (see `keyart.rs`): an owner
//! can hold the art a host shows for itself next to the art an agent shows
//! for that host, and see at a glance whether the keys match.

use crate::keyart;
use crate::paint::{self, DIM, INK, VOLT};
use crate::plow::Peer;
use crate::status::StatusOutput;

/// How many host pictures fit side by side.
const ART_PER_ROW: usize = 3;

pub fn render(status: &StatusOutput, state_dir: &str) -> String {
    let peer = &status.peer;
    let mut out = Vec::new();
    out.push(format!(
        "{} {}",
        paint::bold(&format!("latch status · {}", peer.name), VOLT),
        paint::fg(&format!("({})", peer.kind), DIM)
    ));
    out.push(String::new());

    // This peer's art on the left, the facts on the right.
    let art = keyart::art(&peer.key, &peer.name).unwrap_or_default();
    // The key in groups of four, so it can be read out loud.
    let (first, second) = peer.key.split_at(peer.key.len() / 2);
    let facts = [
        String::new(),
        format!(
            "{}  {}",
            paint::fg("key  ", DIM),
            paint::fg(&grouped(first), INK)
        ),
        format!("       {}", paint::fg(&grouped(second), INK)),
        String::new(),
        format!("{}  {}", paint::fg("plow ", DIM), status.api_url),
        format!("{}  {}", paint::fg("state", DIM), state_dir),
        String::new(),
        paint::fg("same key, same picture: compare it with", DIM),
        paint::fg("what the other machine shows", DIM),
    ];
    for (i, line) in art.iter().enumerate() {
        match facts.get(i) {
            Some(fact) if !fact.is_empty() => {
                out.push(format!("{}   {fact}", paint::fg(line, VOLT)))
            }
            _ => out.push(paint::fg(line, VOLT)),
        }
    }

    out.push(String::new());
    out.push(paint::bold(&format!("hosts ({})", status.hosts.len()), INK));
    for host in &status.hosts {
        out.push(host_line(host));
    }
    for row in status.hosts.chunks(ART_PER_ROW) {
        out.push(String::new());
        let arts: Vec<Vec<String>> = row
            .iter()
            .filter_map(|host| keyart::art(&host.key, &host.name))
            .collect();
        for line in 0..keyart::ROWS + 2 {
            let pieces: Vec<String> = arts.iter().map(|art| paint::fg(&art[line], VOLT)).collect();
            out.push(pieces.join("  "));
        }
    }
    out.join("\n")
}

/// `516e8dd4…` as `516e 8dd4 …`.
fn grouped(hex: &str) -> String {
    let chars: Vec<char> = hex.chars().collect();
    let groups: Vec<String> = chars
        .chunks(4)
        .map(|group| group.iter().collect())
        .collect();
    groups.join(" ")
}

/// `● name   seen 3 min ago   relay host`. The dot is the directory's
/// `online` flag (a recent heartbeat), not proof the host can be reached.
fn host_line(host: &Peer) -> String {
    let (dot, color) = if host.online == Some(true) {
        ("●", VOLT)
    } else {
        ("○", DIM)
    };
    let seen = host.last_seen_at.as_deref().map(ago).unwrap_or_default();
    let relay = host.relay_url.as_deref().unwrap_or("-");
    let relay = relay.trim_start_matches("https://").trim_end_matches('/');
    let this = if host.this_machine {
        "  (this machine)"
    } else {
        ""
    };
    format!(
        "{} {}  {}  {}{}",
        paint::fg(dot, color),
        paint::bold(&format!("{:<16}", host.name), INK),
        paint::fg(&format!("{seen:<16}"), DIM),
        paint::fg(&format!("relay {relay}"), DIM),
        paint::fg(this, DIM)
    )
}

/// "seen 40 s ago", "seen 3 min ago", "seen 2 h ago", "seen 9 days ago".
fn ago(time: &str) -> String {
    let Ok(then) = time.parse::<jiff::Timestamp>() else {
        return String::new();
    };
    let seconds = (jiff::Timestamp::now().as_second() - then.as_second()).max(0);
    let text = match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        3600..86400 => format!("{} h", seconds / 3600),
        _ => format!("{} days", seconds / 86400),
    };
    format!("seen {text} ago")
}
