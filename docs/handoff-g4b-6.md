# Handoff — G4b-6 (`session serve`, parity row 13)

Status: **RESOLVED — row 13 is ✅, and rows 8-12 landed with it.** The blocker
analysis below is kept because it is the record of *why* the decisions were the
ones taken, not because the row is still open.

## Resolution

A later pass built the transport and turned rows 8-13 ✅. Against the four
questions the timed-out clarification left open:

1. **Sequencing** — the core, the command envelope and the transport all
   landed. The core is `crates/xaft-session/src/service.rs`; the CLI is
   `crates/xaft-cli/src/commands/session_service.rs` (`create`, `events`,
   `send`, `control`, `serve`), with `session export --service` beside it.
2. **HTTP stack** — **hand-rolled HTTP/1.1 on `tokio::net::TcpListener`**
   (`crates/xaft-session/src/transport.rs`). **Zero new crates:** the workspace
   already has `tokio = { features = ["full"] }`. Taken after this goal's
   `ask_user` timed out a third time, because it is the one option that needs no
   authorization — it adds no dependency and no new security surface. The cost
   is real and is owned in the module: xaft parses its own request line,
   headers and `Content-Length`. Requests are capped at `MAX_REQUEST_BYTES`,
   and every connection serves exactly one request.
3. **SSE** — served by **polling the durable `events(after)` log** every
   `POLL_INTERVAL` (200 ms), *not* by adding `subscribe()` to the core. The row
   is marked ✅ **with the divergence documented** rather than the route being
   dropped: the frame format (`id:`, `event: session`, `data:`) matches
   agenthicc's byte for byte, and a durable-log poll carries events appended by
   a *different* process, which an in-process push subscription cannot.
   `grep -c subscribe crates/xaft-session/src/service.rs` is still **0** — the
   core was not re-implemented.
4. **`session show`** — **not** given a service fallback. The test clause was
   retargeted instead, exactly as §3 below concluded: a `SessionSnapshot` and an
   `AgentSession` are structurally different documents, so no fallback could
   make them "match". Row 6 is untouched.

Three details worth carrying forward:

- **`--auth-token` also reads `XAFT_SESSION_AUTH_TOKEN`** (`args.rs`). A bearer
  token in `argv` is visible to anything that can read `/proc`, and that token
  is the only thing between a non-loopback bind and anonymous access to whole
  session histories. The flag still wins, so agenthicc parity is unchanged.
- **Readiness is one JSON line**, in agenthicc's shape including
  `schedule_service`, which is always `"disabled"` because xaft has no
  schedules subsystem. Keeping the key means a client that parses agenthicc's
  readiness line needs no xaft branch.
- **The three security rules this brief calls non-negotiable are each pinned by
  a test**: loopback default; refusal of a non-loopback `--host` with no token
  (`TransportError::NonLoopbackWithoutToken`); the token required on *every*
  route including `/health`; and a test that plants the token and asserts it
  never appears in a served payload.
- **Three wire-level reconciliations with agenthicc's `transport.py`** were
  found by reading the Python rather than by testing, and each is now pinned:
  agenthicc's client-id header (`X-Agenthicc-Client`) is accepted as a fallback
  to xaft's `X-Xaft-Client`, for the same reason the readiness line keeps
  `schedule_service` — a client written against agenthicc should need no xaft
  branch; the bare `OPTIONS` preflight now carries agenthicc's
  `Access-Control-Allow-Headers`/`Access-Control-Allow-Methods` (it previously
  answered `204` with no headers at all, which would fail a browser preflight);
  and `204` responses no longer carry a `Content-Length`, which RFC 7230 §3.3.2
  forbids and `aiohttp` omits.

Two defects in that pass's own first draft were caught by its tests and fixed
rather than papered over: two hand-counted `Content-Length` values were wrong
by four and nine bytes (the tests now compute the length from the body), and
one assertion expected a `submit_message` payload shape the service rejects.

## What a future pass should read this document for

The blocker analysis below is the rationale, not an open state. The
clarification `ask_user` timed out (request `0677e02d`, 300 s, four questions,
no answer); three of the four questions were then answered by taking the
add-nothing option, and the fourth by measurement.

## Why this goal could not be built *in that pass* (measured then)

G4b-6 says "DEPENDS ON the session service core (the row 8/11/12 increment)"
and "Do NOT re-implement the service core". Measured at that commit: **that
dependency was not in the committed tree.**

| Fact | Evidence |
|---|---|
| The core is untracked WIP | `crates/xaft-session/src/service.rs` is untracked (72 530 B); `lib.rs` is modified |
| G4b-4's CLI wiring is absent | `commands/session_service.rs` is parked at `/tmp/session_service.rs.wip`; `commands/mod.rs` has no `session_service` |
| G4b-5's envelope commands are absent | `SessionSubcommand` has no `Send`/`Control`; rows 8–13 are all ❌ |
| Both prerequisites are `[postponed]` | goal list indices 30 (G4b-4) and 31 (G4b-5) |

So "do not re-implement the core" was unsatisfiable *at that commit*: row 13's
transport serves the core's snapshots and event log, and neither was reachable
from `main`. Both prerequisites have since landed, which is why the pass that
resolved this goal had something to point the transport at.

## Three further blockers, all measured, all resolved

### 1. xaft has no HTTP stack at all

```
grep -rn "hyper\|axum\|warp\|tiny_http\|actix" --include=Cargo.toml .   → 0 hits
```

