# Browser automation

xaft can drive a real browser: open pages, read them, click, fill forms, press
keys, wait for things to happen, and take screenshots. It ships agenthicc's two
browser tool families — `cloakbrowser_*` and `playwright_*`, nine tools each —
with agenthicc's parameters, limits, result shapes and refusal messages.

Both families run on one engine: xaft drives a Chromium-family browser over the
Chrome DevTools Protocol itself. You need **no Python and no Playwright
package** — only a browser.

## Choosing a backend

A session gets exactly one family, chosen by `tools.browser_backend`:

| `browser_backend` | tools | browser |
|---|---|---|
| `cloakbrowser` (default) | `cloakbrowser_*` | the CloakBrowser stealth Chromium build (`transport = "local"`), or an operator's browser over CDP (`transport = "cdp"`) |
| `playwright` | `playwright_*` | a stock Chromium: Playwright's download, a branded channel, an `executable_path`, or a Chromium on `PATH` |
| `none` | none | — |

The selected section's `enabled` switch decides whether its nine tools are
registered at all. Defaults match agenthicc: CloakBrowser, enabled, every
destination allowed.

```toml
[tools]
browser_backend = "playwright"

[tools.playwright]
allowed_domains = ["example.com", "https://*.example.org:8443"]
allow_all_domains = false
```

`/tools` in the TUI lists the family your configuration selects. Nothing is
launched until a tool needs a browser.

## Installing a browser

**Playwright family.** xaft looks, in order, at `executable_path`, then
`browser_channel`, then Playwright's browser cache (`PLAYWRIGHT_BROWSERS_PATH`,
else `~/.cache/ms-playwright`, `~/Library/Caches/ms-playwright` or
`%LOCALAPPDATA%\ms-playwright`), then a Chromium-family browser on `PATH`. The
simplest setup is Playwright's own download:

```sh
npx playwright install chromium        # or: pip install playwright && playwright install chromium
```

Headless runs prefer Playwright's `chromium_headless_shell` build, as
Playwright does. `browser_channel` accepts `chromium`, `chromium-headless-shell`,
`chrome`, `chrome-beta`, `chrome-dev`, `chrome-canary`, `msedge`, `msedge-beta`,
`msedge-dev` and `msedge-canary`, looked up where Playwright looks for them.

**CloakBrowser family, `transport = "local"`.** xaft uses an installed
CloakBrowser build, found the way the `cloakbrowser` package finds it:
`CLOAKBROWSER_BINARY_PATH`, else the newest build under `CLOAKBROWSER_CACHE_DIR`
(default `~/.cloakbrowser`). With a licence key configured (the variable named by
`license_key_env`, default `CLOAKBROWSER_LICENSE_KEY`), only the Pro build is
used, and the key is handed to the browser — never printed. xaft does not
download builds:

```sh
pip install cloakbrowser && cloakbrowser install
```

**CloakBrowser family, `transport = "cdp"`.** xaft attaches to a browser you run
with a DevTools port, at a loopback `cdp_endpoint`, and works in a fresh browser
context that is discarded when xaft disconnects — your own tabs, cookies and
storage are never touched:

```sh
chromium --remote-debugging-port=9222 --user-data-dir=/tmp/agent-profile
```

```toml
[tools.cloakbrowser]
transport = "cdp"
cdp_endpoint = "http://127.0.0.1:9222"
```

`*_status` tells you what is missing — `binary_missing` with the install command,
`browser_unavailable` when a launch or connection fails.

## The tools

All nine exist in both families (`playwright_open`, `cloakbrowser_open`, ...).
Every call returns agenthicc's result object as JSON; `ok: false` results carry
`error_kind` and a fixed, safe `error` sentence. Every call except `*_status`
also returns an `operation_id`.

