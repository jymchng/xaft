//! Browser tools against a real Chromium (parity rows 60-77).
//!
//! Every test serves its pages from an in-process HTTP server on 127.0.0.1 —
//! no network access — and launches the Chromium that xaft's own discovery
//! finds (Playwright's cache, then `PATH`). Without one, each test prints why
//! it is skipping and passes: the unit tests in `src/browser/` cover the
//! manager, policy and tool contract without a browser.
//!
//! The tests share one lock so at most one browser runs at a time; on a loaded
//! machine several concurrent Chromium launches are what makes browser tests
//! flaky, not the code under test.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use agtrs_runtime::tool::ToolContext;
use xaft_config::{CloakBrowserConfig, PlaywrightConfig};
use xaft_tools::ToolRegistryBuilder;
use xaft_tools::browser::policy::{ResolveFuture, Resolver};
use xaft_tools::browser::{
    BrowserDiscovery, BrowserLaunchOptions, BrowserPolicy, BrowserSessionManager, BrowserSettings,
    CdpBrowserClient, browser_tools,
};

/// A 1x1 transparent PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn browser_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The Chromium these tests drive, found by xaft's own discovery.
fn chromium(test: &str) -> Option<PathBuf> {
    let discovery = BrowserDiscovery::from_env();
    let found = discovery
        .playwright_headless_shell()
        .or_else(|| discovery.playwright_chromium())
        .or_else(|| discovery.path_browser());
    if found.is_none() {
        eprintln!(
            "skipping {test}: no Chromium binary found (looked in PLAYWRIGHT_BROWSERS_PATH, \
             the Playwright cache and PATH); install one with `npx playwright install chromium`"
        );
    }
    found
}

/// Pages, keyed by path. `{port}` is replaced with the server's port.
fn route(path: &str, port: u16) -> (&'static str, &'static str, Vec<u8>, String) {
    let html = |body: String| {
        (
            "200 OK",
            "text/html; charset=utf-8",
            body.into_bytes(),
            String::new(),
        )
    };
    let base = path.split('?').next().unwrap_or("/");
    match base {
        "/" => html(
            r#"<!doctype html><html><head><title>Fixture Home</title></head><body>
<h1>Welcome to the fixture</h1>
<p id="intro">Plain paragraph text for the snapshot.</p>
<a id="next-link" href="/next?token=secret#frag">Next page</a>
<form id="search" action="/search" method="get">
  <input id="q" name="q" aria-label="Search query" placeholder="Search">
  <input type="hidden" name="h" value="x">
  <input id="pw" type="password" name="pw">
  <input id="card" type="text" autocomplete="cc-number">
  <select id="choice"><option>a</option></select>
  <button type="submit" id="go">Go</button>
</form>
<button id="later" onclick="setTimeout(() => { const d = document.createElement('div'); d.id = 'late'; d.textContent = 'Arrived late'; document.body.appendChild(d); }, 300)">Delay</button>
<button id="popup" onclick="window.open('/next', '_blank')">Popup</button>
<button id="alert" onclick="alert('hi'); document.title = 'Alert handled'">Alert</button>
<div style="height: 3000px">tall</div>
<p id="bottom">Bottom of the page</p>
</body></html>"#
                .to_string(),
        ),
        "/next" => html(
            "<!doctype html><title>Next Page</title><p>You reached the next page</p>".to_string(),
        ),
        "/search" => html(format!(
            "<!doctype html><title>Results</title><p>Results for {}</p>",
            path.split("q=").nth(1).unwrap_or("").split('&').next().unwrap_or("")
        )),
        "/assets" => html(format!(
            r#"<!doctype html><title>Assets</title>
<img src="http://allowed.test:{port}/ok.png"><img src="http://blocked.test:{port}/pixel.png">
<p>assets page</p>"#
        )),
        "/redirect" => (
            "302 Found",
            "text/plain",
            Vec::new(),
            format!("Location: http://blocked.test:{port}/next\r\n"),
        ),
        "/ok.png" | "/pixel.png" => ("200 OK", "image/png", PIXEL.to_vec(), String::new()),
        _ => ("404 Not Found", "text/plain", b"not found".to_vec(), String::new()),
    }
}

