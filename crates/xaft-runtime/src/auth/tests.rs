//! The login, refresh and logout flows against an in-process OAuth server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// One request the mock provider saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    form: HashMap<String, String>,
}

/// `path -> (status, body)`.
type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

struct MockProvider {
    base: String,
    routes: Routes,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl MockProvider {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (r, s) = (Arc::clone(&routes), Arc::clone(&seen));
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (routes, seen) = (Arc::clone(&r), Arc::clone(&s));
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let header = |name: &str| {
                        head.lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.trim()
                                    .eq_ignore_ascii_case(name)
                                    .then(|| v.trim().to_string())
                            })
                            .unwrap_or_default()
                    };
                    let len: usize = header("content-length").parse().unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                    let mut first = head.lines().next().unwrap_or("").split(' ');
                    let method = first.next().unwrap_or("").to_string();
                    let path = first.next().unwrap_or("").to_string();
                    let form = body
                        .split('&')
                        .filter(|p| !p.is_empty())
                        .map(|p| {
                            let (k, v) = p.split_once('=').unwrap_or((p, ""));
                            (decode(k), decode(v))
                        })
                        .collect();
                    seen.lock().unwrap().push(Seen {
                        method,
                        path: path.clone(),
                        authorization: header("authorization"),
                        form,
                    });
                    let (status, payload) = routes
                        .lock()
                        .unwrap()
                        .get(&path)
                        .cloned()
                        .unwrap_or((404, "{}".to_string()));
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                });
            }
        });
        Self { base, routes, seen }
    }

    fn route(&self, path: &str, status: u16, body: serde_json::Value) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_string(), (status, body.to_string()));
    }

    fn seen(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }

    fn config(&self) -> AuthConfig {
        AuthConfig {
            client_id: "test-cli".into(),
            authorize_url: format!("{}/authorize", self.base),
            token_url: format!("{}/token", self.base),
            revoke_url: format!("{}/revoke", self.base),
            userinfo_url: String::new(),
            login_timeout_s: 10,
            ..AuthConfig::default()
        }
    }
}

