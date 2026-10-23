//! Operation streams over authenticated iroh or the owner's private local socket.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use iroh::endpoint::{ConnectionError, VarInt};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::failure::{Failure, code};

pub enum Connection {
    Remote(iroh::endpoint::Connection),
    Local(Mutex<Option<UnixStream>>),
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
        }
    }

    pub fn close_reason(&self) -> Option<ConnectionError> {
        match self {
            Self::Remote(connection) => connection.close_reason(),
            Self::Local(_) => None,
        }
    }

    pub fn close(&self, code: VarInt, reason: &[u8]) {
        if let Self::Remote(connection) = self {
            connection.close(code, reason);
        }
    }
}

pub enum SendStream {
    Remote(iroh::endpoint::SendStream),
    // Dropping the owned half shuts down writes while the reader stays live.
    Local(Option<OwnedWriteHalf>),
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
        }
    }

    pub fn reset(&mut self, code: VarInt) -> io::Result<()> {
        match self {
            Self::Remote(send) => send.reset(code).map_err(io::Error::other),
            Self::Local(send) => {
                send.take();
                Ok(())
            }
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
            Self::Local(None) => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Remote(send) => Pin::new(send).poll_flush(cx),
            Self::Local(Some(send)) => Pin::new(send).poll_flush(cx),
            Self::Local(None) => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Remote(send) => Pin::new(send).poll_shutdown(cx),
            Self::Local(Some(send)) => Pin::new(send).poll_shutdown(cx),
            Self::Local(None) => Poll::Ready(Ok(())),
        }
    }
}

pub enum RecvStream {
    Remote(iroh::endpoint::RecvStream),
    Local(OwnedReadHalf),
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
        }
    }
}

/// Incompatible local receivers have separate endpoints, just as remote ALPNs do.
pub fn local_socket_path(state: &Path) -> PathBuf {
    state.join(format!("serve-v{}.sock", crate::wire::protocol()))
}

/// The longest path a Unix socket may have: the OS's `sun_path`, less its
/// closing NUL.
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

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

pub async fn connect_local(state: &Path) -> Result<UnixStream, Failure> {
    UnixStream::connect(socket_path(state)?).await.map_err(|err| {
        if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) {
            Failure::new(code::serve_required, "this machine needs a live receiver; start `latch serve` on this state folder, then retry".into())
        } else {
            Failure::new(code::local_socket, format!("could not connect to this machine's receiver: {err}"))
        }
    })
}

/// Directory permissions and socket mode grant access only to this OS user.
pub struct LocalSocket {
    pub listener: UnixListener,
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
        let listener = UnixListener::bind(&path).map_err(io)?;
        let socket = Self {
            listener,
            path: path.clone(),
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io)?;
        Ok(socket)
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
        let _accepted = current.listener.accept().await.unwrap();
    }

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
