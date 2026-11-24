//! Relays the driver's NUL-delimited CDP pipe to a running browser's
//! loopback DevTools WebSocket. When either end closes, dropping the
//! socket releases the driver's DevTools sessions and interceptions.

use std::error::Error;
use std::io;
use std::os::fd::OwnedFd;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::pipe::{Receiver, Sender};
use tokio_websockets::{ClientBuilder, Message};

const CONNECT: Duration = Duration::from_secs(5);

/// Runs beside the driver's blocking pipe reader. The worker and the pipe
/// own the connection's lifetime, including when the host dies.
pub(super) fn start(port: u16, commands: OwnedFd, replies: OwnedFd) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name("cdp-relay".into())
        .spawn(move || {
            if let Err(err) = runtime.block_on(relay(port, commands, replies)) {
                eprintln!("CDP relay: {err}");
            }
        })?;
    Ok(())
}

async fn relay(port: u16, commands: OwnedFd, replies: OwnedFd) -> Result<(), Box<dyn Error>> {
    let mut commands = BufReader::new(Receiver::from_owned_fd(commands)?);
    let mut replies = Sender::from_owned_fd(replies)?;
    let path = socket_path(port).await?;
    let client = ClientBuilder::new().uri(&format!("ws://127.0.0.1:{port}{path}"))?;
    let (mut socket, _) = tokio::time::timeout(CONNECT, client.connect()).await??;
    let mut command = Vec::new();
    loop {
        tokio::select! {
            read = commands.read_until(0, &mut command) => {
                if read? == 0 || command.pop() != Some(0) {
                    return Ok(());
                }
                socket.send(Message::text(String::from_utf8(std::mem::take(&mut command))?)).await?;
            }
            message = socket.next() => {
                let Some(message) = message.transpose()? else {
                    return Ok(());
                };
                if message.is_close() {
                    return Ok(());
                }
                if let Some(text) = message.as_text() {
                    replies.write_all(text.as_bytes()).await?;
                    replies.write_all(&[0]).await?;
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    socket: String,
}

async fn socket_path(port: u16) -> Result<String, Box<dyn Error>> {
    let version = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(CONNECT)
        .build()?
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await?
        .error_for_status()?
        .json::<Version>()
        .await?;
    let socket = url::Url::parse(&version.socket)?;
    if socket.scheme() != "ws" || !socket.path().starts_with("/devtools/browser/") {
        return Err("/json/version did not name a browser DevTools WebSocket".into());
    }
    // The discovery reply supplies only the path. Both connections use
    // the same explicit loopback address from the host.
    Ok(socket.path().to_string())
}
