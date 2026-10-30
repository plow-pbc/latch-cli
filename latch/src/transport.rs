//! Operation streams over authenticated iroh or the owner's private local socket.
//!
//! The local socket is a Unix socket, which only this OS user can open. Windows
//! has none that tokio drives, and its named pipes cannot close one direction
//! while the other stays open, which a local stream needs: there it is a
//! loopback TCP socket that serves a caller only once it has sent the secret
//! the endpoint file holds, in the state folder only this user can read.

use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use iroh::endpoint::{ConnectionError, VarInt};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
#[cfg(windows)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::TcpListener;
#[cfg(windows)]
pub use tokio::net::TcpStream as LocalStream;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(unix)]
pub use tokio::net::UnixStream as LocalStream;
#[cfg(windows)]
pub use tokio::net::tcp::{OwnedReadHalf as LocalRead, OwnedWriteHalf as LocalWrite};
#[cfg(unix)]
pub use tokio::net::unix::{OwnedReadHalf as LocalRead, OwnedWriteHalf as LocalWrite};

use crate::failure::{Failure, code};

pub enum Connection {
    Remote(iroh::endpoint::Connection),
    Local(Mutex<Option<LocalStream>>),
    /// Another host, through this machine's host (see `proxy.rs`).
    Proxied(crate::proxy::Proxied),
}

impl From<iroh::endpoint::Connection> for Connection {
    fn from(connection: iroh::endpoint::Connection) -> Self {
        Self::Remote(connection)
    }
}

impl Connection {
    pub async fn open_bi(&self) -> io::Result<(SendStream, RecvStream)> {
        match self {
            Self::Remote(connection) => connection
                .open_bi()
                .await
                .map(|(send, recv)| (send.into(), recv.into()))
                .map_err(io::Error::other),
            Self::Local(socket) => {
                let socket = socket
                    .lock()
                    .expect("local socket lock")
                    .take()
                    .ok_or_else(|| io::Error::other("local connection already used"))?;
                let (recv, send) = socket.into_split();
                Ok((SendStream::Local(Some(send)), RecvStream::Local(recv)))
            }
            Self::Proxied(proxied) => proxied.open_bi(),
        }
    }

    /// Why the host refused us, once it closed the connection for it.
    pub fn denied(&self) -> Option<String> {
        match self {
            Self::Remote(connection) => refusal(connection),
            Self::Local(_) => None,
            Self::Proxied(proxied) => proxied.denied(),
        }
    }

    pub fn close(&self, code: VarInt, reason: &[u8]) {
        match self {
            Self::Remote(connection) => connection.close(code, reason),
            Self::Local(_) => {}
            Self::Proxied(proxied) => proxied.close(),
        }
    }
}

/// Why a host refused us, once it closed `connection` for it: the reason
/// it closed it with the "denied" code.
pub fn refusal(connection: &iroh::endpoint::Connection) -> Option<String> {
    match connection.close_reason() {
        Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == VarInt::from_u32(crate::wire::CLOSE_DENIED) =>
        {
            Some(String::from_utf8_lossy(&close.reason).to_string())
        }
        _ => None,
    }
}

pub enum SendStream {
    Remote(iroh::endpoint::SendStream),
    // Dropping the owned half shuts down writes while the reader stays live.
    Local(Option<LocalWrite>),
    // Dropping it finishes the stream.
    Proxied(Option<DuplexStream>),
}

impl From<iroh::endpoint::SendStream> for SendStream {
    fn from(stream: iroh::endpoint::SendStream) -> Self {
        Self::Remote(stream)
    }
}

impl SendStream {
    pub fn finish(&mut self) -> io::Result<()> {
        match self {
            Self::Remote(send) => send.finish().map_err(io::Error::other),
            Self::Local(send) => {
                send.take();
                Ok(())
            }
            Self::Proxied(send) => {
                send.take();
                Ok(())
            }
        }
    }

