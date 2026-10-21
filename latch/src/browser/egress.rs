//! Where a host's browser may go: the web, and nothing of the host's own.
//!
//! - Addresses: an agent's request loads only `http` and `https` (`open`,
//!   `tab new`, a step's navigation, `requested`); `file:`, `chrome:`,
//!   `view-source:`, `data:` and every other scheme are refused with
//!   `url_refused` before anything is sent. A tab that ends up on one
//!   anyway (a page's own link, redirect or script, to a file or a page
//!   it made itself, `data:` or `blob:`) is never read: `shown`.
//! - The network: the engine reaches it only through `Proxy`, on 127.0.0.1
//!   in the worker, which resolves every name itself and connects only to
//!   the addresses it checked, so a name that resolves (or re-resolves) to
//!   the host's loopback, link-local or private networks is refused, as is
//!   such an address written out. The owner allows those for every caller
//!   with `{"allow_local_browsing": true}` in the state directory's
//!   `host.json`.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::failure::{Exit, Failure};

/// The owner's settings file in the state directory.
const SETTINGS: &str = "host.json";

/// How long a client may take to send a request's head, and a connection
/// to the site to open.
const HEAD_WITHIN: Duration = Duration::from_secs(30);
const CONNECT_WITHIN: Duration = Duration::from_secs(10);
/// The longest request or response head the proxy reads.
const HEAD_MAX: usize = 64 * 1024;

/// Whether the owner lets this host's browser reach local addresses
/// (`host.json`'s `allow_local_browsing`); no file means no.
pub fn local_allowed(state_dir: &Path) -> Result<bool, Failure> {
    let path = state_dir.join(SETTINGS);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(Failure::failed(
                "bad_host_settings",
                format!("{}: {err}", path.display()),
            ));
        }
    };
    let settings: Value = serde_json::from_str(&text).map_err(|err| {
        Failure::failed("bad_host_settings", format!("{}: {err}", path.display()))
    })?;
    match &settings["allow_local_browsing"] {
        Value::Null => Ok(false),
        Value::Bool(allowed) => Ok(*allowed),
        _ => Err(Failure::failed(
            "bad_host_settings",
            format!("{}: allow_local_browsing is true or false", path.display()),
        )),
    }
}

/// Refuses an address an agent asked to load that is not on the web: any
/// scheme but `http` and `https` (`about:blank` aside), and, unless `local`,
/// a host that resolves to a local address. A name that does not resolve
/// is left to the engine, whose proxy refuses it again if it must.
pub fn requested(url: &str, local: bool) -> Result<(), Failure> {
    let Some(parsed) = web(url)? else {
        return Ok(());
    };
    if local {
        return Ok(());
    }
    let (Some(host), Some(port)) = (parsed.host_str(), parsed.port_or_known_default()) else {
        return Err(refused(format!("{url} names no host")));
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match (host, port).to_socket_addrs() {
        Ok(addresses) => match addresses.map(|a| a.ip()).find(|ip| !public(*ip)) {
            Some(ip) => Err(refused(local_address(host, ip))),
            None => Ok(()),
        },
        Err(_) => Ok(()),
    }
}

/// Refuses an address of any scheme but `http` and `https` (`about:blank`
/// aside, which is `None`): what the host checks before anything else, the
/// gatekeeper included, and the worker again.
pub fn web(url: &str) -> Result<Option<url::Url>, Failure> {
    if url == "about:blank" {
        return Ok(None);
    }
    let parsed = url::Url::parse(url).map_err(|err| refused(format!("{url}: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(refused(format!(
            "this host's browser goes only to http and https addresses, not {}:",
            parsed.scheme()
        )));
    }
    Ok(Some(parsed))
}

/// Whether a tab on `url` may be read: a web page, a blank one, or the
/// engine's page for a load that failed.
pub fn shown(url: &str) -> bool {
    let scheme = url.split(':').next().unwrap_or_default();
    matches!(scheme, "http" | "https" | "chrome-error")
        || url == "about:blank"
        || ["about:neterror", "about:certerror"]
            .iter()
            .any(|page| url.starts_with(page))
}

/// What a tab that is not `shown` answers; the tab is left blank.
pub fn not_shown(url: &str) -> Failure {
    let scheme = url.split(':').next().unwrap_or_default();
    refused(format!(
        "the tab went to a {scheme}: address, which this host's browser does not show; it is blank now"
    ))
}

fn refused(message: String) -> Failure {
    Failure::new(Exit::Denied, "url_refused", message)
}

