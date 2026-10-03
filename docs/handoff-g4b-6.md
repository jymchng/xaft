# Handoff — G4b-6 (`session serve`, parity row 13)

Status: **NOT IMPLEMENTED.** `goal_implemented` was deliberately not called.
Row 13 is still ❌, nothing is committed, and no transport exists.

The clarification `ask_user` timed out (request `0677e02d`, 300 s, four
questions, no answer). The five assumptions this pass would have needed a
decision on are recorded below **unconfirmed** — they are the first thing a
future pass should revisit.

## Why this goal could not be built as scoped

G4b-6 says "DEPENDS ON the session service core (the row 8/11/12 increment)"
and "Do NOT re-implement the service core". Measured: **that dependency is not
in the committed tree.**

| Fact | Evidence |
|---|---|
| The core is untracked WIP | `crates/xaft-session/src/service.rs` is untracked (72 530 B); `lib.rs` is modified |
| G4b-4's CLI wiring is absent | `commands/session_service.rs` is parked at `/tmp/session_service.rs.wip`; `commands/mod.rs` has no `session_service` |
| G4b-5's envelope commands are absent | `SessionSubcommand` has no `Send`/`Control`; rows 8–13 are all ❌ |
| Both prerequisites are `[postponed]` | goal list indices 30 (G4b-4) and 31 (G4b-5) |

So "do not re-implement the core" is unsatisfiable: row 13's transport serves
the core's snapshots and event log, and neither is reachable from `main`.

## Three further blockers, all measured

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

## The clarification that timed out

Four questions were asked (dependency/sequencing, HTTP stack, SSE scope,
`session show`). The answers this pass would have needed:

1. **Sequencing** — collapse G4b-4 + G4b-5 + G4b-6 into this pass (three
   commits), or land only the core and postpone row 13?
2. **HTTP** — hand-rolled on tokio, `tiny_http`, `axum`, or raw `hyper`?
3. **SSE** — add `subscribe()` to the core, ship the other 5 routes and defer
   SSE, or mark row 13 ✅ and document the divergence?
4. **`session show`** — add a service fallback, or retarget the test to
   `session export`?

No answer was recorded, so none was applied. Only measurement was done, plus a
`sessions`/`SessionSubcommand` restructure in `args.rs` that was **reverted**:
it is drafted at `/tmp/args.rs.g4b6.wip` and left the crate non-compiling
(`session.rs:28` passing the new `SessionServiceExportArgs` to the old
`sessions::export`, and `dispatch.rs:60` not matching the new
`Commands::Sessions` arm). A compiling tree beat half-wired arguments, as in
G4b-5.

## Tree state (compiles)

`cargo check --workspace` exits 0. Unchanged from G4b-5's handoff:

- `crates/xaft-session/src/service.rs` — untracked WIP (core + envelope).
- `crates/xaft-session/src/lib.rs` — modified, exports the service names.
- `crates/xaft-cli/src/args.rs` — pristine; new arg structs drafted in the WIP file.
- `docs/handoff-g4b-5.md` — still accurate for G4b-4/5.

Sandbox is ENGAGED (`./scripts/sandbox-build.sh --status`): 11 manifests carry
agtrs path rewrites, so `--off` must run before any `git add`.

## Row numbering

The goal's own row number is **correct**: `docs/parity-matrix.md:52` is
`| 13 | session serve | cli/commands/session_service.py | — | ❌ |`, and §1c
line 139 names G4b-6 as its owner. The off-by-one is in G4b-4's and G4b-5's
briefs only (they claim "8, 11, 12" and "9, 10"; the real split is 10–12 and
8–9). **Do not "correct" row 13.**

## Minimum viable path for a future pass

1. Get an answer on the HTTP stack — nothing can be built without one.
2. Land G4b-4 and G4b-5 (both already drafted in
   `/tmp/session_service.rs.wip` and `/tmp/args.rs.g4b6.wip`).
3. Then build `LocalSessionServer`'s Rust equivalent, keeping the security
   rule non-negotiable: loopback default, refuse a non-loopback `--host`
   without `--auth-token`, require the token on every content-bearing request,
   and never emit a secret in a served payload (the `mcp` redaction rules from
   `fffb6fb`).
4. Retarget the snapshot-comparison test to `session export`, not
   `session show`.
