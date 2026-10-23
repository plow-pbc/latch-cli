//! The host's side of `file get` and `file put`. A sandboxed worker
//! (`worker.rs`) reads every file, and receives every upload into a temp
//! folder of its own; the host passes the bytes between it and the stream,
//! hashing them on the way, so the audit log can record each file's path,
//! size and sha256 (never its contents). The host itself moves a complete
//! upload to its destination (`place`).

mod place;

use std::path::Path;

use crate::transport::{RecvStream, SendStream};
use iroh::endpoint::VarInt;
use serde_json::{Value, json};

use place::{destination, source};

use crate::failure::code;
use crate::serve::Done;
use crate::wire::{self, Response};
use crate::worker::{WorkerError, WorkerSetup};

/// `file_get`: the worker reads the file; its bytes go to the client.
pub async fn get(workers: &WorkerSetup, args: &Value, send: &mut SendStream) -> Done {
    let Some(path) = args["remote_path"].as_str() else {
        return bad_arguments();
    };
    let (mut worker, first) = match workers.start_read(Path::new(path)).await {
        Ok(started) => started,
        Err(error) => return refused(path, error),
    };
    let Some(size) = first["size"].as_u64() else {
        return refused(path, worker.finish().await.err().unwrap_or_else(unexpected));
    };

    // First answer: how many bytes follow.
    let start = serde_json::to_vec(&Response::Ok(json!({ "size": size }))).expect("serializes");
    if wire::write_frame(send, &start).await.is_err() {
        return lost(path);
    }
    let Ok(sha256) = wire::copy(&mut worker.stdout, send, size).await else {
        // The client left, or the file ended early. Stop the worker too, so
        // it is not left waiting to write to a pipe nobody reads.
        let _ = worker.child.kill().await;
        return broken_off(send, path, unexpected());
    };
    match worker.finish().await {
        Ok(()) => moved(path, size, sha256),
        Err(error) => broken_off(send, path, error),
    }
}

/// Resolves a get's or put's `remote_path` in `args` to the host path it
/// reads or writes (`source`, `destination`), before it is judged, so the
/// judge and the audit see that path. Answers the refusal if there is none.
pub fn resolve(op: &str, args: &mut Value, workers: &WorkerSetup) -> Option<Done> {
    let remote = args["remote_path"].as_str()?.to_string();
    let resolved = match op {
        "file_get" => source(&remote, workers),
        "file_put" => destination(&remote, args["name"].as_str(), workers),
        _ => return None,
    };
    let path = match resolved {
        Ok(path) => path,
        Err(failure) => {
            return Some(Done {
                response: Response::error(&failure),
                audit_args: json!({ "path": remote }),
                outcome: failure.code,
            });
        }
    };
    let Some(path) = path.to_str() else {
        let message = format!("{} is not UTF-8", path.display());
        return Some(Done::failed(
            code::bad_path,
            message,
            json!({ "path": remote }),
        ));
    };
    args["remote_path"] = json!(path);
    None
}

/// `file_put`: the client's bytes go to the worker, which writes them into
/// a temp folder of its own; once they are all there, the host moves them
/// to `remote_path`, which `destination` resolved before the put was judged.
pub async fn put(
    workers: &WorkerSetup,
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let (Some(path), Some(size)) = (args["remote_path"].as_str(), args["size"].as_u64()) else {
        return bad_arguments();
    };
    let staging = match tempfile::Builder::new().prefix("latch-put-").tempdir() {
        Ok(staging) => staging,
        Err(err) => {
            return Done::failed(
                code::tmpdir_failed,
                err.to_string(),
                json!({ "path": path }),
            );
        }
    };
    // Resolved, as the sandbox sees it (macOS's /var is /private/var).
    let staged = match staging.path().canonicalize() {
        Ok(dir) => dir.join("upload"),
        Err(err) => {
            return Done::failed(
                code::tmpdir_failed,
                err.to_string(),
                json!({ "path": path }),
            );
        }
    };
    let (mut worker, first) = match workers.start_write(size, &staged).await {
        Ok(started) => started,
        Err(error) => return refused(path, error),
    };
    if first["ready"] != true {
        return refused(path, worker.finish().await.err().unwrap_or_else(unexpected));
    }

    // First answer: "go ahead", so the client sends nothing before the
    // worker is ready to write. The file is moved into place only once all
    // `size` bytes arrived, so if the client breaks off, nothing changes.
    let go = serde_json::to_vec(&Response::Ok(json!({}))).expect("serializes");
    if wire::write_frame(send, &go).await.is_err() {
        return lost(path);
    }
    let mut stdin = worker.child.stdin.take().expect("stdin is piped");
    let copied = wire::copy(recv, &mut stdin, size).await;
    drop(stdin);
    match (copied, worker.finish().await) {
        (Ok(sha256), Ok(())) => match place::place(&staged, Path::new(path), workers) {
            Ok(()) => moved(path, size, sha256),
            Err(failure) => Done {
                response: Response::error(&failure),
                audit_args: json!({ "path": path }),
                outcome: failure.code,
            },
        },
        (_, Err(error)) => Done::worker(error, json!({ "path": path })),
        (Err(_), Ok(())) => lost(path),
    }
}

/// The file moved: the final answer and its audit details.
fn moved(path: &str, size: u64, sha256: String) -> Done {
    let details = json!({ "path": path, "size": size, "sha256": sha256 });
    Done {
        response: Response::Ok(details.clone()),
        audit_args: details,
        outcome: "ok".to_string(),
    }
}

/// The worker refused before any bytes moved: a sandbox refusal is a
/// denial (exit 4), anything else a failure (exit 1), by its code.
fn refused(path: &str, error: WorkerError) -> Done {
    Done::worker(error, json!({ "path": path }))
}

/// The worker failed after the size went out. The client is waiting for
/// bytes, not an answer, so the stream is cut (the client reports exit 5).
fn broken_off(send: &mut SendStream, path: &str, error: WorkerError) -> Done {
    let _ = send.reset(VarInt::from_u32(1));
    Done::worker(error, json!({ "path": path }))
}

fn lost(path: &str) -> Done {
    let message = "the connection to the client was lost".to_string();
    Done::failed(code::connection_lost, message, json!({ "path": path }))
}

fn unexpected() -> WorkerError {
    WorkerError {
        code: "transfer_failed".to_string(),
        message: "the transfer stopped early".to_string(),
    }
}

fn bad_arguments() -> Done {
    let message = "the request is missing remote_path or size".to_string();
    Done::failed(code::bad_request, message, Value::Null)
}