fn local_address(host: &str, ip: IpAddr) -> String {
    format!("this host's browser does not go to local addresses: {host} is {ip}")
}

/// Whether `ip` is on the public internet: not loopback, unspecified,
/// private, shared (carrier NAT, Tailscale), link-local, multicast or
/// reserved, nor one of those inside an IPv6 address.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = ip.segments();
            let embedded = |high: u16, low: u16| {
                public_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)))
            };
            let local = ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                // IPv4-compatible and the rest of ::/96.
                || s[..6] == [0; 6]
                // Unique local, link-local, and the old site-local.
                || s[0] & 0xfe00 == 0xfc00
                || s[0] & 0xffc0 == 0xfe80
                || s[0] & 0xffc0 == 0xfec0
                // Documentation.
                || (s[0], s[1]) == (0x2001, 0x0db8)
                // NAT64 and 6to4, by the IPv4 address inside.
                || ((s[0], s[1]) == (0x0064, 0xff9b) && !embedded(s[6], s[7]))
                || (s[0] == 0x2002 && !embedded(s[1], s[2]));
            !local
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let local = a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_documentation()
        || (a == 100 && (64..128).contains(&b))
        || (a, b, c) == (192, 0, 0)
        || (a == 198 && (b == 18 || b == 19))
        // Multicast, reserved and broadcast.
        || a >= 224;
    !local
}

/// The proxy every request of the engine goes through: plain `http`
/// forwarded one request per connection, everything else (`https`,
/// WebSockets) tunnelled with `CONNECT`.
pub struct Proxy {
    pub address: SocketAddr,
}

impl Proxy {
    /// Listens on a free port of 127.0.0.1 for as long as the process runs;
    /// with `local`, local addresses are let through too.
    pub fn start(local: bool) -> io::Result<Proxy> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                thread::spawn(move || {
                    let _ = relay(client, local);
                });
            }
        });
        Ok(Proxy { address })
    }

    /// The proxy as the engines take it.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

fn relay(mut client: TcpStream, local: bool) -> io::Result<()> {
    client.set_read_timeout(Some(HEAD_WITHIN))?;
    let (head, early) = read_head(&mut client)?;
    client.set_read_timeout(None)?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (method, target, version) = (
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
    );
    let headers: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    let connect = method == "CONNECT";
    let (authority, path) = if connect {
        (target, "")
    } else {
        let Some(rest) = target.strip_prefix("http://") else {
            return answer(
                &mut client,
                "400 Bad Request",
                "latch's proxy takes http:// requests",
            );
        };
        rest.find('/').map_or((rest, "/"), |at| rest.split_at(at))
    };
    let Some((host, port)) = host_and_port(authority, if connect { 443 } else { 80 }) else {
        return answer(&mut client, "400 Bad Request", "no host in the request");
    };
    let mut site = match open(host, port, local) {
        Ok(site) => site,
        Err(why) => return answer(&mut client, "403 Forbidden", &why),
    };
    if connect {
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
        site.write_all(&early)?;
        return splice(client, site);
    }
    let named = |line: &str, names: &[&str]| {
        let name = line.split(':').next().unwrap_or_default().trim();
        names.iter().any(|n| name.eq_ignore_ascii_case(n))
    };
    // A WebSocket's handshake keeps its own Connection header; any other
    // request is the connection's last, so a browser never sends another
    // site's request down it.
    let upgrade = headers.iter().any(|line| named(line, &["upgrade"]));
    let dropped: &[&str] = if upgrade {
        &["proxy-connection", "proxy-authorization"]
    } else {
        &[
            "proxy-connection",
            "proxy-authorization",
            "connection",
            "keep-alive",
        ]
    };
    let mut out = format!("{method} {path} {version}\r\n");
    for line in headers.iter().filter(|line| !named(line, dropped)) {
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !upgrade {
        out.push_str("Connection: close\r\n");
    }
    out.push_str("\r\n");
    site.write_all(out.as_bytes())?;
    site.write_all(&early)?;
    if upgrade {
        return splice(client, site);
    }
    // The request's body goes on as it comes; the response's head is told
    // the connection closes, in case the site did not say so itself.
    let (mut body_from, mut body_to) = (client.try_clone()?, site.try_clone()?);
    let body = thread::spawn(move || {
        let _ = io::copy(&mut body_from, &mut body_to);
        let _ = body_to.shutdown(Shutdown::Write);
    });
    loop {
        let (head, early) = read_head(&mut site)?;
        let interim = head
            .split(' ')
            .nth(1)
            .is_some_and(|status| status.starts_with('1') && status != "101");
        let mut lines = head.split("\r\n").filter(|line| !line.is_empty());
        let mut out = format!("{}\r\n", lines.next().unwrap_or_default());
        for line in lines.filter(|line| interim || !named(line, &["connection", "keep-alive"])) {
            out.push_str(line);
            out.push_str("\r\n");
        }
        if !interim {
            out.push_str("Connection: close\r\n");
        }
        out.push_str("\r\n");
        client.write_all(out.as_bytes())?;
        client.write_all(&early)?;
        if !interim {
            break;
        }
    }
    let _ = io::copy(&mut site, &mut client);
    let _ = client.shutdown(Shutdown::Both);
    let _ = site.shutdown(Shutdown::Both);
    let _ = body.join();
    Ok(())
}

/// `host` and `port` from `host:port` or `[v6]:port`, the port `default`
/// when there is none.
fn host_and_port(authority: &str, default: u16) -> Option<(&str, u16)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (host, after) = rest.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(port) => port.parse().ok()?,
        None => default,
    };
    (!host.is_empty()).then_some((host, port))
}

