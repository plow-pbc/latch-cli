//! The client's side of `file get` and `file put`: stream a file's bytes
//! to or from the host (see `wire.rs` for the order of frames and bytes),
//! hash them as they pass, and check the host saw the same sha256. Only the
//! path, size and sha256 are printed, never the contents.

use std::path::{Path, PathBuf};

use iroh::endpoint::{Connection, RecvStream};
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
/// exactly that many bytes, and the host's last answer, whose sha256 must
/// match. The bytes go into a temporary file beside `local`, which takes
/// `local`'s place only once they match: a download that broke off, does
/// not match, or whose process was killed never leaves a file (or half of
/// one) under `local`'s name. Returns both answers and the sha256.
pub async fn download(
    connection: &Connection,
    request: &Request,
    local: &Path,
) -> Result<(Value, Value, String), Failure> {
    let (mut send, mut recv) = dial::start(connection, request).await?;
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let start = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let size = size_of(&start)?;
    let temp = crate::worker::temp_path(local).map_err(|err| local_failure(local, err))?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
        .map_err(|err| local_failure(local, err))?;
    let result = receive_into(connection, &mut recv, &mut file, size).await;
    let result = match result {
        Ok((done, sha256)) => tokio::fs::rename(&temp, local)
            .await
            .map(|()| (start, done, sha256))
            .map_err(|err| local_failure(local, err)),
        Err(failure) => Err(failure),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// `size` bytes into `file`, and the host's last answer, which must carry
/// the same sha256.
async fn receive_into(
    connection: &Connection,
    recv: &mut RecvStream,
    file: &mut File,
    size: u64,
) -> Result<(Value, String), Failure> {
    let sha256 = wire::copy(recv, file, size)
        .await
        .map_err(|_| dial::lost(connection, true))?;
    let done = dial::answer(connection, recv, ANSWER_WITHIN).await?;
    check_same(&done, &sha256)?;
    Ok((done, sha256))
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
    use iroh::endpoint::SendStream;
    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode};
    use wire::Response;

    /// A host and a client endpoint, and both ends of a connection between them.
    async fn connected() -> (Endpoint, Endpoint, Connection, Connection) {
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
        (host, client, accepted, connection)
    }

    /// Answers a download with the `size` it promises, then `bytes`.
    async fn start_sending(accepted: &Connection, size: u64, bytes: &[u8]) -> SendStream {
        let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
        wire::read_frame(&mut recv).await.unwrap();
        let frame = serde_json::to_vec(&Response::Ok(json!({"size": size}))).unwrap();
        wire::write_frame(&mut send, &frame).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut send, bytes)
            .await
            .unwrap();
        send
    }

    fn file_get() -> Request {
        Request {
            op: "file_get".into(),
            args: json!({}),
            chat: None,
        }
    }

    /// A download from a host that sends `bytes` of the `size` it promised,
    /// then `sha256` as the one it saw.
    async fn download_from(
        size: u64,
        bytes: &[u8],
        sha256: &str,
        local: &Path,
    ) -> Result<(), Failure> {
        let (host, client, accepted, connection) = connected().await;
        let serving = async {
            let mut send = start_sending(&accepted, size, bytes).await;
            let frame = |value| serde_json::to_vec(&Response::Ok(value)).unwrap();
            if bytes.len() as u64 == size {
                wire::write_frame(&mut send, &frame(json!({"sha256": sha256})))
                    .await
                    .unwrap();
            }
            send.finish().unwrap();
            let _ = send.stopped().await;
        };
        let request = file_get();
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

        // Neither leaves a file, and an earlier one under the name stays as it was.
        let fresh = dir.path().join("fresh");
        for target in [&local, &fresh] {
            let wrong = download_from(5, b"hellO", &sha256, target)
                .await
                .unwrap_err();
            assert_eq!(wrong.code, "checksum_mismatch");
            let short = download_from(5, b"hel", &sha256, target).await.unwrap_err();
            assert_eq!(short.code, "outcome_unknown");
        }
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");
        assert!(!fresh.exists());
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left: {names:?}");
    }

    #[tokio::test]
    async fn a_download_killed_halfway_leaves_nothing_under_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("got");
        std::fs::write(&local, "the old file").unwrap();
        let (host, client, accepted, connection) = connected().await;
        // Dropped halfway, as a killed client is: no cleanup of its own runs.
        let request = file_get();
        let download = download(&connection, &request, &local);
        let (_send, cut) = tokio::join!(
            start_sending(&accepted, 1_000_000, &[7; 100_000]),
            tokio::time::timeout(std::time::Duration::from_millis(500), download)
        );
        assert!(cut.is_err(), "the download was still waiting for bytes");
        let left = std::fs::read(&local).unwrap();
        assert!(
            left == b"the old file",
            "{} bytes there instead",
            left.len()
        );
        client.close().await;
        host.close().await;
    }
}
