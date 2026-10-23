//! `xaft login`, `xaft logout`, `xaft whoami` — agenthicc's
//! `cli/commands/auth.py`, over [`xaft_runtime::auth`].
//!
//! The printed lines are agenthicc's: `Opening browser... if it doesn't open,
//! visit:` + the URL, `Logged in as {email}  [plan: {plan}]`, `Logged out.`,
//! and `{email}  plan={plan}  token_expires={%Y-%m-%d %H:%M}` /
//! `Not logged in. Run: xaft login`.

use std::io::Write;

use xaft_runtime::ExitCode;
use xaft_runtime::auth::{AuthClient, WhoAmI, open_browser};

use crate::args::LoginArgs;
use crate::error::XaftError;

fn client() -> Result<AuthClient, XaftError> {
    let config = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides::default())?;
    Ok(AuthClient::from_config(&config)?)
}

/// Execute `xaft login`.
pub async fn handle_login(args: &LoginArgs) -> Result<ExitCode, XaftError> {
    login(&client()?, !args.no_browser, &mut std::io::stdout()).await
}

/// Execute `xaft logout`.
pub async fn handle_logout() -> Result<ExitCode, XaftError> {
    logout(&client()?, &mut std::io::stdout()).await
}

/// Execute `xaft whoami`.
pub async fn handle_whoami() -> Result<ExitCode, XaftError> {
    whoami(&client()?, &mut std::io::stdout(), &mut std::io::stderr()).await
}

/// Sign in with `client`, writing progress to `out`.
pub async fn login(
    client: &AuthClient,
    browser: bool,
    out: &mut dyn Write,
) -> Result<ExitCode, XaftError> {
    let pending = client.start_login()?;
    let url = pending.authorize_url().to_string();
    if browser {
        open_browser(&url);
        writeln!(
            out,
            "Opening browser... if it doesn't open, visit:\n  {url}"
        )?;
    } else {
        writeln!(
            out,
            "Open this URL in a browser on this machine to sign in:\n  {url}"
        )?;
    }
    writeln!(
        out,
        "Waiting up to {}s for the sign-in to finish...",
        pending.timeout_secs()
    )?;
    out.flush()?;
    let bundle = pending.complete().await?;
    writeln!(
        out,
        "Logged in as {}  [plan: {}]",
        bundle.display_name(),
        bundle.plan
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Sign out with `client`.
pub async fn logout(client: &AuthClient, out: &mut dyn Write) -> Result<ExitCode, XaftError> {
    client.logout().await?;
    writeln!(out, "Logged out.")?;
    Ok(ExitCode::SUCCESS)
}

/// Print the signed-in account. Like agenthicc, "not logged in" is an answer,
/// not a failure: the exit code is 0 either way.
pub async fn whoami(
    client: &AuthClient,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, XaftError> {
    match client.whoami().await {
        WhoAmI::SignedOut => writeln!(out, "Not logged in. Run: xaft login")?,
        WhoAmI::SignedIn(bundle) => writeln!(out, "{}", bundle.whoami_line())?,
        WhoAmI::Expired(stale, reason) => {
            writeln!(out, "{}", stale.whoami_line())?;
            writeln!(
                err,
                "xaft: the token has expired and could not be refreshed: {reason}"
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
