//! The client's side of `file get` and `file put`: stream a file's bytes
//! to or from the host (see `wire.rs` for the order of frames and bytes),
//! hash them as they pass, and check the host saw the same sha256. Only the
//! path, size and sha256 are printed, never the contents.

use std::path::{Path, PathBuf};

use iroh::endpoint::Connection;
use serde_json::{Value, json};
use tokio::fs::File;

use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::plow::Peer;
use crate::state::StateDir;
use crate::wire::{self, Request};

/// `latch file get REMOTE [LOCAL]`. Without LOCAL, the file lands in the
/// current directory under the remote file's name; an existing file there
/// is replaced (like `scp`).
pub async fn get(
    state: &StateDir,
    host: &Peer,
    remote_path: &str,
    local_dest: Option<PathBuf>,
) -> Result<Value, Failure> {
    let local = match local_dest {
        Some(path) => path,
        None => match Path::new(remote_path).file_name() {
            Some(name) => PathBuf::from(name),
            None => {
                let message = format!("{remote_path:?} has no file name; give a local destination");
                return Err(Failure::usage("no_file_name", message));
            }
        },
    };
    let link = dial::connect(state, host).await?;
    let result = receive(&link.connection, remote_path, &local).await;
    link.close().await;
    result
}

async fn receive(
    connection: &Connection,
    remote_path: &str,
    local: &Path,
) -> Result<Value, Failure> {
    let request = Request {
        op: "file_get".to_string(),
        args: json!({ "remote_path": remote_path }),
    };
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let size = size_of(&dial::answer(connection, &mut recv, ANSWER_WITHIN).await?)?;

    let mut file = File::create(local)
        .await
        .map_err(|err| local_failure(local, err))?;
    let copied = wire::copy(&mut recv, &mut file, size).await;
    let Ok(sha256) = copied else {
        // Don't leave half a file behind.
        let _ = tokio::fs::remove_file(local).await;
        return Err(dial::lost(connection, true));
    };
    let done = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    check_same(&done, &sha256)?;
    Ok(json!({
        "path": local,
        "remote_path": remote_path,
        "size": size,
        "sha256": sha256,
        "audit_id": done["audit_id"],
    }))
}

/// `latch file put LOCAL REMOTE`.
pub async fn put(
    state: &StateDir,
    host: &Peer,
    local: &Path,
    remote_path: &str,
) -> Result<Value, Failure> {
    let mut file = File::open(local)
        .await
        .map_err(|err| local_failure(local, err))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|err| local_failure(local, err))?;
    if !metadata.is_file() {
        return Err(Failure::usage(
            "not_a_file",
            format!("{} is not a file", local.display()),
        ));
    }
    let size = metadata.len();

    // Sent along so a host can put the file inside a folder named as the
    // target, as `cp` does.
    let name = local.file_name().and_then(|name| name.to_str());
    let link = dial::connect(state, host).await?;
    let result = send(&link.connection, &mut file, size, remote_path, name).await;
    link.close().await;
    let mut result = result?;
    result["local_path"] = json!(local);
    Ok(result)
}

async fn send(
    connection: &Connection,
    file: &mut File,
    size: u64,
    remote_path: &str,
    name: Option<&str>,
) -> Result<Value, Failure> {
    let request = Request {
        op: "file_put".to_string(),
        args: json!({ "remote_path": remote_path, "size": size, "name": name }),
    };
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    // Wait for "go ahead": the host has the file open, or says why not.
    dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;

    let sha256 = wire::copy(file, &mut send, size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let done = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    check_same(&done, &sha256)?;
    Ok(json!({
        // Where the host put it: inside `remote_path` if that is a folder.
        "path": done["path"].as_str().unwrap_or(remote_path),
        "size": size,
        "sha256": sha256,
        "audit_id": done["audit_id"],
    }))
}

/// The byte count in the host's first answer to `file_get`.
fn size_of(start: &Value) -> Result<u64, Failure> {
    start["size"].as_u64().ok_or_else(|| {
        let message = "the host's answer did not say how many bytes follow".to_string();
        Failure::new(Exit::Unknown, "bad_response", message)
    })
}

/// Both sides hashed the bytes as they passed; they must agree.
fn check_same(done: &Value, sha256: &str) -> Result<(), Failure> {
    if done["sha256"] == sha256 {
        return Ok(());
    }
    let message = format!("the host saw sha256 {}, this side {sha256}", done["sha256"]);
    Err(Failure::failed("checksum_mismatch", message))
}

fn local_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::usage("local_file", format!("{}: {err}", path.display()))
}
