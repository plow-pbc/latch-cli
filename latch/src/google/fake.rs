//! A stand-in for gog, for tests only (feature `fake-gog`, which cannot
//! build in release): this latch binary, copied in as the host's gog,
//! answers a mailbox of one made-up order email instead of calling
//! Google. The host accepts it because its bytes hold `MARKER`, as only
//! such a build's do.
//!
//! The token the test's fake Plow mints for the account is `shop=ORIGIN`,
//! the test shop the email links to.

use std::process::ExitCode;

use serde_json::{Value, json};

#[cfg(not(debug_assertions))]
compile_error!("the fake-gog feature is for tests only: it never builds in release");

/// A string only a build with the stand-in holds (each answer carries
/// it), which the release workflow checks its binaries never do.
pub const MARKER: &str = "latch-fake-gog-test-only";

/// Runs as gog when this binary was started under that name.
pub fn run_if_gog() -> Option<ExitCode> {
    let name = std::env::args_os().next()?;
    if std::path::Path::new(&name).file_name()? != "gog" {
        return None;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let token = std::env::var("GOG_ACCESS_TOKEN").unwrap_or_default();
    let shop = token.strip_prefix("shop=").unwrap_or("http://127.0.0.1:9");
    match answer(&args, shop) {
        Some(mut value) => {
            value["fake"] = json!(MARKER);
            println!("{value}");
            Some(ExitCode::SUCCESS)
        }
        // gog's exit code for a request it cannot serve.
        None => Some(ExitCode::from(2)),
    }
}

/// gog's JSON for the command in `args`, after its global flags.
fn answer(args: &[String], shop: &str) -> Option<Value> {
    let at = args.iter().position(|arg| arg == "gmail")?;
    let words: Vec<&str> = args[at + 1..]
        .iter()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'))
        .collect();
    let link = format!("{shop}/orders.html?order=A1001");
    match words.as_slice() {
        ["search", ..] | ["messages", "search", ..] => Some(json!({
            "threads": [{
                "id": "thread-a1001",
                "date": "2026-10-18 09:12",
                "from": "Greenleaf Market <orders@shop.example>",
                "subject": "Your Greenleaf Market order A1001 has shipped",
                "labels": ["INBOX"],
                "messageCount": 1,
            }],
            "nextPageToken": "",
        })),
        ["get", id, ..] | ["thread", "get", id, ..] if id.contains("a1001") => Some(json!({
            "id": "msg-a1001",
            "threadId": "thread-a1001",
            "from": "Greenleaf Market <orders@shop.example>",
            "subject": "Your Greenleaf Market order A1001 has shipped",
            "body": format!(
                "Thanks for your order A1001.\n\
                 2 x White tank (M)\n1 x Oxford shirt (L)\n1 x Wool socks\n1 x Canvas belt\n\n\
                 View or return your order: {link}\n"
            ),
        })),
        _ => None,
    }
}
