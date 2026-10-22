//! Where the browser goes: the web only. A file (a sentinel standing in for
//! the host's own state), another scheme's page, or a local address is
//! refused when an agent names it (`open`, `tab new`, `goto`), and out of reach of a page's own link,
//! script or redirect; the web itself loads as before.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::browser::engine::named;

use super::{CALLER, Host, act, find, serve, test_state};

conformance!(
    no_file_or_other_schemes_page_is_ever_shown,
    a_data_or_blob_page_is_never_read,
    local_addresses_are_refused_by_number_and_by_name,
);

/// The web, for real: not part of the gate, which runs offline.
mod web {
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn cft() {
        super::the_web_loads_as_before("cft").await
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn camoufox() {
        super::the_web_loads_as_before("camoufox").await
    }
}

/// A made-up secret no page may ever show.
fn sentinel(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{name}-{nanos:x}")
}

/// Answers every request on 127.0.0.1 with a redirect to `location`;
/// returns the port.
fn redirecting(location: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { continue };
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(reply.as_bytes());
        }
    });
    port
}

async fn no_file_or_other_schemes_page_is_ever_shown(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let secret = sentinel("SENTINEL");
    let file = state.join("sentinel-config.json");
    fs::write(&file, format!(r#"{{"plow_token": "{secret}"}}"#)).unwrap();
    let file_url = format!("file://{}", file.canonicalize().unwrap().display());
    let redirect = format!("http://127.0.0.1:{}/", redirecting(file_url.clone()));
    let page = format!(
        "<!doctype html><title>leads to a file</title><body>\
         <p><a href=\"{file_url}\">Read the file</a></p>\
         <p><button onclick=\"location.href = '{file_url}'\">Go by script</button></p>\
         <p><a href=\"{redirect}\">Redirected</a></p></body>"
    );
    let start = format!(
        "http://127.0.0.1:{}/",
        serve(vec![("/".into(), page.into())])
    );
    let mut h = Host::new(&state);

    // Named by the agent: refused before anything is sent.
    for url in [
        file_url.as_str(),
        "file:///etc/hosts",
        "chrome://version",
        "devtools://devtools/bundled/inspector.html",
        &format!("view-source:{start}"),
        "data:text/html,<h1>made up</h1>",
        &redirect.replace("http:", "ftp:"),
    ] {
        let refused = h
            .call(CALLER, "open", named(engine), json!({"url": url}))
            .await
            .unwrap_err();
        println!("open {url}: {} ({})", refused.code, refused.message);
        assert_eq!(refused.code, "url_refused", "{url}");
    }
    let (s, _) = h.open(CALLER, engine, json!({"url": start})).await;
    for (command, args) in [
        ("tab", json!({"action": "new", "url": file_url})),
        ("goto", json!({"url": file_url})),
        ("goto", json!({"url": "chrome://version"})),
    ] {
        let refused = h.call(CALLER, command, &s, args).await.unwrap_err();
        assert_eq!(refused.code, "url_refused", "{command}");
    }
    let stayed = h.call(CALLER, "observe", &s, json!({})).await.unwrap();
    assert_eq!(stayed["url"], json!(start));
    h.call(CALLER, "close", &s, json!({})).await.unwrap();

    // A page's own link, script and redirect: the file is never shown.
    for (role, name) in [
        ("link", "Read the file"),
        ("button", "Go by script"),
        ("link", "Redirected"),
    ] {
        let (s, page) = h.open(CALLER, engine, json!({"url": start})).await;
        let target = find(&page, role, name);
        let clicked = h
            .call(CALLER, "do", &s, act(&format!("click {target}")))
            .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let observed = h.call(CALLER, "observe", &s, json!({})).await;
        let text = h.call(CALLER, "text", &s, json!({})).await;
        println!(
            "{name}: click {}; now on {}",
            clicked
                .as_ref()
                .map_or_else(|f| f.code.clone(), |_| "sent".into()),
            observed
                .as_ref()
                .map_or_else(|f| f.code.clone(), |o| o["url"].to_string()),
        );
        let seen = format!("{clicked:?} {observed:?} {text:?}");
        assert!(!seen.contains(&secret), "{name}: {seen}");
        let url = observed.unwrap()["url"].as_str().unwrap().to_string();
        assert!(!url.starts_with("file:"), "{name}: {url}");
        h.call(CALLER, "close", &s, json!({})).await.unwrap();
    }
    let audits = format!("{:?}", h.audits);
    assert!(!audits.contains(&secret));
}

async fn local_addresses_are_refused_by_number_and_by_name(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    // The owner has not allowed local browsing: the default.
    fs::remove_file(state.join("host.json")).unwrap();
    let secret = sentinel("LOCAL-SERVICE");
    let service = format!("<!doctype html><title>local admin</title><h1>{secret}</h1>");
    let port = serve(vec![("/".into(), service.into())]);
    let mut h = Host::new(&state);

    // Named by the agent: refused before anything is sent.
    for url in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://localhost:{port}/"),
        format!("https://127.0.0.1:{port}/"),
    ] {
        let refused = h
            .call(CALLER, "open", named(engine), json!({"url": url}))
            .await
            .unwrap_err();
        println!("open {url}: {} ({})", refused.code, refused.message);
        assert_eq!(refused.code, "url_refused", "{url}");
        assert!(
            refused.message.contains("local addresses"),
            "{}",
            refused.message
        );
    }

    let (s, _) = h.open(CALLER, engine, json!({})).await;
    for url in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://localhost:{port}/"),
    ] {
        let refused = h
            .call(CALLER, "goto", &s, json!({"url": url}))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "url_refused", "goto {url}");
    }

    // A page's own way there, by name and by number, meets the proxy.
    let script = format!(
        "const frame = document.createElement('iframe');\n\
         frame.src = 'http://127.0.0.1:{port}/';\n\
         document.body.append(frame); 1"
    );
    h.call(CALLER, "eval", &s, json!({"script": script}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let framed = h.call(CALLER, "observe", &s, json!({})).await.unwrap();
    println!("a frame on 127.0.0.1: {}", framed["frames"]);
    assert!(!framed.to_string().contains(&secret), "{framed:#}");
    let script = format!("location.href = 'http://localhost:{port}/'; 1");
    h.call(CALLER, "eval", &s, json!({"script": script}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let text = h.call(CALLER, "text", &s, json!({})).await.unwrap();
    println!("a page sent to localhost reads: {}", text["text"]);
    assert!(!text.to_string().contains(&secret), "{text:#}");
    assert!(text.to_string().contains("local addresses"), "{text:#}");
    h.call(CALLER, "close", &s, json!({})).await.unwrap();
}

async fn the_web_loads_as_before(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    fs::remove_file(state.join("host.json")).unwrap();
    let mut h = Host::new(&state);
    for url in ["https://example.com/", "http://example.com/"] {
        let (s, page) = h.open(CALLER, engine, json!({"url": url})).await;
        let text = h.call(CALLER, "text", &s, json!({})).await.unwrap();
        println!("{url}: {} {:?}", page["url"], page["title"]);
        assert!(text.to_string().contains("Example Domain"), "{text:#}");
        h.call(CALLER, "close", &s, json!({})).await.unwrap();
    }
}

/// A file Chrome would itself load (the driver's own navigation, a new
/// tab on it) is stopped by latch's interception before it starts: the
/// worker's check of the address is bypassed here, by driving the engine
/// directly, so only the engine-level stop is left to hold.
#[tokio::test]
async fn chrome_never_starts_a_file_load_even_one_it_would_allow() {
    use crate::browser::cft::Cft;
    use crate::browser::driver::Driver;
    let (_dir, state, _area) = test_state("cft");
    let secret = sentinel("SENTINEL");
    let file = state.join("sentinel-config.json");
    fs::write(&file, format!(r#"{{"plow_token": "{secret}"}}"#)).unwrap();
    let file_url = format!("file://{}", file.canonicalize().unwrap().display());
    let scratch = tempfile::tempdir().unwrap();
    let state = state.canonicalize().unwrap();
    let scratch_path = scratch.path().canonicalize().unwrap();
    let profile = crate::browser::profile::make(&state).unwrap();
    let proxy = crate::browser::egress::Proxy::start(true).unwrap().url();
    let mut browser = Cft::launch(
        &state,
        profile.path(),
        &scratch_path,
        super::HEADLESS,
        None,
        &proxy,
        false,
    )
    .unwrap();
    let went = browser.navigate(&file_url);
    let opened = browser.new_tab(Some(&file_url));
    let mut shown = Vec::new();
    for tab in browser.tabs().unwrap() {
        browser.switch_tab(&tab.tab).unwrap();
        let text = browser.eval("document.documentElement.innerText");
        shown.push(format!("{} {text:?}", tab.url));
    }
    browser.close();
    println!("navigate: {went:?}\nnew tab: {opened:?}\ntabs: {shown:?}");
    for attempt in [went.map(|_| String::new()), opened] {
        let failure = attempt.unwrap_err();
        assert!(
            failure.message.contains("ERR_BLOCKED_BY_CLIENT"),
            "{failure:?}"
        );
    }
    assert!(!format!("{shown:?}").contains(&secret), "{shown:?}");
}

/// A page a script sends to a `data:` or `blob:` address of its own making
/// (a blob page is one the engines load themselves) is never read: no
/// engine hook stops those loads before they start, so the tab is blanked
/// when latch next reads it. A `data:` image still loads.
async fn a_data_or_blob_page_is_never_read(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let secret = sentinel("SENTINEL");
    let pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let page = format!(
        "<!doctype html><title>pictures</title><p>pictures</p>\
         <img id=pixel alt=pixel src=\"data:image/png;base64,{pixel}\">"
    );
    let start = format!(
        "http://127.0.0.1:{}/",
        serve(vec![("/".into(), page.into())])
    );
    let mut h = Host::new(&state);
    let (s, _) = h.open(CALLER, engine, json!({"url": start})).await;
    let width = h
        .call(
            CALLER,
            "eval",
            &s,
            json!({"script": "document.getElementById('pixel').naturalWidth"}),
        )
        .await
        .unwrap();
    assert_eq!(width["value"], json!(1), "{width:#}");
    for target in [
        format!("URL.createObjectURL(new Blob(['<h1>{secret}</h1>'], {{type: 'text/html'}}))"),
        format!("'data:text/html,<h1>{secret}</h1>'"),
    ] {
        let script = format!("location.href = {target}; 1");
        h.call(CALLER, "eval", &s, json!({"script": script}))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let read = h.call(CALLER, "text", &s, json!({})).await;
        let after = h.call(CALLER, "observe", &s, json!({})).await.unwrap();
        println!(
            "{}: {} then on {}",
            &target[..24],
            read.as_ref()
                .map_or_else(|f| f.message.clone(), |_| "read".into()),
            after["url"]
        );
        assert!(!format!("{read:?} {after}").contains(&secret));
        let url = after["url"].as_str().unwrap();
        assert!(url == "about:blank" || url.starts_with(&start), "{url}");
        h.call(CALLER, "goto", &s, json!({"url": start}))
            .await
            .unwrap();
    }
    h.call(CALLER, "close", &s, json!({})).await.unwrap();
}