    pub fn reset(&mut self, code: VarInt) -> io::Result<()> {
        match self {
            Self::Remote(send) => send.reset(code).map_err(io::Error::other),
            Self::Local(send) => {
                send.take();
                Ok(())
            }
            Self::Proxied(_) => Err(io::Error::other("a proxied stream is not reset")),
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Remote(send) => AsyncWrite::poll_write(Pin::new(send), cx, bytes),
            Self::Local(Some(send)) => Pin::new(send).poll_write(cx, bytes),
            Self::Proxied(Some(send)) => Pin::new(send).poll_write(cx, bytes),
            Self::Local(None) | Self::Proxied(None) => {
                Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Remote(send) => Pin::new(send).poll_flush(cx),
            Self::Local(Some(send)) => Pin::new(send).poll_flush(cx),
            Self::Proxied(Some(send)) => Pin::new(send).poll_flush(cx),
            Self::Local(None) | Self::Proxied(None) => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Remote(send) => Pin::new(send).poll_shutdown(cx),
            Self::Local(Some(send)) => Pin::new(send).poll_shutdown(cx),
            Self::Proxied(Some(send)) => Pin::new(send).poll_shutdown(cx),
            Self::Local(None) | Self::Proxied(None) => Poll::Ready(Ok(())),
        }
    }
}

pub enum RecvStream {
    Remote(iroh::endpoint::RecvStream),
    Local(LocalRead),
    Proxied(DuplexStream),
}
impl From<iroh::endpoint::RecvStream> for RecvStream {
    fn from(stream: iroh::endpoint::RecvStream) -> Self {
        Self::Remote(stream)
    }
}
impl AsyncRead for RecvStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Remote(recv) => Pin::new(recv).poll_read(cx, buf),
            Self::Local(recv) => Pin::new(recv).poll_read(cx, buf),
            Self::Proxied(recv) => Pin::new(recv).poll_read(cx, buf),
        }
    }
}

/// Incompatible local receivers have separate endpoints, just as remote ALPNs do.
/// On Windows it is the endpoint file: the address, a space and the secret.
pub fn local_socket_path(state: &Path) -> PathBuf {
    let kind = if cfg!(windows) { "endpoint" } else { "sock" };
    state.join(format!("serve-v{}.{kind}", crate::wire::protocol()))
}

/// The longest path a Unix socket may have: the OS's `sun_path`, less its
/// closing NUL. An endpoint file's path has no such limit.
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "macos") {
    103
} else if cfg!(windows) {
    usize::MAX
} else {
    107
};

/// `local_socket_path`, or a failure that says the state folder's path is
/// too long for one.
fn socket_path(state: &Path) -> Result<PathBuf, Failure> {
    let path = local_socket_path(state);
    let length = path.as_os_str().len();
    if length > MAX_SOCKET_PATH {
        let message = format!(
            "the state folder {} is too deep for this machine's local socket: its path {} is \
             {length} bytes, and the limit is {MAX_SOCKET_PATH}; use a state folder with a \
             shorter path",
            state.display(),
            path.display()
        );
        return Err(Failure::new(code::state_dir_too_long, message));
    }
    Ok(path)
}

pub async fn connect_local(state: &Path) -> Result<LocalStream, Failure> {
    let failed = |err: io::Error| {
        if matches!(
            err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) {
            Failure::new(code::serve_required, "this machine needs a live receiver; start `latch serve` on this state folder, then retry".into())
        } else {
            Failure::new(
                code::local_socket,
                format!("could not connect to this machine's receiver: {err}"),
            )
        }
    };
    #[cfg(unix)]
    return LocalStream::connect(socket_path(state)?)
        .await
        .map_err(failed);
    #[cfg(windows)]
    {
        let endpoint = fs::read_to_string(socket_path(state)?).map_err(failed)?;
        let (address, secret) = endpoint.trim_end().split_once(' ').ok_or_else(|| {
            failed(io::Error::new(
                io::ErrorKind::InvalidData,
                "the endpoint file is damaged",
            ))
        })?;
        let mut stream = LocalStream::connect(address).await.map_err(failed)?;
        stream
            .write_all(format!("{secret}\n").as_bytes())
            .await
            .map_err(failed)?;
        Ok(stream)
    }
}

