//! The host's own log: one time-stamped line per event, never tokens or
//! contents. It goes to stderr, where a service manager collects it: a
//! managed host's is `serve.log` in the state directory (see `managed.rs`).

#[cfg(test)]
use std::sync::Mutex;

pub fn log(message: &str) {
    let line = format!("{} {message}", jiff::Timestamp::now());
    #[cfg(test)]
    CAPTURED
        .lock()
        .expect("captured log lock")
        .push(line.clone());
    eprintln!("{line}");
}

/// Every line logged in this test process, for tests to look through.
#[cfg(test)]
static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
pub fn captured() -> Vec<String> {
    CAPTURED.lock().expect("captured log lock").clone()
}