fn id_token(claims: serde_json::Value) -> String {
    let b64 = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
    format!(
        "{}.{}.signature",
        b64(br#"{"alg":"none"}"#),
        b64(claims.to_string().as_bytes())
    )
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

fn stored(expires_in: f64) -> TokenBundle {
    TokenBundle {
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        expires_at: now() + expires_in,
        plan: "pro".into(),
        email: "dev@example.com".into(),
        user_id: "user-42".into(),
    }
}

#[test]
fn pkce_matches_rfc_7636_appendix_b() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn bundles_expire_within_the_buffer_and_print_like_agenthicc() {
    let b = stored(30.0);
    assert!(b.is_expired(), "30s left is inside the 60s buffer");
    assert!(!stored(600.0).is_expired());
    assert!(b.is_pro());
    let line = b.whoami_line();
    assert!(
        line.starts_with("dev@example.com  plan=pro  token_expires="),
        "{line}"
    );
    let stamp = line.rsplit('=').next().unwrap();
    assert_eq!(stamp.len(), "2026-10-23 15:00".len(), "{stamp}");
    let anonymous = TokenBundle {
        email: String::new(),
        ..stored(600.0)
    };
    assert_eq!(anonymous.display_name(), "user-42");
}

#[test]
fn query_encoding_round_trips() {
    for s in ["a b&c=d", "ünï/?#", "plain-_.~"] {
        assert_eq!(decode(&encode(s)), s);
    }
    assert_eq!(decode("User+said+no%21"), "User said no!");
    assert_eq!(decode("100%"), "100%", "a stray percent is literal");
    assert_eq!(
        decode("%+1"),
        "% 1",
        "a sign is not a hex digit, so the percent is literal (and `+` a space)"
    );
}

#[tokio::test]
async fn an_unconfigured_provider_is_named_not_attempted() {
    let tmp = tempfile::TempDir::new().unwrap();
    let client = AuthClient::new(
        &AuthConfig::default(),
        TokenStore::new(tmp.path().join("a.json")),
    );
    assert!(!client.is_configured());
    let err = client.start_login().unwrap_err();
    assert_eq!(err, AuthError::NotConfigured);
    assert!(
        err.to_string()
            .contains("[auth] authorize_url and token_url")
    );
    assert_eq!(client.access_token().await, Err(AuthError::NotLoggedIn));
}

/// The whole flow: authorize URL → browser callback → code exchange with the
/// PKCE verifier → identity from the ID token → tokens stored 0600.
#[tokio::test]
async fn login_exchanges_the_code_with_the_verifier_and_stores_tokens() {
    let provider = MockProvider::start().await;
    provider.route(
        "/token",
        200,
        json!({
            "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 900,
            "token_type": "Bearer",
            "id_token": id_token(json!({"sub": "user-42", "email": "dev@example.com"})),
        }),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let client = AuthClient::new(
        &provider.config(),
        TokenStore::new(tmp.path().join("auth.json")),
    );

    let pending = client.start_login().unwrap();
    let url = pending.authorize_url().to_string();
    assert!(
        url.starts_with(&format!("{}/authorize?", provider.base)),
        "{url}"
    );
    assert_eq!(query_param(&url, "client_id").as_deref(), Some("test-cli"));
    assert_eq!(query_param(&url, "response_type").as_deref(), Some("code"));
    assert_eq!(
        query_param(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        query_param(&url, "scope").as_deref(),
        Some("openid profile email")
    );
    let redirect = query_param(&url, "redirect_uri").unwrap();
    assert_eq!(redirect, pending.redirect_uri());
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));
    let state = query_param(&url, "state").unwrap();
    let challenge = query_param(&url, "code_challenge").unwrap();

    let login = tokio::spawn(pending.complete());
    // The browser: a stray request first, then the provider's redirect.
    let browser = reqwest::Client::new();
    let favicon = browser
        .get(redirect.replace("/callback", "/favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(favicon.status(), 404);
    let page = browser
        .get(format!("{redirect}?code=the-code&state={}", encode(&state)))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.text().await.unwrap(), SUCCESS_PAGE);

    let bundle = login.await.unwrap().expect("login completes");
    assert_eq!(bundle.access_token, "at-1");
    assert_eq!(bundle.refresh_token, "rt-1");
    assert_eq!(bundle.email, "dev@example.com", "from the ID token");
    assert_eq!(bundle.user_id, "user-42");
    assert_eq!(bundle.plan, "free", "agenthicc's default plan");
    assert!(bundle.expires_at > now() + 800.0);
    assert_eq!(
        bundle.expires_at.fract(),
        0.0,
        "whole seconds, so the stored bundle reads back identical"
    );

    let exchange = provider.seen("/token");
    assert_eq!(exchange.len(), 1);
    let form = &exchange[0].form;
    assert_eq!(exchange[0].method, "POST");
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "test-cli");
    assert_eq!(form["code"], "the-code");
    assert_eq!(form["redirect_uri"], redirect);
    assert_eq!(
        pkce_challenge(&form["code_verifier"]),
        challenge,
        "the verifier sent is the one the challenge was made from"
    );
    assert!(form["code_verifier"].len() >= 43, "RFC 7636 minimum length");

    assert_eq!(client.current_bundle(), Some(bundle));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(client.store().path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn identity_falls_back_to_the_userinfo_endpoint() {
    let provider = MockProvider::start().await;
    provider.route(
        "/token",
        200,
        json!({"access_token": "at-9", "expires_in": 60}),
    );
    provider.route(
        "/userinfo",
        200,
        json!({"sub": "u-9", "email": "nine@example.com", "plan": "pro"}),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let config = AuthConfig {
        userinfo_url: format!("{}/userinfo", provider.base),
        ..provider.config()
    };
    let client = AuthClient::new(&config, TokenStore::new(tmp.path().join("auth.json")));
    let pending = client.start_login().unwrap();
    let state = query_param(pending.authorize_url(), "state").unwrap();
    let redirect = pending.redirect_uri().to_string();
    let login = tokio::spawn(pending.complete());
    reqwest::get(format!("{redirect}?state={}&code=c", encode(&state)))
        .await
        .unwrap();
    let bundle = login.await.unwrap().unwrap();
    assert_eq!(
        (bundle.email.as_str(), bundle.user_id.as_str()),
        ("nine@example.com", "u-9")
    );
    assert_eq!(bundle.plan, "pro");
    assert_eq!(bundle.refresh_token, "", "no refresh token was issued");
    let info = provider.seen("/userinfo");
    assert_eq!(info[0].authorization, "Bearer at-9");
}

#[tokio::test]
async fn the_callback_decides_denials_and_foreign_state() {
    let provider = MockProvider::start().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let client = AuthClient::new(
        &provider.config(),
        TokenStore::new(tmp.path().join("auth.json")),
    );

    let pending = client.start_login().unwrap();
    let redirect = pending.redirect_uri().to_string();
    let login = tokio::spawn(pending.complete());
    let page = reqwest::get(format!(
        "{redirect}?error=access_denied&error_description=User+said+no"
    ))
    .await
    .unwrap();
    assert_eq!(page.status(), 400);
    assert_eq!(
        login.await.unwrap(),
        Err(AuthError::Denied("access_denied: User said no".into()))
    );

    let pending = client.start_login().unwrap();
    let redirect = pending.redirect_uri().to_string();
    let login = tokio::spawn(pending.complete());
    reqwest::get(format!("{redirect}?code=x&state=forged"))
        .await
        .unwrap();
    assert_eq!(login.await.unwrap(), Err(AuthError::StateMismatch));
    assert!(provider.seen("/token").is_empty(), "no code was exchanged");
    assert!(client.current_bundle().is_none());
}

#[tokio::test]
async fn a_login_nobody_finishes_times_out() {
    let provider = MockProvider::start().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let config = AuthConfig {
        login_timeout_s: 1,
        ..provider.config()
    };
    let client = AuthClient::new(&config, TokenStore::new(tmp.path().join("auth.json")));
    let pending = client.start_login().unwrap();
    assert_eq!(pending.timeout_secs(), 1);
    assert_eq!(pending.complete().await, Err(AuthError::Timeout(1)));
}

#[tokio::test]
async fn a_rejected_or_unreachable_token_endpoint_is_reported() {
    let provider = MockProvider::start().await;
    provider.route(
        "/token",
        400,
        json!({"error": "invalid_grant", "error_description": "code expired"}),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let client = AuthClient::new(
        &provider.config(),
        TokenStore::new(tmp.path().join("auth.json")),
    );
    let err = client
        .exchange_code("c", "v", "http://127.0.0.1:1/callback")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AuthError::Rejected(m) if m.contains("invalid_grant: code expired")),
        "{err}"
    );

    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let offline = AuthConfig {
        token_url: format!("http://{closed}/token"),
        ..provider.config()
    };
    let client = AuthClient::new(&offline, TokenStore::new(tmp.path().join("auth.json")));
    let err = client.exchange_code("c", "v", "r").await.unwrap_err();
    assert!(
        matches!(&err, AuthError::Network(m) if m.starts_with("Auth request failed (ConnectError")
            && m.ends_with("Check your connection and try again.")),
        "{err}"
    );
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_and_stored_before_use() {
    let provider = MockProvider::start().await;
    provider.route(
        "/token",
        200,
        json!({"access_token": "new-access", "expires_in": 3600}),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let store = TokenStore::new(tmp.path().join("auth.json"));
    let client = AuthClient::new(&provider.config(), store.clone());

    store.save(&stored(3600.0)).unwrap();
    assert_eq!(client.access_token().await.unwrap(), "old-access");
    assert!(
        provider.seen("/token").is_empty(),
        "a fresh token is not refreshed"
    );

    store.save(&stored(10.0)).unwrap();
    assert_eq!(client.access_token().await.unwrap(), "new-access");
    let refresh = provider.seen("/token");
    assert_eq!(refresh[0].form["grant_type"], "refresh_token");
    assert_eq!(refresh[0].form["refresh_token"], "old-refresh");
    assert_eq!(refresh[0].form["client_id"], "test-cli");
    let saved = store.load().unwrap();
    assert_eq!(saved.access_token, "new-access");
    assert_eq!(
        saved.refresh_token, "old-refresh",
        "kept when the response omits it"
    );
    assert_eq!(
        (saved.plan.as_str(), saved.email.as_str()),
        ("pro", "dev@example.com")
    );
    assert!(!saved.is_expired());
}

#[tokio::test]
async fn whoami_refreshes_an_expired_token_and_reports_a_failed_refresh() {
    let provider = MockProvider::start().await;
    provider.route(
        "/token",
        200,
        json!({"access_token": "fresh", "refresh_token": "rt-2", "plan": "enterprise"}),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let store = TokenStore::new(tmp.path().join("auth.json"));
    let client = AuthClient::new(&provider.config(), store.clone());
    assert_eq!(client.whoami().await, WhoAmI::SignedOut);

    store.save(&stored(-100.0)).unwrap();
    let WhoAmI::SignedIn(me) = client.whoami().await else {
        panic!("an expired token is refreshed");
    };
    assert_eq!(
        (me.access_token.as_str(), me.refresh_token.as_str()),
        ("fresh", "rt-2")
    );
    assert_eq!(me.plan, "enterprise");

    provider.route("/token", 401, json!({"error": "invalid_grant"}));
    store.save(&stored(-100.0)).unwrap();
    let WhoAmI::Expired(stale, err) = client.whoami().await else {
        panic!("a failed refresh reports the stale bundle");
    };
    assert_eq!(stale.access_token, "old-access");
    assert!(matches!(err, AuthError::Rejected(_)), "{err}");
}

#[tokio::test]
async fn logout_revokes_both_tokens_and_always_clears() {
    let provider = MockProvider::start().await;
    provider.route("/revoke", 200, json!({}));
    let tmp = tempfile::TempDir::new().unwrap();
    let store = TokenStore::new(tmp.path().join("auth.json"));
    let client = AuthClient::new(&provider.config(), store.clone());

    store.save(&stored(600.0)).unwrap();
    let outcome = client.logout().await.unwrap();
    assert!(outcome.had_tokens);
    assert_eq!(outcome.revoked, ["refresh_token", "access_token"]);
    let revoked = provider.seen("/revoke");
    assert_eq!(revoked.len(), 2);
    assert_eq!(revoked[0].form["token"], "old-refresh");
    assert_eq!(revoked[0].form["token_type_hint"], "refresh_token");
    assert_eq!(revoked[1].form["token"], "old-access");
    assert_eq!(revoked[1].form["client_id"], "test-cli");
    assert!(store.load().is_none());

    // Revocation is best-effort: a failing endpoint still logs out locally.
    provider.route("/revoke", 503, json!({}));
    store.save(&stored(600.0)).unwrap();
    let outcome = client.logout().await.unwrap();
    assert!(outcome.revoked.is_empty());
    assert!(store.load().is_none());

    // Nothing stored: nothing to revoke, still fine.
    let outcome = client.logout().await.unwrap();
    assert!(!outcome.had_tokens);
}
