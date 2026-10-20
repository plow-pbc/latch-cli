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
    chat: Option<String>,
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
    let result = receive(&link.connection, chat, remote_path, &local).await;
    link.close().await;
    result
}

async fn receive(
    connection: &Connection,
    chat: Option<String>,
    remote_path: &str,
    local: &Path,
) -> Result<Value, Failure> {
    let request = Request {
        op: "file_get".to_string(),
        args: json!({ "remote_path": remote_path }),
        chat,
    };
    let (start, done, sha256) = download(connection, &request, local).await?;
    Ok(json!({
        "path": local,
        "remote_path": remote_path,
        "size": start["size"],
        "sha256": sha256,
        "audit_id": done["audit_id"],
    }))
}

/// `latch file put LOCAL REMOTE`.
pub async fn put(
    state: &StateDir,
    host: &Peer,
    chat: Option<String>,
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
    let result = send(&link.connection, chat, &mut file, size, remote_path, name).await;
    link.close().await;
    let mut result = result?;
    result["local_path"] = json!(local);
    Ok(result)
}

async fn send(
    connection: &Connection,
    chat: Option<String>,
    file: &mut File,
    size: u64,
    remote_path: &str,
    name: Option<&str>,
) -> Result<Value, Failure> {
    let request = Request {
        op: "file_put".to_string(),
        args: json!({ "remote_path": remote_path, "size": size, "name": name }),
        chat,
    };
    let (done, sha256) = upload(connection, &request, file, size).await?;
    check_same(&done, &sha256)?;
    Ok(json!({
        // Where the host put it: inside `remote_path` if that is a folder.
        "path": done["path"].as_str().unwrap_or(remote_path),
        "size": size,
        "sha256": sha256,
        "audit_id": done["audit_id"],
    }))
}

/// Sends `request`, then `size` bytes of `file` once the host says go
/// ahead (the host has somewhere to put them, or says why not). Returns
/// the host's last answer and this side's sha256; whether that answer
/// carries a sha256 to check depends on the operation.
pub async fn upload(
    connection: &Connection,
    request: &Request,
    file: &mut File,
    size: u64,
) -> Result<(Value, String), Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let sha256 = wire::copy(file, &mut send, size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let done = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    Ok((done, sha256))
}

/// Sends `request`, then reads the host's first answer with the size,
/// exactly that many bytes into `local`, and the host's last answer, whose
/// sha256 must match. A file that broke off or does not match is removed.
/// Returns both answers and the sha256.
pub async fn download(
    connection: &Connection,
    request: &Request,
    local: &Path,
) -> Result<(Value, Value, String), Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let start = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let size = size_of(&start)?;
    let mut file = File::create(local)
        .await
        .map_err(|err| local_failure(local, err))?;
    let Ok(sha256) = wire::copy(&mut recv, &mut file, size).await else {
        // Don't leave half a file behind.
        let _ = tokio::fs::remove_file(local).await;
        return Err(dial::lost(connection, true));
    };
    let done = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    if let Err(failure) = check_same(&done, &sha256) {
        let _ = tokio::fs::remove_file(local).await;
        return Err(failure);
    }
    Ok((start, done, sha256))
}

/// The byte count in the host's first answer to a download.
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

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode};
    use wire::Response;

    /// A download from a host that sends `bytes` of the `size` it promised,
    /// then `sha256` as the one it saw.
    async fn download_from(
        size: u64,
        bytes: &[u8],
        sha256: &str,
        local: &Path,
    ) -> Result<(), Failure> {
        let bind = |alpns: Vec<Vec<u8>>| {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(alpns)
                .bind()
        };
        let host = bind(vec![wire::ALPN.to_vec()]).await.unwrap();
        let client = bind(vec![]).await.unwrap();
        let accepting = host.accept();
        let (accepted, connection) =
            tokio::join!(async { accepting.await.unwrap().await.unwrap() }, async {
                client.connect(host.addr(), wire::ALPN).await.unwrap()
            });
        let serving = async {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            wire::read_frame(&mut recv).await.unwrap();
            let frame = |value| serde_json::to_vec(&Response::Ok(value)).unwrap();
            wire::write_frame(&mut send, &frame(json!({"size": size})))
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut send, bytes)
                .await
                .unwrap();
            if bytes.len() as u64 == size {
                wire::write_frame(&mut send, &frame(json!({"sha256": sha256})))
                    .await
                    .unwrap();
            }
            send.finish().unwrap();
            let _ = send.stopped().await;
        };
        let request = Request {
            op: "file_get".into(),
            args: json!({}),
            chat: None,
        };
        let ((), result) = tokio::join!(serving, download(&connection, &request, local));
        client.close().await;
        host.close().await;
        result.map(|_| ())
    }

    #[tokio::test]
    async fn a_download_that_breaks_off_or_does_not_match_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("got");
        let sha256 = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(b"hello"));

        download_from(5, b"hello", &sha256, &local).await.unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");

        let wrong = download_from(5, b"hello", "00", &local).await.unwrap_err();
        assert_eq!(wrong.code, "checksum_mismatch");
        assert!(!local.exists(), "a file that does not match is removed");

        let short = download_from(5, b"hel", &sha256, &local).await.unwrap_err();
        assert_eq!(short.code, "outcome_unknown");
        assert!(!local.exists(), "a file that broke off is removed");
    }
}
