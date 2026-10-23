//! The host's handle on one session: its runtime's worker (`runtime.rs`).
//! Its calls go to the worker one at a time.

use std::sync::Arc;

use serde_json::Value;

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
    /// `bytes`.
    pub async fn call_with(&mut self, call: Value, input: Option<&[u8]>) -> Result<Reply, Failure> {
        self.runtime.call(call, input).await
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