/// A connection to `host`, at an address checked first: resolved once, and
/// refused if any address it resolves to is local (unless `local`).
fn open(host: &str, port: u16, local: bool) -> Result<TcpStream, String> {
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("{host}: {err}"))?
        .collect();
    if !local && let Some(ip) = addresses.iter().map(|a| a.ip()).find(|ip| !public(*ip)) {
        return Err(local_address(host, ip));
    }
    let mut last = format!("{host} has no address");
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_WITHIN) {
            Ok(site) => return Ok(site),
            Err(err) => last = format!("{host}: {err}"),
        }
    }
    Err(last)
}

/// Everything up to the blank line that ends a head, and whatever came
/// after it in the same reads.
fn read_head(stream: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let early = bytes.split_off(at + 4);
            return Ok((String::from_utf8_lossy(&bytes).into_owned(), early));
        }
        if bytes.len() > HEAD_MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too long"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

fn answer(client: &mut TcpStream, status: &str, text: &str) -> io::Result<()> {
    let body = format!("latch: {text}\n");
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    client.write_all(head.as_bytes())?;
    client.write_all(body.as_bytes())
}

/// Copies each way until both sides are done.
fn splice(client: TcpStream, site: TcpStream) -> io::Result<()> {
    let (mut up_from, mut up_to) = (client.try_clone()?, site.try_clone()?);
    let up = thread::spawn(move || {
        let _ = io::copy(&mut up_from, &mut up_to);
        let _ = up_to.shutdown(Shutdown::Write);
    });
    let (mut down_from, mut down_to) = (site, client);
    let _ = io::copy(&mut down_from, &mut down_to);
    let _ = down_to.shutdown(Shutdown::Write);
    let _ = up.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A site on 127.0.0.1 that answers every request with what it was
    /// sent, as its body; returns its port.
    fn echo_site() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for socket in listener.incoming() {
                let Ok(mut socket) = socket else { continue };
                thread::spawn(move || {
                    let Ok((head, _)) = read_head(&mut socket) else {
                        return;
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{head}",
                        head.len()
                    );
                    let _ = socket.write_all(reply.as_bytes());
                });
            }
        });
        port
    }

    /// What the proxy at `proxy` answers `request`, to the end.
    fn through(proxy: &Proxy, request: &str) -> String {
        let mut client = TcpStream::connect(proxy.address).unwrap();
        client.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = client.read_to_string(&mut answer);
        answer
    }

    #[test]
    fn only_public_addresses_count_as_the_web() {
        for ip in [
            "93.184.215.14",
            "8.8.8.8",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "127.8.9.10",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.100",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "64:ff9b::7f00:1",
            "2002:c0a8:101::1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn an_agent_loads_only_web_addresses() {
        for url in [
            "file:///etc/hosts",
            "FILE:///etc/hosts",
            "chrome://version",
            "devtools://devtools/bundled/inspector.html",
            "view-source:https://example.com/",
            "data:text/html,<h1>hi</h1>",
            "javascript:alert(1)",
            "about:config",
            "blob:https://example.com/0f6e",
            "not a url",
        ] {
            let failure = requested(url, true).unwrap_err();
            assert_eq!(
                (failure.code.as_str(), failure.exit),
                ("url_refused", Exit::Denied),
                "{url}"
            );
        }
        for url in [
            "http://127.0.0.1:19244/health",
            "http://localhost:8080/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "https://10.0.0.1/",
        ] {
            let failure = requested(url, false).unwrap_err();
            assert_eq!(failure.code, "url_refused", "{url}");
            assert!(
                failure.message.contains("local addresses"),
                "{}",
                failure.message
            );
            requested(url, true).unwrap();
        }
        requested("https://93.184.215.14/", false).unwrap();
        requested("about:blank", false).unwrap();
    }

    #[test]
    fn only_web_pages_are_shown() {
        for url in [
            "https://example.com/",
            "http://127.0.0.1:8/x",
            "about:blank",
            "chrome-error://chromewebdata/",
            "about:neterror?e=connectionFailure",
        ] {
            assert!(shown(url), "{url}");
        }
        for url in [
            "file:///Users/someone/state/config.json",
            "data:text/html,hi",
            "blob:https://example.com/0f6e",
            "chrome://version/",
            "view-source:https://example.com/",
            "about:config",
            "",
        ] {
            assert!(!shown(url), "{url}");
        }
    }

    #[test]
    fn the_proxy_refuses_local_addresses_by_name_and_number() {
        let site = echo_site();
        let proxy = Proxy::start(false).unwrap();
        for request in [
            format!("GET http://127.0.0.1:{site}/ HTTP/1.1\r\nHost: 127.0.0.1:{site}\r\n\r\n"),
            format!("GET http://localhost:{site}/ HTTP/1.1\r\nHost: localhost:{site}\r\n\r\n"),
            format!("GET http://[::1]:{site}/ HTTP/1.1\r\nHost: [::1]:{site}\r\n\r\n"),
            format!("CONNECT 127.0.0.1:{site} HTTP/1.1\r\nHost: 127.0.0.1:{site}\r\n\r\n"),
            format!("CONNECT localhost:{site} HTTP/1.1\r\nHost: localhost:{site}\r\n\r\n"),
        ] {
            let answer = through(&proxy, &request);
            assert!(answer.starts_with("HTTP/1.1 403 "), "{request}: {answer}");
            assert!(answer.contains("local addresses"), "{answer}");
            assert!(!answer.contains("200 OK"), "{answer}");
        }
        let answer = through(&proxy, "GET /relative HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
    }

    #[test]
    fn the_proxy_forwards_one_request_per_connection_and_tunnels_the_rest() {
        let site = echo_site();
        let proxy = Proxy::start(true).unwrap();
        let answer = through(
            &proxy,
            &format!(
                "GET http://localhost:{site}/a/b?c=d HTTP/1.1\r\nHost: localhost:{site}\r\n\
                 Proxy-Connection: keep-alive\r\nConnection: keep-alive\r\nCookie: k=v\r\n\r\n"
            ),
        );
        let (head, sent) = answer.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{answer}");
        // The browser is told the connection closes, whatever the site said.
        assert!(
            head.contains("Connection: close") && !head.contains("keep-alive"),
            "{head}"
        );
        // The site gets the path alone, and nothing meant for the proxy.
        assert!(sent.starts_with("GET /a/b?c=d HTTP/1.1\r\n"), "{sent}");
        assert!(
            sent.contains("Cookie: k=v") && sent.contains("Connection: close"),
            "{sent}"
        );
        assert!(
            !sent.contains("Proxy-Connection") && !sent.contains("keep-alive"),
            "{sent}"
        );

        let mut client = TcpStream::connect(proxy.address).unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{site} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();
        let (head, _) = read_head(&mut client).unwrap();
        assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
        client
            .write_all(b"GET /tunnelled HTTP/1.1\r\n\r\n")
            .unwrap();
        let (head, early) = read_head(&mut client).unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        let mut body = String::from_utf8(early).unwrap();
        while !body.ends_with("\r\n\r\n") {
            let mut chunk = [0u8; 256];
            let n = client.read(&mut chunk).unwrap();
            body.push_str(std::str::from_utf8(&chunk[..n]).unwrap());
        }
        assert!(body.starts_with("GET /tunnelled HTTP/1.1"), "{body}");
    }

    #[test]
    fn the_owners_setting_is_read_from_host_json() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!local_allowed(dir.path()).unwrap());
        let file = dir.path().join(SETTINGS);
        std::fs::write(&file, r#"{"allow_local_browsing": true}"#).unwrap();
        assert!(local_allowed(dir.path()).unwrap());
        std::fs::write(&file, r#"{"allow_local_browsing": false, "other": 1}"#).unwrap();
        assert!(!local_allowed(dir.path()).unwrap());
        for bad in [r#"{"allow_local_browsing": "yes"}"#, "not json"] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(
                local_allowed(dir.path()).unwrap_err().code,
                "bad_host_settings"
            );
        }
    }
}
