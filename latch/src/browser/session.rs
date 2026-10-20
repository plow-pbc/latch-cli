//! The host's handle on one session: its key in its profile runtime's
//! worker (`runtime.rs`). Its calls go to the worker one at a time, in turn
//! with the other sessions' calls, each carrying the sites other sessions
//! reserved.

use std::sync::Arc;

use serde_json::{Value, json};

pub use super::runtime::Reply;
use super::runtime::Runtime;
use crate::failure::Failure;

pub struct Session {
    pub runtime: Arc<Runtime>,
    key: String,
    /// What the worker said once the browser was up: engine, version, pid.
    pub ready: Value,
}

impl Session {
    /// Starts a session in `runtime`, with a window of its own.
    pub async fn begin(runtime: &Arc<Runtime>) -> Result<Self, Failure> {
        let key = runtime.new_key();
        runtime
            .call(json!({"op": "begin", "session": key}), None)
            .await?;
        Ok(Session {
            runtime: runtime.clone(),
            key,
            ready: runtime.ready.clone(),
        })
    }

    /// The worker's name for this session.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// One call (a `core::Call` as JSON), with an upload's bytes after it.
    /// Bytes the worker sends back (a screenshot, a download) come back as
    /// `bytes`.
    pub async fn call_with(
        &mut self,
        mut call: Value,
        input: Option<&[u8]>,
    ) -> Result<Reply, Failure> {
        call["session"] = json!(self.key);
        let reserved = self.runtime.reserved_by_others(&self.key);
        if !reserved.is_empty() {
            call["reserved"] = json!(reserved);
        }
        self.runtime.call(call, input).await
    }

    pub async fn call(&mut self, call: Value) -> Result<Reply, Failure> {
        self.call_with(call, None).await
    }

    /// Ends the session: its tabs close; the runtime's other sessions go on.
    pub async fn end(&mut self) -> Result<Value, Failure> {
        self.call(json!({"op": "end"}))
            .await
            .map(|reply| reply.result)
    }

    /// A runtime with this one session in it, for tests that drive the
    /// worker directly.
    #[cfg(all(test, target_os = "macos"))]
    pub async fn start_with(
        latch: &std::path::Path,
        state_dir: &std::path::Path,
        profile: &str,
        idle: std::time::Duration,
        headless: bool,
    ) -> Result<Self, Failure> {
        let runtime =
            Arc::new(Runtime::start_with(latch, state_dir, profile, idle, headless).await?);
        Session::begin(&runtime).await
    }

    #[cfg(all(test, target_os = "macos"))]
    pub fn scratch(&self) -> std::path::PathBuf {
        self.runtime.scratch()
    }

    /// Closes the browser and waits for the worker to end.
    #[cfg(all(test, target_os = "macos"))]
    pub async fn close(self) -> Result<Value, Failure> {
        self.runtime.close().await
    }
}
