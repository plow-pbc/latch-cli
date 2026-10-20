//! A latch host, from a browser: dial its key through its relay, then
//! exchange one framed JSON request and answer per stream, as latch's own
//! clients do (`wire.rs`: a 4-byte big-endian length, then the JSON).
//!
//! The page's key is a fresh one each time; nothing is stored.

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl};
use wasm_bindgen::prelude::*;

/// The phone protocol latch accepts beside its own (`vault/link.rs`).
const ALPN: &[u8] = b"plow/latch-phone/1";

/// The largest answer the page accepts.
const MAX_FRAME: usize = 1024 * 1024;

fn js(err: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&err.to_string())
}

/// An open connection to one host.
#[wasm_bindgen]
pub struct Host {
    endpoint: Endpoint,
    connection: Connection,
}

/// Dials `host` (its key, hex) through `relay`. The handshake proves the
/// far end holds that key's private half.
#[wasm_bindgen]
pub async fn connect(host: String, relay: String) -> Result<Host, JsValue> {
    console_error_panic_hook::set_once();
    let id: EndpointId = host.parse().map_err(js)?;
    let relay: RelayUrl = relay.parse().map_err(js)?;
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::custom([relay.clone()]))
        .bind()
        .await
        .map_err(js)?;
    let connection = endpoint
        .connect(EndpointAddr::new(id).with_relay_url(relay), ALPN)
        .await
        .map_err(js)?;
    Ok(Host {
        endpoint,
        connection,
    })
}

#[wasm_bindgen]
impl Host {
    /// Sends one request (JSON text) on a new stream and resolves to the
    /// host's answer (JSON text).
    pub fn call(&self, request: String) -> js_sys::Promise {
        let connection = self.connection.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let (mut send, mut recv) = connection.open_bi().await.map_err(js)?;
            let body = request.into_bytes();
            send.write_all(&(body.len() as u32).to_be_bytes())
                .await
                .map_err(js)?;
            send.write_all(&body).await.map_err(js)?;
            send.finish().map_err(js)?;
            let mut length = [0u8; 4];
            recv.read_exact(&mut length).await.map_err(js)?;
            let length = u32::from_be_bytes(length) as usize;
            if length > MAX_FRAME {
                return Err(js("the host's answer is too large"));
            }
            let mut answer = vec![0u8; length];
            recv.read_exact(&mut answer).await.map_err(js)?;
            let answer = String::from_utf8(answer).map_err(js)?;
            Ok(JsValue::from_str(&answer))
        })
    }

    /// The page's own throwaway key, for the host's log to be matched.
    pub fn key(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Closes the connection, telling the host.
    pub async fn close(self) {
        self.connection.close(0u32.into(), b"done");
        self.endpoint.close().await;
    }
}
