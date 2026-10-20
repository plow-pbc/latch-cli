//! `latch browser`: a real browser on a host, driven over a private pipe.
//!
//! - `engine.rs`: the pinned engine plugin (Chrome for Testing) and its check.
//! - `profile.rs`: latch-owned browser profiles in the state directory.
//! - `client.rs`: the agent's side; `latch browser …` commands.
//! - `host.rs`: the host's `browser` operation; one session per profile.
//! - `fill.rs`: the host's half of `login`, `fill` and `fill-code`.
//! - `session.rs`: a session's worker, as the host starts and calls it.
//! - `worker.rs`: the long-lived browser worker; runs the core on its engine.
//! - `core.rs`: the shared core: snapshots, refs, actions and results.
//! - `reader.js`: the page reader the core runs in every frame.
//! - `driver.rs`: the driver contract every engine implements.
//! - `cft.rs`: the Chrome for Testing driver.
//! - `cdp.rs`: the DevTools protocol over the engine's pipe.
//! - `tags.rs`: workaround tags for the audit of an agent's `eval`.

pub mod cdp;
pub mod cft;
pub mod client;
pub mod core;
pub mod driver;
pub mod engine;
mod fill;
pub mod host;
pub mod model;
pub mod profile;
pub mod session;
mod steps;
pub mod tags;
// Uses macOS's own sandbox query; the Linux engine has its own tests.
#[cfg(all(test, target_os = "macos"))]
mod tests;
pub mod worker;
