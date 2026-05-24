 Implementation Priority List — xaft-prd-z-ai
  
  Key difference from the other PRD

  This version inlines agtrs crates (e.g. agtrs-core, agtrs-signal listed separately) and has cleaner v0.1/v0.2/v0.3 versioning. Most agtrs primitives already
  exist in /root/rust_projects/agtrs — xaft builds the application layer.

  ---
  P0 — v0.1 MVP (unblock everything)

  1. xaft-config (~1,500 LOC)
  Config loading: .xaft.toml + env + CLI flags merged. Nothing can start without config. Minimal API key + model name. Block on nothing.

  2. xaft-cli (~1,000 LOC)
  clap entry point → xaft-runtime. xaft run "task" command. xaft-cli depends only on xaft-runtime + xaft-config.

  3. xaft-tools (~6,000 LOC, first pass)
  ReadFileTool, WriteFileTool, EditFileTool, BashExecTool, GitStatusTool. Thin wrappers over agtrs-workspace/agtrs-git/agtrs-shell. Largest P0 crate but fully
  parallelizable with agent work.

  4. xaft-agent (~4,000 LOC)
  XaftAgent wrapping agtrs Agent trait with lifecycle hooks: git auto-commit on on_finish, streaming emission on before_llm_call. PlanModeAgent uses
  OneShotPlanner → IterativeRefinementPlanner. Core execution.

  5. xaft-runtime (~3,000 LOC)
  XaftRuntime struct: compose AgentExecutor + SignalBus + WorkspaceStore + GitRepo. Boot sequence. Provider init (CostedProvider → FallbackProvider). Main event
  loop consuming StreamEvent.

  ---
  P1 — v0.1 complete + v0.2 start (differentiators)

  6. xaft-tui (~5,000 LOC)
  Ratatui app consuming SignalBus events. Minimum viable: token streaming pane + approval dialog. 60fps rendering loop. Without TUI, xaft is just a headless CLI —
   functional but uncompetitive.

  7. xaft-session (~2,000 LOC)
  SessionManager + SqliteSessionStore. Save/resume via ConversationStore. Session IDs. Required for any task longer than a single turn.

  8. Approval gate integration
  requires_confirmation per-tool + RiskLevel classification + modal approval in TUI. Non-negotiable per design principles. Low/Medium auto-approve; High gates.

  9. xaft-stream (~2,500 LOC)
  StreamEvent → TUI bridge. Backpressure. Headless mode (JSON output for CI). SSE bridge deferred to P2. Needed before TUI can consume properly.

  ---
  P2 — v0.2 features

  10. Multi-agent: TeamMode
  CoordinatorExecutor + CollaborateExecutor from agtrs-runtime wired into xaft-runtime. Enables parallel subagent delegation with SubagentTool<T>.

  11. xaft-index (~3,000 LOC)
  Tree-sitter symbol extraction + fuzzy search. Repository-scale context. Needed once tasks exceed single-file scope. SemanticSearch (embeddings) is P3.

  12. xaft-mcp (~2,000 LOC)
  MCP client + tool registration bridge. Enables third-party tools without rebuilding xaft.

  13. TreeOfThoughtPlanner
  Already in agtrs-runtime. Wire into xaft-agent's planner selection heuristic.

  14. xaft-shell refinements
  Sandbox config (allowlist), audit log, output streaming improvements. Base shell already exists in agtrs-shell.

  ---
  P3 — v0.3 / v1.0
  
  15. xaft-stream SSE bridge (Axum)
  Remote access over HTTP. Needed for CI/CD + team scenarios. sse feature flag.

  16. Semantic search (embeddings in xaft-index)
  Voyage/OpenAI embeddings + tantivy. Upgrade from tree-sitter fuzzy to semantic similarity.

  17. Workflow DAG execution
  Parallel task graph. Complex multi-agent coordination. Blocked on solid sequential path.

  18. xaft-proc-macros #[tool]
  Declarative tool definition macro. Nice-to-have; not blocking since manual impl Tool works.

  19. Guardrails plugin system
  Custom Guardrail trait implementations. Security audit use case.

  ---
  Dependency order (strict)

  xaft-config
    → xaft-cli
    → xaft-tools (parallel)
    → xaft-agent
      → xaft-runtime ← P0 complete
        → xaft-stream
          → xaft-tui ← P1 complete
          → xaft-session
        → multi-agent (TeamMode)
        → xaft-index
        → xaft-mcp ← P2 complete
          → SSE bridge
          → Workflow DAG ← P3

  ---
  Start: xaft-config + xaft-tools + xaft-agent in parallel → xaft-runtime → binary that can xaft run "fix this bug" end-to-end. That's v0.1 without TUI (~2
  weeks). TUI + session = v0.1 complete (~4 weeks).