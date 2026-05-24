# Distributed Runtime

## Overview

`xaft` v1 is single-process, single-machine. This document describes the architectural direction for distributed execution in v2+.

## Distributed Topology (Future)

```
                    ┌─────────────────┐
                    │  xaft CLI/TUI   │   ← User interface
                    │  (coordinator)  │
                    └────────┬────────┘
                             │ HTTP/WebSocket
                    ┌────────▼────────┐
                    │  xaft-server    │   ← Coordination API
                    │  (orchestrator) │
                    └────────┬────────┘
                             │
               ┌─────────────┼─────────────┐
               │             │             │
    ┌──────────▼──┐  ┌───────▼────┐  ┌───▼─────────┐
    │ xaft-worker │  │ xaft-worker│  │ xaft-worker  │
    │ CodeAgent   │  │ FixerAgent │  │ ReviewAgent  │
    │ (host A)    │  │ (host B)   │  │ (host C)     │
    └─────────────┘  └────────────┘  └─────────────-┘
```

## Worker Protocol

Workers expose a minimal gRPC/HTTP API:

```
POST /worker/accept   — Accept a task step
POST /worker/cancel   — Cancel in-progress step
GET  /worker/status   — Health + current load
GET  /worker/stream   — SSE stream of step execution
```

## Session Distribution Strategy

The orchestrator assigns steps to workers based on:
1. **Affinity**: Steps touching the same files prefer the same worker (cache locality)
2. **Load**: New steps go to the least-loaded worker
3. **Capability**: Steps requiring shell execution go to workers with the right tools

## Workspace Synchronization

Distributed execution requires workspace sync across workers:

```
Option A: Shared NFS mount (simple, single datacenter)
Option B: Git-based sync (clone + push worktree branches)
Option C: rsync before each step (explicit, auditable)
Option D: Content-addressed store (S3/GCS, future)
```

v2 will implement **Option B** (git-based sync) as it aligns with the existing worktree model.

## State Management

The `TaskRunner` and all checkpoints are stored in a shared database (PostgreSQL or Redis) accessible by all workers. Workers are stateless except for in-flight tool execution state.

## References

- agtrs: `agtrs-runtime/src/task.rs` (TaskRunner state machine)
- Future: `xaft-worker` crate (not yet implemented)
