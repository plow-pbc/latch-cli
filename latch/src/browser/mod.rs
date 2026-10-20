//! `latch browser`: a real browser on a host, driven over a private pipe.
//!
//! - `engine.rs`: the pinned engine plugins (Chrome for Testing, Camoufox)
//!   and their checks.
//! - `profile.rs`: latch-owned browser profiles in the state directory.
//! - `client.rs`: the agent's side; `latch browser …` commands.
//! - `host.rs`: the host's `browser` operation; sessions and their owners.
//! - `fill.rs`: the host's half of `login`, `fill` and `fill-code`.
//! - `plan.rs`: the canonical plan, and the short form that compiles to it.
//! - `steps.rs`: the host's step executor: `do`, `act`, confirmations,
//!   `find` and `wait`.
//! - `journal.rs`: each plan request's journal, for `do --history`.
//! - `model.rs`: the decision model, behind a swappable provider.
//! - `runtime.rs`: a profile's browser worker, as the host starts and calls it.
//! - `session.rs`: one session's handle on its profile's worker.
//! - `worker.rs`: the long-lived browser worker; runs each session's core.
//! - `core.rs`: the shared core: snapshots, refs, actions and results.
//! - `reader.js`: the page reader the core runs in every frame.
//! - `driver.rs`: the driver contract every engine implements.
//! - `cft.rs`: the Chrome for Testing driver.
//! - `cdp.rs`: the DevTools protocol over the engine's pipe.
//! - `camoufox.rs`: the Camoufox driver, through the helper in `camoufox/`.
//! - `tags.rs`: workaround tags for the audit of an agent's `eval`.

pub mod camoufox;
pub mod cdp;
pub mod cft;
pub mod client;
pub mod core;
pub mod display;
pub mod driver;
pub mod engine;
mod fill;
pub mod handoff;
pub mod host;
mod journal;
pub mod model;
pub mod plan;
pub mod profile;
pub mod runtime;
pub mod session;
mod steps;
pub mod tags;
// Uses macOS's own sandbox query; the Linux engine has its own tests.
#[cfg(all(test, target_os = "macos"))]
mod tests;
pub mod worker;
