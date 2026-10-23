//! `/login` and `/logout` — the account sign-in of `xaft login` /
//! `xaft logout`, inside the TUI, over the same `xaft_runtime::auth` module
//! and the same `$XAFT_HOME/auth.json`.
//!
//! `/login` cannot block the event loop for the minutes a person may take in
//! the browser, so it prepares the login (PKCE, loopback listener, URL),
//! prints the URL, and finishes on a background task whose outcome arrives as
//! a transcript line through [`Notices`]. `/logout` is a short call (agenthicc
//! gives revocation 10 s) and runs as an ordinary async command.

use xaft_runtime::auth::{AuthClient, open_browser};

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult, Notices};

/// Start a login with `client` and finish it in the background.
///
/// Returns the lines to print now; the outcome (`Logged in as …` or
/// `Login failed: …`) is pushed to `notices` when the browser returns.
pub fn begin_login(client: &AuthClient, notices: Notices, browser: bool) -> CommandResult {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return CommandResult::Error("No async runtime available.".into());
    };
    let pending = match client.start_login() {
        Ok(p) => p,
        Err(e) => return CommandResult::Error(e.to_string()),
    };
    let url = pending.authorize_url().to_string();
    let wait = pending.timeout_secs();
    let opened = browser && open_browser(&url);
    runtime.spawn(async move {
        let line = match pending.complete().await {
            Ok(bundle) => format!(
                "Logged in as {}  [plan: {}]",
                bundle.display_name(),
                bundle.plan
            ),
            Err(e) => format!("Login failed: {e}"),
        };
        notices.push(line);
    });
    let lead = if opened {
        "Opening browser... if it doesn't open, visit:"
    } else {
        "Open this URL in a browser on this machine to sign in:"
    };
    CommandResult::Lines(vec![
        lead.to_string(),
        format!("  {url}"),
        format!("Waiting up to {wait}s for the sign-in to finish…"),
    ])
}

/// Sign out with `client`.
pub async fn finish_logout(client: AuthClient) -> CommandResult {
    match client.logout().await {
        Ok(_) => CommandResult::Lines(vec!["Logged out.".into()]),
        Err(e) => CommandResult::Error(e.to_string()),
    }
}

/// `/login`.
pub struct LoginHandler;

impl SlashHandler for LoginHandler {
    fn description(&self) -> &'static str {
        "Sign in to the configured account (OAuth 2.0 + PKCE in your browser)"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        match AuthClient::from_config(&ctx.config) {
            Ok(client) => begin_login(
                &client,
                ctx.services.notices.clone(),
                ctx.services.open_browser,
            ),
            Err(e) => CommandResult::Error(e.to_string()),
        }
    }
}

/// `/logout`.
pub struct LogoutHandler;

impl SlashHandler for LogoutHandler {
    fn description(&self) -> &'static str {
        "Sign out: revoke the stored tokens and delete them"
    }

    fn is_async(&self) -> bool {
        true
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Error("use async execute".into())
    }

    fn execute_boxed_async(
        &self,
        ctx: CommandContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>> {
        Box::pin(async move {
            match AuthClient::from_config(&ctx.config) {
                Ok(client) => finish_logout(client).await,
                Err(e) => CommandResult::Error(e.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use xaft_config::AuthConfig;
    use xaft_runtime::auth::{TokenBundle, TokenStore};

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(l) => l,
            CommandResult::Error(e) => vec![format!("ERROR {e}")],
            other => vec![format!("{other:?}")],
        }
    }

    async fn token_endpoint() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let body = r#"{"access_token":"at","refresh_token":"rt","expires_in":600,"email":"tui@example.com"}"#;
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        format!("http://{addr}")
    }

    /// `/login` returns at once with the URL; the outcome arrives later as a
    /// notice, once the browser has been sent back.
    #[tokio::test(flavor = "multi_thread")]
    async fn login_prints_the_url_and_reports_through_notices() {
        let base = token_endpoint().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let config = AuthConfig {
            authorize_url: format!("{base}/authorize"),
            token_url: format!("{base}/token"),
            login_timeout_s: 20,
            ..AuthConfig::default()
        };
        let store = TokenStore::new(tmp.path().join("auth.json"));
        let client = AuthClient::new(&config, store.clone());
        let notices = Notices::default();

        let printed = lines(begin_login(&client, notices.clone(), false));
        assert_eq!(
            printed[0],
            "Open this URL in a browser on this machine to sign in:"
        );
        let url = printed[1].trim().to_string();
        assert!(
            url.starts_with(&format!("{base}/authorize?client_id=xaft-cli&")),
            "{url}"
        );
        assert!(
            notices.drain().is_empty(),
            "nothing is reported before the browser returns"
        );

        let query = url.split_once('?').unwrap().1;
        let get = |k: &str| {
            query
                .split('&')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap()
                .replace("%3A", ":")
                .replace("%2F", "/")
        };
        reqwest::get(format!(
            "{}?code=c&state={}",
            get("redirect_uri"),
            get("state")
        ))
        .await
        .unwrap();
        let mut reported = Vec::new();
        for _ in 0..200 {
            reported = notices.drain();
            if !reported.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(reported, ["Logged in as tui@example.com  [plan: free]"]);
        assert_eq!(store.load().map(|b| b.access_token), Some("at".to_string()));

        // /logout clears the same store.
        assert_eq!(lines(finish_logout(client).await), ["Logged out."]);
        assert!(store.load().is_none());
    }

    #[tokio::test]
    async fn an_unconfigured_provider_is_refused_with_the_keys_to_set() {
        let tmp = tempfile::TempDir::new().unwrap();
        let client = AuthClient::new(
            &AuthConfig::default(),
            TokenStore::new(tmp.path().join("a.json")),
        );
        let out = lines(begin_login(&client, Notices::default(), false));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].starts_with("ERROR no account provider is configured"),
            "{out:?}"
        );
        assert!(out[0].contains("authorize_url"));
    }

    #[tokio::test]
    async fn logout_without_a_session_still_succeeds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TokenStore::new(tmp.path().join("auth.json"));
        store
            .save(&TokenBundle {
                access_token: "a".into(),
                refresh_token: String::new(),
                expires_at: 0.0,
                plan: "free".into(),
                email: String::new(),
                user_id: String::new(),
            })
            .unwrap();
        let client = AuthClient::new(&AuthConfig::default(), store.clone());
        assert_eq!(lines(finish_logout(client).await), ["Logged out."]);
        assert!(
            store.load().is_none(),
            "no revoke_url: the local tokens still go"
        );
    }
}
