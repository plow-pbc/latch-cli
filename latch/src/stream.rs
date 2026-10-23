//! Every stream latch prints (`audit`, `status --follow`, `logs`,
//! `login --events`) is JSON lines, each with a `type`, the first `hello`
//! with the `schema` its lines follow. A reader ignores fields it does not
//! know.

use serde_json::{Value, json};

/// The version of the streams' lines, which `hello` says.
pub const SCHEMA: u32 = 1;

/// The first line of every stream.
pub fn hello() -> Value {
    json!({"type": "hello", "schema": SCHEMA})
}
