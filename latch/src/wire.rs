//! What a client and a host say to each other on one QUIC stream.
//!
//! Every operation is one bidirectional stream carrying length-prefixed JSON
//! frames: the client sends a `Request` frame and the host answers with one
//! `Response` frame. An operation that moves a file adds the raw bytes in
//! between (see `copy`), framed by a first answer that says how many follow:
//!
//! - `file_get`: request, then `Ok({"size": N})`, N bytes host → client, and
//!   the final answer;
//! - `file_put`: request with `"size": N`, then `Ok({})` ("go ahead"), N bytes
//!   client → host, and the final answer.
//!
//! Any error answer instead of the first `Ok` ends the operation there. A frame is a 4-byte big-endian length, then that many
//! bytes of JSON. Nothing here knows about iroh: any byte stream works, so
//! another transport could carry the same frames later.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::state::to_hex;

/// The protocol name both sides announce when connecting (QUIC "ALPN"). A
/// host only accepts connections that speak exactly this version. Version 2
/// requires per-call exec writes and opt-in network access.
pub const ALPN: &[u8] = b"plow/latch/2";

/// The protocol version in `ALPN`, as a number.
pub fn protocol() -> u32 {
    let alpn = std::str::from_utf8(ALPN).expect("the ALPN is text");
    let version = alpn
        .rsplit('/')
        .next()
        .expect("the ALPN ends in its version");
    version.parse().expect("the ALPN's version is a number")
}

/// The code a host closes a connection with when it refuses the caller.
/// The client turns it into exit 4 (denied), with the close reason as message.
pub const CLOSE_DENIED: u32 = 4;

/// The largest frame either side accepts. Big enough for any result the
/// spec allows (gog JSON is capped at 1 MiB), small enough that a broken
/// peer cannot make us allocate without limit.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// One operation, e.g. `{"op": "exec", "args": {"argv": ["uname", "-a"]}}`.
#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub op: String,
    pub args: Value,
    /// The Plow chat an agent acts for, when its line has more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
}

/// The host's answer: `{"ok": ...}` or `{"error": {...}}`. Errors carry
/// their exit class and structured details without exposing child stderr.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// The operation's result.
    Ok(Value),
    /// An operation failed, was refused, or needs corrected arguments.
    Error {
        #[serde(with = "exit_code")]
        exit: crate::failure::Exit,
        code: String,
        message: String,
        details: Value,
    },
}

// The protocol carries the public numeric code, independently of Rust names.
mod exit_code {
    use crate::failure::Exit;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(exit: &Exit, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8((*exit).into())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Exit, D::Error> {
        Exit::try_from(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Writes one frame. `impl AsyncWrite + Unpin` means "any stream we can
/// write to asynchronously", such as an iroh `SendStream`.
pub async fn write_frame(out: &mut (impl AsyncWrite + Unpin), json: &[u8]) -> std::io::Result<()> {
    if json.len() > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    out.write_all(&(json.len() as u32).to_be_bytes()).await?;
    out.write_all(json).await
}

/// Reads one frame and returns its JSON bytes.
pub async fn read_frame(input: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut json = vec![0u8; length];
    input.read_exact(&mut json).await?;
    Ok(json)
}

/// The same framing for the single-threaded worker, before any runtime starts.
pub fn read_frame_sync(input: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut json = vec![0u8; length];
    input.read_exact(&mut json)?;
    Ok(json)
}

/// Copies exactly `size` bytes from `input` to `out` and returns their
/// sha256 as hex. Fails if `input` ends early. Used on both sides of a file
/// transfer, so each side can check the other moved the same bytes.
pub async fn copy(
    input: &mut (impl AsyncRead + Unpin),
    out: &mut (impl AsyncWrite + Unpin),
    size: u64,
) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut left = size;
    while left > 0 {
        let want = buffer.len().min(left as usize);
        let read = input.read(&mut buffer[..want]).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the bytes ended early",
            ));
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read]).await?;
        left -= read as u64;
    }
    out.flush().await?;
    Ok(to_hex(&hasher.finalize()))
}

impl Response {
    pub fn into_result(self) -> Result<Value, crate::failure::Failure> {
        match self {
            Self::Ok(value) => Ok(value),
            Self::Error {
                exit,
                code,
                message,
                details,
            } => {
                let mut failure = crate::failure::Failure::new(exit, &code, message);
                failure.details = details;
                Err(failure)
            }
        }
    }

    pub fn error(failure: &crate::failure::Failure) -> Self {
        Self::Error {
            exit: failure.exit,
            code: failure.code.clone(),
            message: failure.message.clone(),
            details: failure.details.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_frame_reads_back_as_written() {
        let request = Request {
            op: "exec".to_string(),
            args: serde_json::json!({"argv": ["uname", "-a"]}),
            chat: None,
        };
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        assert_eq!(&buffer[..4], &(buffer.len() as u32 - 4).to_be_bytes());

        let json = read_frame(&mut buffer.as_slice()).await.unwrap();
        let back: Request = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.op, "exec");
        assert_eq!(back.args["argv"][1], "-a");
    }

    #[tokio::test]
    async fn copy_moves_exactly_size_bytes_and_hashes_them() {
        let mut input: &[u8] = b"hello, latch! and more";
        let mut out = Vec::new();
        let sha = copy(&mut input, &mut out, 5).await.unwrap();
        assert_eq!(out, b"hello");
        // sha256("hello")
        assert_eq!(
            sha,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let mut short: &[u8] = b"abc";
        assert!(copy(&mut short, &mut Vec::new(), 5).await.is_err());
    }

    #[test]
    fn responses_have_a_plain_json_shape() {
        let denied = Response::Error {
            exit: crate::failure::Exit::Denied,
            details: Value::Null,
            code: "caller_denied".to_string(),
            message: "unknown key".to_string(),
        };
        let json = serde_json::to_string(&denied).unwrap();
        assert_eq!(
            json,
            r#"{"error":{"exit":4,"code":"caller_denied","message":"unknown key","details":null}}"#
        );
    }
}