/// An in-process HTTP server that records `(host, path)` for every request.
struct Fixture {
    port: u16,
    hits: Arc<Mutex<Vec<(String, String)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).to_string();
                    let mut lines = head.lines();
                    let path = lines
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let host = lines
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("host").then(|| v.trim().to_string())
                        })
                        .unwrap_or_default();
                    recorded.lock().unwrap().push((host, path.clone()));
                    let (status, content_type, body, extra) = route(&path, port);
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                         Cache-Control: no-store\r\nConnection: close\r\n{extra}\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { port, hits, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn requested(&self, host_prefix: &str, path: &str) -> bool {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .any(|(h, p)| h.starts_with(host_prefix) && p == path)
    }

    fn requested_prefix(&self, host_prefix: &str, path_prefix: &str) -> bool {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .any(|(h, p)| h.starts_with(host_prefix) && p.starts_with(path_prefix))
    }

    fn count(&self, host_prefix: &str, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(h, p)| h.starts_with(host_prefix) && p == path)
            .count()
    }

    fn requested_host(&self, host_prefix: &str) -> bool {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .any(|(h, _)| h.starts_with(host_prefix))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Launch options for a test: profiles under the test's temp dir, and no
/// discovery at all — the executable comes from `executable_path`.
fn options(tmp: &TempDir) -> BrowserLaunchOptions {
    BrowserLaunchOptions {
        discovery: Some(BrowserDiscovery::default()),
        temp_root: Some(tmp.path().to_path_buf()),
        launch_timeout: Some(Duration::from_secs(120)),
        ..BrowserLaunchOptions::default()
    }
}

/// Generous deadlines: these run on shared, loaded CI machines.
fn playwright_config(chrome: &std::path::Path) -> PlaywrightConfig {
    PlaywrightConfig {
        executable_path: chrome.display().to_string(),
        navigation_timeout_s: 45.0,
        action_timeout_s: 30.0,
        ..PlaywrightConfig::default()
    }
}

fn page_id(result: &Value) -> String {
    result["page"]["page_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no page id in {result}"))
        .to_string()
}

fn assert_ok(result: &Value, what: &str) {
    assert_eq!(result["ok"], true, "{what}: {result}");
}

