//! The host's handle on one session: its runtime's worker (`runtime.rs`).
//! Its calls go to the worker one at a time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::host::add_time;
pub use super::runtime::Reply;
use super::runtime::Runtime;
use crate::failure::Failure;

pub struct Session {
    pub runtime: Arc<Runtime>,
    /// What the worker said once the browser was up: engine, version, pid.
    pub ready: Value,
}

impl Session {
    /// The session `runtime` started its browser with.
    pub fn new(runtime: Arc<Runtime>) -> Self {
        let ready = runtime.ready.clone();
        Session { runtime, ready }
    }

    /// One call (a `core::Call` as JSON), with an upload's bytes after it.
    /// Bytes the worker sends back (a screenshot, a download) come back as
    /// `bytes`. Its time goes to the request's phases: what the worker says
    /// it spent waiting for pages to settle (its audit's `settle_ms`, taken
    /// out), and the rest as the browser's action.
    pub async fn call_with(&mut self, call: Value, input: Option<&[u8]>) -> Result<Reply, Failure> {
        let calling = Instant::now();
        let mut reply = self.runtime.call(call, input).await;
        let audit = match &mut reply {
            Ok(reply) => reply.audit.as_object_mut(),
            Err(failure) => failure
                .details
                .get_mut("audit")
                .and_then(Value::as_object_mut),
        };
        let settle = audit
            .and_then(|audit| audit.remove("settle_ms"))
            .and_then(|ms| ms.as_u64())
            .map(Duration::from_millis);
        if let Some(settle) = settle {
            add_time(|phases| &mut phases.settle_ms, settle);
        }
        let action = calling.elapsed().saturating_sub(settle.unwrap_or_default());
        add_time(|phases| &mut phases.action_ms, action);
        reply
    }

    pub async fn call(&mut self, call: Value) -> Result<Reply, Failure> {
        self.call_with(call, None).await
    }

    /// A runtime with this one session in it, for tests that drive the
    /// worker directly.
    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub async fn start_with(
        latch: &std::path::Path,
        state_dir: &std::path::Path,
        engine: &str,
        idle: std::time::Duration,
        headless: bool,
    ) -> Result<Self, Failure> {
        let runtime =
            Arc::new(Runtime::start_with(latch, state_dir, engine, idle, headless).await?);
        Ok(Session::new(runtime))
    }

    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub fn scratch(&self) -> std::path::PathBuf {
        self.runtime.scratch()
    }

    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub fn profile(&self) -> std::path::PathBuf {
        self.runtime.profile()
    }

    /// Closes the browser and waits for the worker to end.
    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub async fn close(self) -> Result<Value, Failure> {
        self.runtime.close().await
    }
}
