//! `latch browser`: a real browser on a host, driven over a private pipe.
//!
//! - `engine.rs`: the pinned engine plugins (Chrome for Testing, Camoufox)
//!   and their checks.
//! - `profile.rs`: latch-owned browser profiles in the state directory.
//! - `client.rs`: the agent's side; `latch browser …` commands.
//! - `host.rs`: the host's `browser` operation; sessions and their owners.
//! - `fill.rs`: the host's half of `fill`, for every vault kind.
//! - `plan.rs`: the canonical plan, and the short form that compiles to it.
//! - `suggest.rs`: what a fill may use on a page: offers, suggested
//!   mappings, and the checks every mapping passes.
//! - `steps.rs`: the host's step executor: `do`, `act`, confirmations,
//!   `find` and `wait`.
//! - `journal.rs`: each plan request's journal, for `do --history`.
//! - `model.rs`: the decision model, behind a swappable provider.
//! - `runtime.rs`: a profile's browser worker, as the host starts and calls it.
//! - `session.rs`: one session's handle on its profile's worker.
//! - `worker.rs`: the long-lived browser worker; runs each session's core.
//! - `core.rs`: the shared core: snapshots, refs, actions and results.
//! - `shown.rs`: what an answer shows the agent of a page.
//! - `reader.js`: the page reader the core runs in every frame.
//! - `driver.rs`: the driver contract every engine implements.
//! - `egress.rs`: where the browser may go, and the proxy it goes through.
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
pub mod egress;
pub mod engine;
mod fill;
pub mod handoff;
pub mod host;
mod journal;
mod judged;
pub mod model;
pub mod plan;
pub mod profile;
pub mod runtime;
pub mod session;
mod shown;
mod steps;
pub mod suggest;
pub mod tags;
// Real engines: on macOS always, on Linux only where one is installed.
#[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
mod tests;
pub mod worker;
