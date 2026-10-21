//! Files named in gog arguments belong to the client. The protocol sends
//! metadata first, followed by exactly the declared number of bytes.
use serde_json::{Value, json};
use std::path::Path;
use tokio::fs::File;

use super::{Input, Request, policy, problem};
use crate::cli::GoogleArgs;
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::state::StateDir;
use crate::wire;

/// `latch google`. Opens every client file gog will read before dialing, so
/// a missing file fails here without contacting the host. The host parses
/// the arguments again and is the one that decides what is allowed.
pub async fn run(state: &StateDir, args: GoogleArgs) -> Result<Value, Failure> {
    // `latch google -- help` (or `--help`) answers here, without a host:
    // the list of what a host would run.
    if matches!(args.gog_args.as_slice(), [one] if one == "help" || one == "--help" || one == "-h")
    {
        return Ok(json!({
            "allowed": policy::allowed_commands(),
            "usage": "latch google [--host HOST] [--account EMAIL] -- <command> [args]; \
                      `-- <command> --help` shows gog's own help for one",
            "guide": "latch google --help prints the agent guide",
        }));
    }
    // latch's own flags may be written among gog's; the host lifts them out
    // again, but the files gog reads are found in what is left.
    let lifted = policy::lift(&args.gog_args)?;
    let accounts_only = lifted.args == ["accounts"];
    let plan = if accounts_only {
        None
    } else {
        policy::parse(&lifted.args).ok()
    };
    let mut files = Vec::new();
    let mut inputs = Vec::new();
    if let Some(plan) = &plan {
        for input in &plan.inputs {
            let file = File::open(&input.path)
                .await
                .map_err(|err| Failure::usage("local_file", err.to_string()))?;
            let metadata = file
                .metadata()
                .await
                .map_err(|err| Failure::usage("local_file", err.to_string()))?;
            if !metadata.is_file() {
                return Err(problem(Exit::Usage, "not_a_file", json!({})));
            }
            let name = Path::new(&input.path)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| problem(Exit::Usage, "local_file", json!({})))?;
            inputs.push(Input {
                index: input.index,
                size: metadata.len(),
                name: name.into(),
            });
            files.push(file);
        }
    }
    let host = crate::hosts::resolve(state, args.host.host).await?;
    let link = dial::connect(state, &host).await?;
    let request = Request {
        gog_args: lifted.args,
        account: lifted.accounts.unwrap_or(args.account),
        confirm_conflict: args.confirm_conflict || lifted.confirm_conflict,
        inputs,
    };
    let chat = args.host.chat;
    let result = exchange(&link.connection, chat, request, files, plan.as_ref()).await;
    link.close().await;
    result
}
/// One `google` operation on an open connection: the request, a first answer
/// ("ready"), the client's file bytes, a header listing the files gog wrote,
/// each file's bytes (written to its client path and checked by sha256), and
/// the final answer.
async fn exchange(
    connection: &iroh::endpoint::Connection,
    chat: Option<String>,
    request: Request,
    mut inputs: Vec<File>,
    plan: Option<&policy::Plan>,
) -> Result<Value, Failure> {
    let request = wire::Request {
        op: "google".into(),
        args: serde_json::to_value(&request).expect("request serializes"),
        chat,
    };
    let (mut send, mut recv) = dial::start(connection, &request).await?;
    let first = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    if plan.is_none() {
        send.finish().map_err(|_| dial::lost(connection, true))?;
        return Ok(first);
    }
    for (file, input) in inputs
        .iter_mut()
        .zip(request.args["inputs"].as_array().expect("inputs"))
    {
        wire::copy(file, &mut send, input["size"].as_u64().expect("size"))
            .await
            .map_err(|_| dial::lost(connection, true))?;
    }
    send.finish().map_err(|_| dial::lost(connection, true))?;
    let header = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    let moved: Vec<super::File> = serde_json::from_value(header["files"].clone())
        .map_err(|_| problem(Exit::Failed, "bad_response", json!({})))?;
    let mut local_files = Vec::new();
    for moved in moved {
        let path = plan.expect("plan").destination(&moved.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| Failure::failed("local_file", err.to_string()))?;
        }
        // As `file get` does: the bytes take the destination's place only
        // once they match.
        let staged = crate::transfer::Staged::beside(&path)?;
        let copied = wire::copy(&mut recv, &mut staged.file()?, moved.size).await;
        if copied.as_ref().ok() != Some(&moved.sha256) {
            return Err(problem(Exit::Failed, "checksum_mismatch", json!({})));
        }
        staged.place(&path)?;
        local_files.push(json!({"path":path,"size":moved.size,"sha256":moved.sha256}));
    }
    let mut value = dial::answer(connection, &mut recv, ANSWER_WITHIN).await?;
    if value.get("files").is_some() {
        value["files"] = json!(local_files);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode};
    use wire::Response;

    /// `gmail attachment` saving to `local`, from a host whose gog wrote a
    /// file of `size` bytes with `sha256`, of which it sends `bytes`.
    async fn attachment_from(
        size: u64,
        bytes: &[u8],
        sha256: &str,
        local: &Path,
    ) -> Result<Value, Failure> {
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
            wire::write_frame(&mut send, &frame(json!({"ready": true})))
                .await
                .unwrap();
            let files = json!([{"path": "0/attachment", "size": size, "sha256": sha256}]);
            wire::write_frame(&mut send, &frame(json!({"files": files})))
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut send, bytes)
                .await
                .unwrap();
            if bytes.len() as u64 == size {
                wire::write_frame(&mut send, &frame(json!({"files": files})))
                    .await
                    .unwrap();
            }
            send.finish().unwrap();
            let _ = send.stopped().await;
        };
        let words = [
            "gmail",
            "attachment",
            "m1",
            "a1",
            "--out",
            local.to_str().unwrap(),
        ];
        let words: Vec<String> = words.iter().map(|word| word.to_string()).collect();
        let plan = policy::parse(&words).unwrap();
        let request = Request {
            gog_args: words,
            account: vec![],
            confirm_conflict: false,
            inputs: vec![],
        };
        let exchanging = exchange(&connection, None, request, vec![], Some(&plan));
        let ((), result) = tokio::join!(serving, exchanging);
        client.close().await;
        host.close().await;
        result
    }

    #[tokio::test]
    async fn a_file_that_breaks_off_or_does_not_match_leaves_the_old_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("got");
        let fresh = dir.path().join("fresh");
        let sha256 = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(b"hello"));
        attachment_from(5, b"hello", &sha256, &local).await.unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");

        for target in [&local, &fresh] {
            let wrong = attachment_from(5, b"hellO", &sha256, target).await;
            assert_eq!(wrong.unwrap_err().code, "checksum_mismatch");
            let short = attachment_from(5, b"hel", &sha256, target).await;
            assert_eq!(short.unwrap_err().code, "checksum_mismatch");
        }
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");
        assert!(!fresh.exists());
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left: {names:?}");
    }

    #[tokio::test]
    async fn a_file_over_a_private_one_leaves_it_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("got");
        std::fs::write(&local, "the old file").unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o600)).unwrap();
        let sha256 = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(b"hello"));
        attachment_from(5, b"hello", &sha256, &local).await.unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");
        let mode = std::fs::metadata(&local).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
}
