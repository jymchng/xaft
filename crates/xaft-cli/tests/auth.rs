//! `xaft login` / `whoami` / `logout` (parity rows 54-56) against a local
//! OAuth provider: the handlers print agenthicc's lines and share one store.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xaft_cli::commands::auth::{login, logout, whoami};
use xaft_cli::error::XaftError;
use xaft_config::AuthConfig;
use xaft_runtime::ExitCode;
use xaft_runtime::auth::{AuthClient, AuthError, TokenStore};

/// A `Write` the test can read while the handler is still running.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// A token endpoint that answers every request with one token response.
async fn token_endpoint(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

fn param(url: &str, key: &str) -> String {
    url.split_once('?')
        .unwrap()
        .1
        .split('&')
        .find_map(|p| p.strip_prefix(&format!("{key}=")))
        .unwrap()
        .replace("%3A", ":")
        .replace("%2F", "/")
}

#[tokio::test]
async fn login_without_a_provider_is_a_config_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let client = AuthClient::new(
        &AuthConfig::default(),
        TokenStore::new(tmp.path().join("a.json")),
    );
    let mut out = Vec::new();
    let err = login(&client, false, &mut out).await.unwrap_err();
    assert!(
        matches!(err, XaftError::Auth(AuthError::NotConfigured)),
        "{err}"
    );
    assert_eq!(err.exit_code(), ExitCode::CONFIG_ERROR);
    assert!(err.to_string().contains("[auth]"));
}

#[tokio::test]
async fn login_whoami_and_logout_share_one_session() {
    let base = token_endpoint(
        r#"{"access_token": "at", "refresh_token": "rt", "expires_in": 3600, "plan": "pro", "email": "dev@example.com", "user_id": "u1"}"#,
    )
    .await;
    let tmp = tempfile::TempDir::new().unwrap();
    let config = AuthConfig {
        client_id: "cli-test".into(),
        authorize_url: format!("{base}/authorize"),
        token_url: format!("{base}/token"),
        login_timeout_s: 20,
        ..AuthConfig::default()
    };
    let store = TokenStore::new(tmp.path().join("auth.json"));
    let client = AuthClient::new(&config, store.clone());

    let out = Shared::default();
    let mut handler_out = out.clone();
    let browser = {
        let out = out.clone();
        async move {
            // Act as the browser once the URL has been printed.
            let url = loop {
                if let Some(line) = out
                    .text()
                    .lines()
                    .find(|l| l.trim_start().starts_with("http"))
                {
                    break line.trim().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let redirect = param(&url, "redirect_uri");
            let state = param(&url, "state");
            let page = reqwest::get(format!("{redirect}?code=c0de&state={state}"))
                .await
                .unwrap();
            assert_eq!(page.status(), 200);
        }
    };
    let (code, ()) = tokio::join!(login(&client, false, &mut handler_out), browser);
    assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    let printed = out.text();
    assert!(
        printed.starts_with("Open this URL in a browser on this machine to sign in:\n  http"),
        "{printed}"
    );
    assert!(
        printed.ends_with("Logged in as dev@example.com  [plan: pro]\n"),
        "{printed}"
    );
    assert_eq!(store.load().unwrap().access_token, "at");

    let (mut out, mut err) = (Vec::new(), Vec::new());
    whoami(&client, &mut out, &mut err).await.unwrap();
    let line = String::from_utf8(out).unwrap();
    assert!(
        line.starts_with("dev@example.com  plan=pro  token_expires="),
        "{line}"
    );
    assert!(err.is_empty());

    let mut out = Vec::new();
    logout(&client, &mut out).await.unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "Logged out.\n");
    assert!(store.load().is_none());

    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        whoami(&client, &mut out, &mut err).await.unwrap(),
        ExitCode::SUCCESS
    );
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Not logged in. Run: xaft login\n"
    );
}