/// The whole agent journey on a launched (pipe-transport) Chromium: open,
/// snapshot, fill, press, click, wait, screenshot, close — and the safety
/// behaviours a real page exercises (sensitive fields, popups, dialogs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playwright_tools_drive_a_real_chromium() {
    let Some(chrome) = chromium("playwright_tools_drive_a_real_chromium") else {
        return;
    };
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let settings = BrowserSettings::from_playwright(&playwright_config(&chrome));
    let manager = Arc::new(BrowserSessionManager::new(
        settings,
        "it",
        tmp.path(),
        Some("it-session"),
        options(&tmp),
    ));

    let status = manager.status().await;
    assert_ok(&status, "status");
    assert_eq!(status["status"], "ready");
    assert_eq!(status["transport"], "local");

    let opened = manager.open(&site.url("/"), Some("open-home"), None).await;
    assert_ok(&opened, "open");
    assert_eq!(opened["page"]["title"], "Fixture Home");
    assert_eq!(opened["page"]["url"], site.url("/"));
    assert_eq!(opened["operation_id"], "open-home");
    let home = page_id(&opened);

    // Snapshot: bounded text, redacted links, controls with selectors.
    let snap = manager.snapshot(&home, None, None).await;
    assert_ok(&snap, "snapshot");
    let snapshot = &snap["snapshot"];
    assert_eq!(snapshot["untrusted"], true);
    assert_eq!(snapshot["truncated"], false);
    let text = snapshot["text"].as_str().unwrap();
    assert!(text.contains("Welcome to the fixture"), "{text}");
    let links = snapshot["links"].as_array().unwrap();
    let next = links
        .iter()
        .find(|l| l["text"] == "Next page")
        .expect("next link");
    assert_eq!(next["href"], "/next", "query and fragment are redacted");
    assert_eq!(next["selector"], "#next-link");
    let controls = snapshot["controls"].as_array().unwrap();
    let query = controls
        .iter()
        .find(|c| c["aria-label"] == "Search query")
        .expect("search box");
    assert_eq!(query["selector"], "#q");
    assert_eq!(query["placeholder"], "Search");
    assert!(
        !controls.iter().any(|c| c["type"] == "hidden"),
        "hidden inputs are not offered as controls"
    );
    assert!(!snap.to_string().contains("secret"));

    // Fill + Enter submits the form, and the reported page is the new one.
    let filled = manager.fill(&home, "#q", "rust", None, None).await;
    assert_ok(&filled, "fill");
    assert!(
        !filled.to_string().contains("\"rust\""),
        "fill never echoes the value"
    );
    let pressed = manager.press(&home, "enter", "#q", None, None).await;
    assert_ok(&pressed, "press");
    assert_eq!(pressed["page"]["title"], "Results");
    assert_eq!(pressed["page"]["url"], site.url("/search"));
    assert!(
        site.requested_prefix("127.0.0.1", "/search?q=rust&h=x"),
        "{:?}",
        site.hits.lock().unwrap()
    );

    // A second page: click a link (selector from the snapshot) and wait.
    let second = manager.open(&site.url("/"), None, None).await;
    let page2 = page_id(&second);
    let clicked = manager.click(&page2, "#next-link", None, None).await;
    assert_ok(&clicked, "click");
    assert_eq!(clicked["page"]["title"], "Next Page");
    // agenthicc validates the value as an approved URL first, so wildcards
    // live inside a full URL (a bare `**/next` is refused by the policy).
    let waited = manager
        .wait_for(
            &page2,
            "url",
            &format!("{}*", site.url("/next")),
            None,
            None,
        )
        .await;
    assert_ok(&waited, "wait for url");
    let bare_glob = manager.wait_for(&page2, "url", "**/next", None, None).await;
    assert_eq!(bare_glob["error_kind"], "policy_denied", "{bare_glob}");
    let loaded = manager
        .wait_for(&page2, "load_state", "load", None, None)
        .await;
    assert_ok(&loaded, "wait for load");
    let text_wait = manager
        .wait_for(&page2, "text", "reached the NEXT page", None, None)
        .await;
    assert_ok(&text_wait, "wait for text");

    // A delayed element, found by text= and by selector.
    let third = manager.open(&site.url("/"), None, None).await;
    let page3 = page_id(&third);
    assert_ok(
        &manager.click(&page3, "text=Delay", None, None).await,
        "click text=",
    );
    assert_ok(
        &manager
            .wait_for(&page3, "selector", "#late", None, None)
            .await,
        "wait selector",
    );
    assert_ok(
        &manager
            .wait_for(&page3, "text", "Arrived late", None, None)
            .await,
        "wait text",
    );
    assert_ok(
        &manager
            .wait_for(&page3, "load_state", "networkidle", None, None)
            .await,
        "networkidle",
    );
    // An element far below the fold is scrolled into view and clicked.
    assert_ok(
        &manager.click(&page3, "#bottom", None, None).await,
        "scroll + click",
    );

    // Sensitive fields are refused by element, not just by selector text.
    for selector in ["#pw", "#card", "input[name=password]"] {
        let denied = manager.fill(&page3, selector, "x", None, None).await;
        assert_eq!(
            denied["error_kind"], "policy_denied",
            "{selector}: {denied}"
        );
    }
    let not_fillable = manager.fill(&page3, "#choice", "a", None, None).await;
    assert_eq!(
        not_fillable["error_kind"], "invalid_input",
        "{not_fillable}"
    );
    let bad_selector = manager.click(&page3, "button[", None, None).await;
    assert_eq!(
        bad_selector["error_kind"], "invalid_input",
        "{bad_selector}"
    );

    // A popup is closed while paused, before it requests anything; an alert
    // never wedges the page.
    let pages_before = manager.open_page_ids().len();
    let next_requests = site.count("127.0.0.1", "/next");
    assert_ok(
        &manager.click(&page3, "#popup", None, None).await,
        "popup click",
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        site.count("127.0.0.1", "/next"),
        next_requests,
        "the popup loaded its page"
    );
    assert_ok(
        &manager.click(&page3, "#alert", None, None).await,
        "alert click",
    );
    let after_alert = manager.snapshot(&page3, None, None).await;
    assert_ok(&after_alert, "snapshot after alert");
    assert_eq!(after_alert["snapshot"]["page"]["title"], "Alert handled");
    assert_eq!(manager.open_page_ids().len(), pages_before);
    assert_eq!(manager.status().await["open_pages"], pages_before);

    // Screenshots land in the workspace and are real images.
    for (image_type, full_page, magic) in [
        ("png", false, &[0x89u8, b'P', b'N', b'G'][..]),
        ("jpeg", false, &[0xFFu8, 0xD8, 0xFF][..]),
        ("png", true, &[0x89u8, b'P', b'N', b'G'][..]),
    ] {
        let shot = manager
            .screenshot(&page3, image_type, full_page, None, None)
            .await;
        assert_ok(&shot, "screenshot");
        let path = shot["artifact"]["path"].as_str().unwrap();
        assert!(
            path.starts_with(".xaft/browser-artifacts/it-session/"),
            "{path}"
        );
        let bytes = std::fs::read(tmp.path().join(path)).unwrap();
        assert!(bytes.starts_with(magic), "{image_type} magic");
        assert_eq!(shot["artifact"]["byte_count"], bytes.len());
    }

    // Close one page, then the rest; a closed page is stale.
    assert_ok(
        &manager.close(&page2, false, None, None).await,
        "close page",
    );
    assert_eq!(
        manager.snapshot(&page2, None, None).await["error_kind"],
        "stale_page"
    );
    let all = manager.close("", true, None, None).await;
    assert_eq!(all["closed_pages"], "all");

    // Terminal close, then the open tool brings the browser back.
    manager.close_session().await;
    assert_eq!(manager.status().await["status"], "closed");
    let reopened = manager.open(&site.url("/next"), None, None).await;
    assert_ok(&reopened, "reopen after close");
    manager.close_session().await;
}

