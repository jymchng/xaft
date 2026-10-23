# Accounts: `xaft login`, `logout`, `whoami`

xaft can sign in to an OAuth 2.0 account provider and keep the session fresh,
the way agenthicc signs in to agenthicc.ai. agenthicc hard-codes its service;
xaft has no hosted service of its own, so you name the provider under `[auth]`.

## Configure a provider

```toml
[auth]
client_id     = "xaft-cli"                                  # the OAuth client registered for xaft
authorize_url = "https://id.example.com/oauth/authorize"
token_url     = "https://id.example.com/oauth/token"
revoke_url    = "https://id.example.com/oauth/revoke"       # optional: revoke on logout
userinfo_url  = "https://id.example.com/userinfo"           # optional: the account's email
scopes        = ["openid", "profile", "email"]              # add "offline_access" if refresh tokens need it
redirect_port = 0                                           # 0 = any free port
login_timeout_s = 120
```

| Key | Default | Meaning |
|---|---|---|
| `client_id` | `xaft-cli` | the client id xaft presents |
| `authorize_url` | — | where the browser signs in; with `token_url`, required for `login` |
| `token_url` | — | exchanges the authorization code and refreshes tokens |
| `revoke_url` | — | RFC 7009 revocation, called on logout (skipped when empty) |
| `userinfo_url` | — | OpenID Connect userinfo, used when the token response carries no email or id |
| `scopes` | `openid profile email` | requested scopes |
| `redirect_port` | `0` | loopback port of `http://127.0.0.1:<port>/callback`; fix it when the provider requires an exactly registered redirect URI |
| `login_timeout_s` | `120` | how long `login` waits for the browser |

The endpoints must be `https://` (plain `http://` is accepted only for
`127.0.0.1`, `localhost` and `::1`, for local providers and tests), and
`authorize_url` and `token_url` are set together; a config that breaks either
rule fails to load. The client must be a public client that allows the
authorization-code flow with PKCE and loopback redirects (RFC 8252).

## Commands

```console
$ xaft login
Opening browser... if it doesn't open, visit:
  https://id.example.com/oauth/authorize?client_id=xaft-cli&redirect_uri=...
Waiting up to 120s for the sign-in to finish...
Logged in as dev@example.com  [plan: free]

$ xaft whoami
dev@example.com  plan=free  token_expires=2026-10-23 16:42

$ xaft logout
Logged out.
```

- **`xaft login`** generates a PKCE verifier and challenge (S256) and a random
  `state`, opens the authorize URL (`$BROWSER`, else the platform opener) and
  prints it, then waits for the provider to redirect to
  `http://127.0.0.1:<port>/callback`. The code is exchanged at `token_url`
  with the verifier. `--no-browser` only prints the URL — open it in a browser
  on the same machine, since the redirect goes to its loopback address. A
  callback with the wrong `state` or an `error=` ends the login immediately.
- **`xaft whoami`** prints the stored account. An expired token is refreshed
  first, so the expiry shown is current; if the refresh fails the stored line
  is printed with a warning on stderr. "Not logged in. Run: xaft login" when
  nobody is signed in (exit code 0, as in agenthicc).
- **`xaft logout`** revokes the refresh token and the access token at
  `revoke_url` (best-effort) and deletes the local tokens.

In the TUI, `/login` prints the URL and keeps the session usable while you
sign in; the result ("Logged in as …" or "Login failed: …") appears in the
transcript when the browser comes back. `/logout` signs out.

## Where the tokens live

`$XAFT_HOME/auth.json` (default `~/.xaft/auth.json`), written atomically and
created with mode `0600` in a `0700` directory. The fields are agenthicc's:
`access_token`, `refresh_token`, `expires_at`, `plan`, `email`, `user_id`. A
token within 60 seconds of expiry is refreshed before use. agenthicc prefers
the OS keyring; xaft always uses the file (no keyring backend is linked).

## Differences from agenthicc

- The provider is configured, not `agenthicc.ai`.
- The redirect uses `127.0.0.1` (RFC 8252 §7.3) rather than `localhost`, and
  the flow sends and verifies `state`.
- Logout revokes the refresh token as well as the access token.
- The account's email and id fall back to the ID token's claims and the
  userinfo endpoint, as standard OpenID Connect providers do not put them in
  the token response; `plan` defaults to `free` as in agenthicc.
- `whoami` refreshes an expired token before printing.
- agenthicc's cached account tier (`account-tier.json`, used only to decide
  whether to show agenthicc.ai's sponsored messages) is not written.

## Related

- [Configuration](configuration.md)
- [Commands](commands.md)
