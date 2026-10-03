# Handoff — G4b-5 (`session send` / `session control`)

Status: **NOT IMPLEMENTED.** `goal_implemented` was deliberately not called: neither
command exists, nothing is committed, and parity rows 8–12 are still ❌.

Work stopped because the turn budget ran out mid-integration. This file records
exactly where things stand so the next pass does not re-derive them.

## Tree state (compiles)

`cargo check --workspace` exits 0. `cargo test -p xaft-session --lib` is
**47 passed / 0 failed** (26 of them service tests). Nothing is committed.

- `crates/xaft-session/src/service.rs` — **untracked, kept.** The service core
  *and* the command envelope.
- `crates/xaft-session/src/lib.rs` — **modified, kept.** Exports
  `SessionCommand`, `CommandResult` alongside the existing names.
- `crates/xaft-cli/src/args.rs` — **edits reverted.** They were correct but left
  the crate uncompiling (see below). I chose a compiling tree over half-wired
  arguments.
- `crates/xaft-cli/src/commands/session_service.rs` — **parked out of the tree at
  `/tmp/session_service.rs.wip`.** `xaft-cli` therefore has no service commands
  at all yet.

## What the service core now does

`crates/xaft-session/src/service.rs` adds, on top of the G4b-4 core:

- `SessionCommand` (agenthicc `session_service/models.py:273`) — the envelope
  carrying `capabilities`, `payload`, `idempotency_key`, `expected_sequence`.
- `CommandResult` (`models.py:342`) with the `replayed` flag.
- `SessionService::submit()` (`service.py:740`) implementing all eleven kinds:
  `submit_message`, `cancel`/`interrupt`, `approve`/`reject`, `answer`,
  `resume`/`retry`, `archive`, `invoke_command`, `fork`, `delete`, `attach` —
  plus idempotent replay, a 409 `stale_sequence` conflict, and the
  archived-session guard (`attach`/`resume` only).
- A PRAGMA-guarded `deleted` column and a `service_commands` table, so a
  `sessions.db` created before this schema still migrates.
- The old `control()` method was **removed** so there is exactly one mutation
  path; its callers were rewritten to `submit`.

`SessionServiceError::unknown_kind` now returns agenthicc's exact text,
`unknown session command: {kind}` (code `unknown_command`). It previously said
`Unknown session command kind: {kind}`, which would have failed this goal's
"returns agenthicc's unknown-kind error as text" clause.

Service tests already passing include `submit_message_queues_a_turn_and_
acknowledges_it`, `submit_message_without_the_control_capability_is_denied`
(which asserts the denial is *inert*: no queue depth, no active turn, no event),
`cancel_is_visible_in_the_next_snapshot`, `archive_and_fork_both_work`,
`an_archived_session_accepts_only_attach_or_resume`,
`attach_is_the_one_kind_a_read_only_client_may_submit`,
`resubmitting_the_same_idempotency_key_replays_the_result`,
`a_stale_expected_sequence_is_a_conflict`,
`delete_retires_the_session_but_keeps_its_history` and
`command_result_json_keys_are_sorted`.

## Why `args.rs` was reverted

The edit replaced `SessionSubcommand::Export`/`Inspect` (which the new
`Commands::Sessions` group absorbs) and added five new variants, but
`commands/session.rs` still matched the old two — so `xaft-cli` did not compile.
Half-wired arguments are worse than none; the revert was deliberate.

## Remaining work, in order

1. **`args.rs`** — add to `SessionSubcommand`: `Create(SessionCreateArgs)`,
   `Send(SessionSendArgs)`, `Control(SessionControlArgs)`,
   `Events(SessionEventsArgs)`, `Export(SessionServiceExportArgs)`; drop its
   `Export`/`Inspect`; add top-level
   `Sessions(SessionsCommand)` / `SessionsSubcommand::{Export, Inspect}`.
   Arg structs were drafted with: `SessionCreateArgs { project_root (default
   ".", -p), agent, workflow, config }`; `SessionSendArgs { id, text, config }`;
   `SessionControlArgs { id, kind, payload (Option<String>), config }`;
   `SessionEventsArgs { id, after (default 0), config }`;
   `SessionServiceExportArgs { id, output, config }`.
2. **`commands/session.rs`** — match the new variants; route the old
   `Export`/`Inspect` arms to the new `sessions` group.
3. **`commands/sessions.rs`** — add `handle_sessions(&SessionsSubcommand)` and
   rewrite its module doc. It currently asserts "xaft has no session service at
   all — row 12 is still open", which is now false.
4. **`commands/mod.rs`** — `pub mod session_service;`.
   **`dispatch.rs`** — `Some(Commands::Sessions(..)) => sessions::handle_sessions(..)`.
5. **`session_service.rs`** — add `handle_session_send`/`render_session_send`
   and `handle_session_control`/`render_session_control`. Both submit with
   `CapabilitySet::cli()` and print
   `json.dumps(result.to_dict(), indent=2, sort_keys=True)`.
6. **`session show` needs a service fallback**, or the goal's clause "the new
   state is visible to a subsequent `session show`" is unsatisfiable:
   `handle_session_show` reads only `RuntimeDispatch`, which cannot resolve a
   `sess_*` service id. Add a `service.snapshot()` fallback *after* both existing
   local lookups fail, so no current behaviour changes.
7. Integration tests in `crates/xaft-cli/tests/`, then matrix rows, then
   **`./scripts/sandbox-build.sh --off` BEFORE `git add`**, then commit.

## Assumptions carried forward (clarification timed out; unconfirmed)

**(a) The goal's row numbers are off by one.** Measured in
`docs/parity-matrix.md:47-52`: row 8 = `session send`, 9 = `session control`,
10 = `session create`, 11 = `session events`, 12 = `session export`. §1c
(lines 137-138) repeats the error, crediting "8, 11, 12" to G4b-4 and
"9, 10" to G4b-5. Those must become 10-12 and 8-9.

**(b) `xaft session export` becomes the SERVICE export; the existing local-log
export moves to a new `xaft sessions export`.** This is forced, not cosmetic —
agenthicс has both commands writing different documents and they cannot share
one name. It changes rows 16/17's xaft-location *cell*; their ✅ status is
unchanged. If the user rejects the rename, the alternative is naming the service
export something else and keeping the divergence, which is worse and must not be
chosen silently.

**(c) Undecided: does "the result JSON contains the acknowledged message" mean
agenthicc's literal `data == {"accepted": true, "turn_id": ...}`, or a payload
that also carries the text?** I kept agenthicc's exact shape and test the text
via the durable `turn_queued` event. If the literal reading is required, add
`"text"` to `data` — an additive divergence that should be an explicit decision.