fn test_resolver() -> Resolver {
    let table: HashMap<&'static str, IpAddr> = [
        ("allowed.test", "127.0.0.1".parse().unwrap()),
        ("blocked.test", "127.0.0.1".parse().unwrap()),
    ]
    .into_iter()
    .collect();
    Arc::new(move |host: String| -> ResolveFuture {
        let answer = table.get(host.as_str()).copied();
        Box::pin(async move {
            answer
                .map(|ip| vec![ip])
                .ok_or_else(|| std::io::Error::other("unknown test host"))
        })
    })
}

/// With an allow-list, sub-resources and redirects to other hosts are blocked
/// inside the browser — the request never reaches the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restricted_allow_list_blocks_subresources_and_redirects() {
    let Some(chrome) = chromium("a_restricted_allow_list_blocks_subresources_and_redirects") else {
        return;
    };
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let domains = vec!["allowed.test".to_string()];
    let settings = BrowserSettings::from_playwright(&PlaywrightConfig {
        allowed_domains: domains.clone(),
        allow_all_domains: false,
        ..playwright_config(&chrome)
    });
    let policy = BrowserPolicy::playwright(&domains, false)
        .unwrap()
        .with_resolver(test_resolver());
    let mut launch = options(&tmp);
    // Chromium resolves the test hosts to the fixture server.
    launch
        .extra_args
        .push("--host-resolver-rules=MAP *.test 127.0.0.1".to_string());
    let client = Arc::new(CdpBrowserClient::new(
        settings.clone(),
        Arc::new(policy.clone()),
        tmp.path(),
        "restricted",
        launch,
    ));
    let manager =
        BrowserSessionManager::with_client(settings, "c", tmp.path(), client, Some(policy));

    let opened = manager
        .open(
            &format!("http://allowed.test:{}/assets", site.port),
            None,
            None,
        )
        .await;
    assert_ok(&opened, "open allowed page");
    let page = page_id(&opened);
    assert_ok(
        &manager
            .wait_for(&page, "load_state", "load", None, None)
            .await,
        "load",
    );
    assert!(
        site.requested("allowed.test", "/ok.png"),
        "{:?}",
        site.hits.lock().unwrap()
    );
    assert!(
        !site.requested_host("blocked.test"),
        "a blocked sub-resource reached the server: {:?}",
        site.hits.lock().unwrap()
    );

    let denied = manager
        .open(
            &format!("http://blocked.test:{}/next", site.port),
            None,
            None,
        )
        .await;
    assert_eq!(denied["error_kind"], "policy_denied", "{denied}");
    assert_eq!(denied["error"], "Destination is not allow-listed.");

    let redirected = manager
        .open(
            &format!("http://allowed.test:{}/redirect", site.port),
            None,
            None,
        )
        .await;
    assert_eq!(redirected["ok"], false, "{redirected}");
    assert!(
        !site.requested_host("blocked.test"),
        "a redirect escaped the allow-list: {:?}",
        site.hits.lock().unwrap()
    );
    manager.close_session().await;
}

