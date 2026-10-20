//! Which Plow credential a peer presents.
//!
//! A host is given the owner's token once (`latch login --token-file`, or a
//! login by text) and keeps a copy in its state directory. An agent already holds its own Plow
//! credential for the `plow` CLI, so latch reads that one where it lives,
//! every time, instead of copying it: a rotated agent credential just works.

use std::fs;
use std::path::{Path, PathBuf};

use crate::failure::Failure;
use crate::state::StateDir;

/// Where a cloud agent's credential lives, unless `$PLOW_TOKEN_FILE` says
/// otherwise (the same lookup as the `plow` CLI).
const AGENT_TOKEN_FILE: &str = "/config/secrets/plow-api-token";

/// The token a peer uses: the one saved at login, else the agent's own.
pub fn token(state: &StateDir) -> Result<String, Failure> {
    match state.load_token()? {
        Some(token) => Ok(token),
        None => agent_token(),
    }
}

fn agent_token() -> Result<String, Failure> {
    read(&agent_token_path())
}

/// The agent's own credential, or `None` if this machine has none.
pub fn agent_token_if_present() -> Result<Option<String>, Failure> {
    let path = agent_token_path();
    if !path.exists() {
        return Ok(None);
    }
    read(&path).map(Some)
}

fn agent_token_path() -> PathBuf {
    match std::env::var_os("PLOW_TOKEN_FILE") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(AGENT_TOKEN_FILE),
    }
}

/// Reads a token file. Errors name the file, never its contents.
pub fn read(path: &Path) -> Result<String, Failure> {
    match fs::read_to_string(path) {
        Ok(token) => Ok(token.trim().to_string()),
        Err(err) => {
            let message = format!("cannot read the Plow credential {}: {err}", path.display());
            Err(Failure::usage("no_credential", message))
        }
    }
}
