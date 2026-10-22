//! `latch logout`: this state folder's secrets deleted (its key, Plow
//! token, vault key and audit keys), from the Keychain on a signed macOS
//! latch and as files on any other, once any host on it has stopped. Its
//! other files stay; without the vault and audit keys, its vault and audit
//! log no longer open. This machine stays in Plow's directory: `latch
//! devices remove` from another of the owner's machines takes it out.

use serde_json::{Value, json};

use crate::failure::Failure;
use crate::lifecycle;
use crate::managed;
use crate::secrets::Secrets;
use crate::state::StateDir;

pub async fn run(state: &StateDir) -> Result<Value, Failure> {
    let Ok(dir) = state.path.canonicalize() else {
        return Ok(json!({"removed": [], "stopped": false, "state_dir": state.path}));
    };
    let stopped = managed::stop(state).await?;
    // A host started since would go on with secrets it already read.
    if lifecycle::now(&dir).running {
        let message = format!(
            "a host started on {} while it was being stopped; `latch stop`, then log out again",
            dir.display()
        );
        return Err(Failure::failed("host_running", message));
    }
    let secrets = Secrets::open(&dir)?;
    let removed = secrets.remove_all()?;
    Ok(json!({
        "removed": removed,
        "from": if secrets.in_keychain() { "keychain" } else { "files" },
        "stopped": stopped["stopped"],
        "state_dir": dir,
    }))
}