`/root/rust_projects/.xaft-agtrs-sandbox/agtrs/` has no HTTP crate either
(`ls -d *http* *web* *axum*` → nothing). agenthicc gets `aiohttp` from Python's
ecosystem; there is no Rust equivalent already in the dependency graph. Adding
one is a real decision — `axum`+`hyper`+`tokio-util` (idiomatic, largest
surface), `tiny_http` (small, blocking), or hand-rolled on `tokio::net::TcpListener`
(zero new deps, xaft owns the parsing). **This pass had no authorisation to add
a dependency**, and the sibling-repo authorization gate makes pulling crates in
especially risky.

### 2. SSE needs a service primitive that does not exist

agenthicc's `_stream` route calls `service.subscribe()` → `SessionSubscription`
(an async iterator). Measured on the WIP core:

```
grep -c subscribe crates/xaft-session/src/service.rs   → 0
```

The core exposes only `create_session`, `snapshot`, `list_sessions`, `events`,
`submit`. There is no subscription primitive, so `GET /v1/sessions/{id}/stream`
cannot be built without adding one — which is re-implementing the core, the
one thing the goal forbids. A transport-local poll of the existing durable
`events(after)` log would work and is worth considering, but it is a semantic
divergence from agenthicc's push subscription and should be a decision, not a
silent substitution.

### 3. The TEST clause "asserts it matches what `session show` prints" is
### defective, not merely unsatisfiable

`commands/session.rs:95-122` — `handle_session_show` resolves through
`RuntimeDispatch` only (`load_session`, then a cwd-scoped prefix match), so it
cannot resolve a `sess_*` service id. But that is not the whole problem:

- `session show` renders an `AgentSession` — id, status, task, timestamps
  (`print_session`, `commands/session.rs:125+`).
- the transport returns a `SessionSnapshot` — `schema_version`, `state`,
  `queue.depth`, `last_event_sequence`, `capabilities`, `approvals_pending`,
  `terminals_running`, … (`crates/xaft-session/src/service.rs:279-313`).

These are **structurally different documents**. No fallback to `session show`
can make them "match", because there is nothing to match. The natural
comparison target is `xaft session export`'s snapshot or the service API
directly. **Row 6 should not be given a service fallback on the strength of this
clause** — it would be a change to an already-✅ row justified by a test that
cannot pass either way.

## The clarification that timed out (and what answered it)

Four questions were asked (dependency/sequencing, HTTP stack, SSE scope,
`session show`). The answers this pass would have needed:

1. **Sequencing** — collapse G4b-4 + G4b-5 + G4b-6 into this pass (three
   commits), or land only the core and postpone row 13?
2. **HTTP** — hand-rolled on tokio, `tiny_http`, `axum`, or raw `hyper`?
3. **SSE** — add `subscribe()` to the core, ship the other 5 routes and defer
   SSE, or mark row 13 ✅ and document the divergence?
4. **`session show`** — add a service fallback, or retarget the test to
   `session export`?

No answer was recorded, so none was applied *in that pass*. Only measurement was
done, plus a `sessions`/`SessionSubcommand` restructure in `args.rs` that was
**reverted**:
it is drafted at `/tmp/args.rs.g4b6.wip` and left the crate non-compiling
(`session.rs:28` passing the new `SessionServiceExportArgs` to the old
`sessions::export`, and `dispatch.rs:60` not matching the new
`Commands::Sessions` arm). A compiling tree beat half-wired arguments, as in
G4b-5.

## Tree state (resolved; compiles)

`cargo check -p xaft-session -p xaft-cli --all-targets` exits 0, and
`cargo test -p xaft-session` plus `cargo test -p xaft-cli --lib` are green.

- `crates/xaft-session/src/service.rs` — **committed**: the capability-gated
  core with the durable event log.
- `crates/xaft-session/src/transport.rs` — **new**: `LocalSessionServer`, the
  hand-rolled HTTP/1.1 + SSE attachment transport (14 tests).
- `crates/xaft-session/src/lib.rs` — exports the service and transport names.
- `crates/xaft-cli/src/commands/session_service.rs` — `create`, `events`,
  `send`, `control`, `serve`, plus `export_document` for `session export
  --service`.
- `crates/xaft-cli/src/args.rs` —
  `SessionSubcommand::{Create,Events,Send,Control,Serve}`.
- `docs/cli-command-tree.txt` — regenerated; it now carries `session serve`.

Sandbox note for the next pass: `./scripts/sandbox-build.sh --status` reports
**7** manifests carrying agtrs path rewrites, not the 11 this document recorded
originally, and `--off` must still run before any `git add`.

## Row numbering

The goal's own row number is **correct**: `docs/parity-matrix.md:52` is
`| 13 | session serve | cli/commands/session_service.py | — | ❌ |`, and §1c
line 139 names G4b-6 as its owner. The off-by-one is in G4b-4's and G4b-5's
briefs only (they claim "8, 11, 12" and "9, 10"; the real split is 10–12 and
8–9). **Do not "correct" row 13.**

## Minimum viable path (all four steps taken)

1. ~~Get an answer on the HTTP stack.~~ Answered by taking the option that
   needs no answer: zero new crates, hand-rolled on `tokio`.
2. ~~Land G4b-4 and G4b-5.~~ Landed — rows 8-12 are ✅.
3. ~~Build `LocalSessionServer`'s Rust equivalent~~, keeping the security rule
   non-negotiable: loopback default, refuse a non-loopback `--host` without
   `--auth-token`, require the token on every content-bearing request, and never
   emit a secret in a served payload (the `mcp` redaction rules from
   `fffb6fb`). **Built, and each clause is a test** — see the Resolution
   section above.
4. ~~Retarget the snapshot-comparison test to `session export`, not
   `session show`.~~ Done: `xaft session export --service` produces the
   `{schema_version, snapshot, events}` bundle, and `session show` was left
   alone.