/// The secret a local caller sends first on Windows: 32 random bytes as hex.
#[cfg(windows)]
const SECRET_LEN: usize = 64;

/// Directory permissions and socket mode grant access only to this OS user.
pub struct LocalSocket {
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(windows)]
    listener: TcpListener,
    admission: Admission,
    path: PathBuf,
}
impl LocalSocket {
    pub fn bind(state: &Path) -> Result<Self, Failure> {
        let path = socket_path(state)?;
        let io = |err| Failure::new(code::local_socket, format!("{}: {err}", path.display()));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)),
        }
        #[cfg(unix)]
        let socket = {
            let listener = UnixListener::bind(&path).map_err(io)?;
            let socket = Self {
                listener,
                admission: Admission {},
                path: path.clone(),
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io)?;
            socket
        };
        #[cfg(windows)]
        let socket = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(io)?;
            listener.set_nonblocking(true).map_err(io)?;
            let listener = TcpListener::from_std(listener).map_err(io)?;
            let secret = crate::state::to_hex(&rand::random::<[u8; SECRET_LEN / 2]>());
            let address = listener.local_addr().map_err(io)?;
            crate::state::write_private(&path, format!("{address} {secret}\n").as_bytes())
                .map_err(io)?;
            Self {
                listener,
                admission: Admission {
                    secret: secret.into(),
                },
                path: path.clone(),
            }
        };
        Ok(socket)
    }

    pub async fn accept(&self) -> io::Result<LocalStream> {
        self.listener.accept().await.map(|(socket, _)| socket)
    }

    /// What each caller this socket accepts must pass before it is served.
    pub fn admission(&self) -> Admission {
        self.admission.clone()
    }
}

/// What a local caller must show before the host serves it: on Windows the
/// endpoint file's secret, within a few seconds. A Unix socket's mode has
/// already let in only this user.
#[derive(Clone)]
pub struct Admission {
    #[cfg(windows)]
    secret: std::sync::Arc<str>,
}
impl Admission {
    #[cfg(unix)]
    pub async fn check(&self, _recv: &mut LocalRead) -> bool {
        true
    }

    #[cfg(windows)]
    pub async fn check(&self, recv: &mut LocalRead) -> bool {
        let mut sent = [0; SECRET_LEN + 1];
        let within = std::time::Duration::from_secs(5);
        match tokio::time::timeout(within, recv.read_exact(&mut sent)).await {
            Ok(Ok(_)) => sent[..SECRET_LEN] == *self.secret.as_bytes() && sent[SECRET_LEN] == b'\n',
            _ => false,
        }
    }
}
impl Drop for LocalSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_client_refuses_an_unversioned_local_receiver() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let old = std::os::unix::net::UnixListener::bind(dir.path().join("serve.sock")).unwrap();
        old.set_nonblocking(true).unwrap();
        let failure = connect_local(dir.path()).await.unwrap_err();
        assert_eq!(failure.code, "serve_required");
        assert_eq!(old.accept().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let current = LocalSocket::bind(dir.path()).unwrap();
        let _client = connect_local(dir.path()).await.unwrap();
        let _accepted = current.accept().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_state_folder_too_deep_for_a_socket_says_so_and_names_the_limit() {
        let state = Path::new("/state").join("deep".repeat(MAX_SOCKET_PATH / 4));
        for failure in [
            connect_local(&state).await.unwrap_err(),
            LocalSocket::bind(&state).err().unwrap(),
        ] {
            assert_eq!(failure.code, "state_dir_too_long");
            assert!(
                failure
                    .message
                    .contains(&format!("the limit is {MAX_SOCKET_PATH}"))
            );
            assert!(!failure.message.contains("SUN_LEN"));
        }
    }
}