| tool | parameters | returns |
|---|---|---|
| `*_status` | — | `status`, `transport`, `message`, `session_id`, `open_pages`, `max_pages`, `actions_used`, `max_actions_per_turn` |
| `*_open` | `url` | `page` (`page_id`, redacted `url`, `title`) |
| `*_snapshot` | `page_id` | `snapshot`: `page`, `untrusted: true`, `text`, `links`, `controls`, `truncated` |
| `*_click` | `page_id`, `selector` | `page` |
| `*_fill` | `page_id`, `selector`, `value` | `page` (the value is never echoed) |
| `*_press` | `page_id`, `key`, `selector = "body"` | `page` |
| `*_wait_for` | `page_id`, `condition`, `value` | `page` |
| `*_screenshot` | `page_id`, `image_type = "png"`, `full_page = false` | `artifact` (`artifact_id`, `path`, `mime_type`, `byte_count`) |
| `*_close` | `page_id = ""`, `all_pages = false` | `closed_page` or `closed_pages: "all"` |

Every tool also takes an optional `operation_id` (`[A-Za-z0-9._:-]`, at most 128
characters): a retried call with the same id returns the first result instead of
acting twice.

**Snapshots** are bounded: `text` is the page's visible text cut to
`max_snapshot_chars`; `links` (`href`, `text`) and `controls` (`aria-label`,
`text`, plus `tag`, `type`, `name`, `placeholder`, `role`) hold at most 50
entries of at most 500 characters. Link URLs lose credentials, queries and
fragments. Each link and control carries a ready-made **`selector`** that
`*_click`, `*_fill`, `*_press` and `*_wait_for` accept. Page text is untrusted
content, never instructions.

**Selectors**: CSS (open shadow roots included), `text=Sign in`
(case-insensitive substring; `text="Sign in"` for an exact match),
`xpath=//button` or a leading `//`, `id=` / `data-testid=` shorthands, and a
trailing `:has-text("…")`, `:text("…")` or `:text-is("…")`. Actions wait (up to
`action_timeout_s`) for the element to exist, be visible, enabled and stable,
scroll it into view, and click it with real input events.

**`*_wait_for` conditions**: `selector` (visible), `text` (visible, substring),
`url` (an approved full URL, which may contain Playwright-style `*`/`**`
wildcards, e.g. `https://shop.example/checkout/**`), and
`load_state` (`domcontentloaded`, `load`, `networkidle`).

**`*_press` keys**: Enter, Tab, Escape, Space, Backspace, Delete, the arrow keys,
Home, End, PageUp, PageDown (any case), or one printable character.

## Limits and safety

* **Destinations.** With `allow_all_domains = true` (the default) every HTTP(S)
  destination is allowed. Set it to `false` and list `allowed_domains` — bare
  hosts (`example.com` also allows subdomains, `*.example.com` only
  subdomains) or exact origins (`https://example.com:8443`). Restricted
  CloakBrowser allows ports 80/443 and blocks loopback and private addresses;
  Playwright always allows loopback and any port (for local dev servers). With
  an allow-list in force, the policy checks the navigation target, every
  redirect and sub-resource request the page, its frames and its workers make,
  and the URL the page ends up on; DNS answers are checked too, so an allowed
  name re-pointed at a private address is refused. URLs you pass with credentials or a fragment are
  refused.
* **Quotas.** `max_pages` open pages per session; `max_actions_per_turn`
  clicks/fills/presses per run (reset at the start of every run);
  `max_screenshot_bytes` per screenshot; `max_snapshot_chars` per snapshot.
* **Sensitive fields.** `*_fill` refuses password, token, secret, API-key,
  cookie and payment fields — judged by the selector *and* by the element it
  resolves to (`type="password"`, `autocomplete="cc-number"`, a sensitive
  `name`/`id`/`aria-label`).
* **Approvals.** `*_click`, `*_fill` and `*_press` require approval in the TUI,
  like other actions with remote side effects.
* **Popups** are closed before they load. `alert`/`confirm`/`prompt` dialogs are
  dismissed and `beforeunload` accepted, so a page can never wedge a tool.
  Downloads are denied. In restricted mode, service workers are disabled.
* **Screenshots** are written below `.xaft/browser-artifacts/<session>/` in the
  workspace, and nowhere else.