/// Start an "operator" browser with a DevTools port and return its endpoint.
async fn operator_browser(
    chrome: &std::path::Path,
    profile: &std::path::Path,
) -> (tokio::process::Child, String) {
    let mut child = tokio::process::Command::new(chrome)
        .args([
            "--headless",
            "--no-sandbox",
            "--no-first-run",
            "--no-default-browser-check",
            "--remote-debugging-port=0",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn operator browser");
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let ws = tokio::time::timeout(Duration::from_secs(120), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(rest) = line.strip_prefix("DevTools listening on ") {
                return rest.trim().to_string();
            }
        }
        panic!("the operator browser never announced its DevTools endpoint");
    })
    .await
    .expect("operator browser start-up timed out");
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let port = ws
        .trim_start_matches("ws://127.0.0.1:")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    (child, format!("http://127.0.0.1:{port}"))
}

/// GET a DevTools JSON document. Chromium keeps the connection open after
/// answering, so read by `Content-Length` rather than to end-of-stream.
async fn json_get(endpoint: &str, path: &str) -> Value {
    let address = endpoint.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .expect("DevTools responses carry Content-Length");
                if raw.len() >= end + 4 + length {
                    return serde_json::from_slice(&raw[end + 4..end + 4 + length]).unwrap();
                }
            }
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before the body arrived");
            raw.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("DevTools JSON request timed out")
}

/// `cloakbrowser` with `transport = "cdp"` attaches to an operator's browser,
/// works in an isolated context, and leaves the browser running afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloakbrowser_cdp_transport_isolates_the_session_in_an_operator_browser() {
    let Some(chrome) =
        chromium("cloakbrowser_cdp_transport_isolates_the_session_in_an_operator_browser")
    else {
        return;
    };
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let profile = TempDir::new_in(tmp.path()).unwrap();
    let (mut operator, endpoint) = operator_browser(&chrome, profile.path()).await;

    let settings = BrowserSettings::from_cloakbrowser(&CloakBrowserConfig {
        transport: "cdp".into(),
        cdp_endpoint: endpoint.clone(),
        navigation_timeout_s: 45.0,
        action_timeout_s: 30.0,
        ..CloakBrowserConfig::default()
    });
    let manager = BrowserSessionManager::new(
        settings,
        "c",
        tmp.path(),
        Some("cdp-session"),
        options(&tmp),
    );
    let status = manager.status().await;
    assert_ok(&status, "status");
    assert_eq!(status["transport"], "cdp");

    let opened = manager.open(&site.url("/next"), None, None).await;
    assert_ok(&opened, "open over cdp");
    assert_eq!(opened["page"]["title"], "Next Page");
    let page = page_id(&opened);
    let snap = manager.snapshot(&page, None, None).await;
    assert!(
        snap["snapshot"]["text"]
            .as_str()
            .unwrap()
            .contains("reached the next page")
    );

    manager.close_session().await;
    // The operator's browser and its own page are untouched; ours is gone.
    let targets = json_get(&endpoint, "/json/list").await;
    let urls: Vec<String> = targets
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["url"].as_str().map(str::to_string))
        .collect();
    assert!(urls.iter().any(|u| u == "about:blank"), "{urls:?}");
    assert!(!urls.iter().any(|u| u.contains("/next")), "{urls:?}");
    assert!(json_get(&endpoint, "/json/version").await["Browser"].is_string());
    let _ = operator.kill().await;
}

/// `cloakbrowser` with `transport = "local"` launches the installed
/// CloakBrowser build, found the way the `cloakbrowser` package finds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloakbrowser_local_transport_launches_the_installed_build() {
    let discovery = BrowserDiscovery::from_env();
    if discovery.find_cloakbrowser_executable(false).is_err() {
        eprintln!(
            "skipping cloakbrowser_local_transport_launches_the_installed_build: no CloakBrowser \
             build installed (`pip install cloakbrowser && cloakbrowser install`)"
        );
        return;
    }
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let settings = BrowserSettings::from_cloakbrowser(&CloakBrowserConfig {
        // A variable that is certainly unset, so the free build is chosen.
        license_key_env: "XAFT_TEST_UNSET_CLOAKBROWSER_KEY".into(),
        navigation_timeout_s: 45.0,
        action_timeout_s: 30.0,
        ..CloakBrowserConfig::default()
    });
    let manager = BrowserSessionManager::new(
        settings,
        "c",
        tmp.path(),
        Some("cloak-local"),
        BrowserLaunchOptions {
            discovery: Some(discovery),
            temp_root: Some(tmp.path().to_path_buf()),
            launch_timeout: Some(Duration::from_secs(120)),
            ..BrowserLaunchOptions::default()
        },
    );
    assert_eq!(manager.status().await["status"], "ready");
    let opened = manager.open(&site.url("/"), None, None).await;
    assert_ok(&opened, "open with CloakBrowser");
    assert_eq!(opened["page"]["title"], "Fixture Home");
    let snap = manager.snapshot(&page_id(&opened), None, None).await;
    assert_ok(&snap, "snapshot with CloakBrowser");
    manager.close_session().await;
}

/// The loopback-port launch (used where the pipe transport is unavailable)
/// works too, and a persistent profile lands inside the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_transport_and_persistent_profiles() {
    let Some(chrome) = chromium("port_transport_and_persistent_profiles") else {
        return;
    };
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let settings = BrowserSettings::from_playwright(&PlaywrightConfig {
        allow_persistent_profiles: true,
        profile_root: "profiles".into(),
        ..playwright_config(&chrome)
    });
    let mut launch = options(&tmp);
    launch.force_port_transport = true;
    let manager = BrowserSessionManager::new(settings, "c", tmp.path(), Some("persist-1"), launch);
    let opened = manager.open(&site.url("/next"), None, None).await;
    assert_ok(&opened, "open over the port transport");
    manager.close_session().await;
    let profile = tmp.path().join("profiles").join("persist-1");
    assert!(profile.is_dir(), "persistent profile missing");
    assert!(
        std::fs::read_dir(&profile).unwrap().next().is_some(),
        "the browser wrote nothing into its persistent profile"
    );
}

/// The tools are reachable through a built registry and return agenthicc's
/// result envelopes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_tools_call_through_the_registry() {
    let Some(chrome) = chromium("registered_tools_call_through_the_registry") else {
        return;
    };
    let _serial = browser_lock().lock().await;
    let tmp = TempDir::new().unwrap();
    let site = Fixture::start().await;
    let manager = Arc::new(BrowserSessionManager::new(
        BrowserSettings::from_playwright(&playwright_config(&chrome)),
        "c",
        tmp.path(),
        None,
        options(&tmp),
    ));
    assert_eq!(browser_tools(&manager).len(), 9);
    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .with_browser_session(Arc::clone(&manager))
        .build_coder()
        .unwrap();
    let ctx = ToolContext::new("call-1");
    let open = registry.get("playwright_open").expect("registered");
    let result = open
        .call(json!({ "url": site.url("/next") }), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    let body: Value = serde_json::from_str(&result.content).unwrap();
    let page = page_id(&body);
    let snap = registry
        .get("playwright_snapshot")
        .unwrap()
        .call(json!({ "page_id": page }), &ctx)
        .await
        .unwrap();
    assert!(snap.content.contains("reached the next page"));
    assert!(
        registry
            .get("playwright_click")
            .unwrap()
            .requires_confirmation()
    );
    manager.close_session().await;
}