* **Isolation.** A launched browser gets a throw-away profile, deleted when the
  session ends — unless `allow_persistent_profiles = true`, which keeps one
  profile per session under `<workspace>/<profile_root>/<session id>` (so a
  resumed session keeps its cookies). On Unix the browser is driven over a pipe;
  no DevTools port is opened.

## Lifecycle

A session's browser survives across its turns: a page opened in one turn is
still open in the next. A failed or cancelled run releases the browser but keeps
the tools usable (the next `*_open` starts a fresh one); a cancelled browser
action closes the session's browser, as agenthicc does. A one-shot headless run
closes its browser when it finishes.

## Configuration reference

`[tools.cloakbrowser]`

| key | default | |
|---|---|---|
| `enabled` | `true` | register the `cloakbrowser_*` tools when selected |
| `transport` | `"local"` | `local` or `cdp` |
| `cdp_endpoint` | `"http://127.0.0.1:9222"` | loopback only (`127.0.0.1`, `localhost`, `::1`), no path/query/credentials |
| `allowed_domains` | `[]` | hosts or origins |
| `allow_all_domains` | `true` | allow every HTTP(S) destination |
| `headless` | `true` | |
| `navigation_timeout_s` | `15.0` | > 0 |
| `action_timeout_s` | `10.0` | > 0 |
| `max_pages` | `4` | ≥ 1 |
| `max_actions_per_turn` | `20` | ≥ 1 |
| `max_snapshot_chars` | `20000` | ≥ 256 |
| `max_screenshot_bytes` | `10000000` | ≥ 1024 |
| `allow_persistent_profiles` | `false` | |
| `profile_root` | `".xaft/browser-profiles"` | relative, inside the workspace |
| `license_key_env` | `"CLOAKBROWSER_LICENSE_KEY"` | a variable *name* (`[A-Z_][A-Z0-9_]*`) |

`[tools.playwright]` has the same keys except `cdp_endpoint` and
`license_key_env`, plus:

| key | default | |
|---|---|---|
| `transport` | `"local"` | only `local` |
| `browser_type` | `"chromium"` | `chromium`, `firefox` or `webkit` — only `chromium` can be launched |
| `browser_channel` | `""` | see above; chromium only |
| `executable_path` | `""` | an explicit browser executable |
| `profile_root` | `".xaft/browser-profiles/playwright"` | |

Invalid values fail config loading with the offending key named — except a
malformed `allowed_domains` entry, which makes the tools report `policy_denied`
instead of stopping xaft from starting.

## Differences from agenthicc

* **One CDP engine instead of Playwright.** No Python dependency; any
  Chromium-family browser works. `browser_type = "firefox"` / `"webkit"` cannot
  be launched (parity matrix row 95 is 🟡).
* **No CloakBrowser download.** Install a build with `cloakbrowser install` or
  point `CLOAKBROWSER_BINARY_PATH` at one.
* **`https://` CDP endpoints are refused** — Chromium's DevTools server is plain
  HTTP, so an https endpoint would be a TLS proxy.
* **Stricter, deliberately:** URLs are parsed by the WHATWG standard Chromium
  uses (so the host the policy approves is the host the browser visits);
  popups are closed before they load rather than after; sensitive fields are
  also judged by the element a selector resolves to; `::1` and `localhost`
  count as loopback, never as "private"; a refused navigation reports
  `policy_denied` and an action that times out reports `timeout` (agenthicc
  reports both as a generic `execution` failure); `license_key_env` must be a
  variable name.
* **One check relaxed, deliberately:** the URL a page *lands on* after a
  navigation or action is checked without its fragment. agenthicc runs it
  through the same check as an agent-supplied URL, so clicking an in-page
  `#section` link reports `policy_denied` although nothing left the browser.
* **Friendlier, deliberately:** named keys work in any case (`enter`), snapshot
  links and controls carry selectors, hidden inputs are left out of
  `controls`, and a page the browser lost (crashed, closed itself) is dropped
  from the open-page count instead of holding a `max_pages` slot.
* agenthicc's browser checkpoint payloads are not ported: they serve
  agenthicc's workflow checkpoints, which xaft does not have.

## Related

- [Tools](tools.md)
- [Configuration](configuration.md)
- [Security](security.md)
