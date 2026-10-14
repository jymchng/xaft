# [IMPLEMENTED] CostedProvider — Three-Tier Model Routing PRD

## Problem

`xaft-runtime` currently uses a single LLM for every agent in the pipeline:
planner, coder, QA, fixer, summarizer. This is wasteful:

- Planner and QA do **complex reasoning** — they benefit from the most
  capable (expensive) model.
- Coder does **high-volume code generation** — needs good code quality but
  less open-ended reasoning; a mid-tier model handles it well.
- Fixer and summarizer do **targeted, formulaic work** — cheap models are
  sufficient.

`agtrs-providers-router` ships `CostedProvider` which is already a direct
dependency of `xaft-runtime` (in Cargo.toml) but is not yet used.

## Goal

Wire `CostedProvider` into `xaft-runtime` so each agent tier uses the
appropriate model. All three models are configurable via `xaft-config` and
can be overridden per-run with env vars — no code changes needed to switch
models.

---

## Three-tier model assignment

| Tier | Agents | Rationale |
|------|--------|-----------|
| **Flagship** (expensive) | `planner`, `qa` | Open-ended task decomposition + code reasoning. Wrong decisions here break the whole pipeline. |
| **Standard** (mid-tier) | `coder`, `fixer` | Large context, high token count per turn. Quality matters but cost scales with output. |
| **Fast** (cheapest) | `summarizer`, plan-building fallback | Single-turn, low reasoning. Speed > cost here. |

Routing uses `CostedProvider::route_named()` with predicates on the system
prompt content — same technique as `06_codegen_cli.rs`.

---

## Config changes

### `xaft-config/src/types.rs` — new `ModelTierConfig`

```rust
/// Three-tier model configuration for cost-aware routing.
///
/// All fields are optional; if unset, `default_model` is used for all tiers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelTierConfig {
    /// Model for complex reasoning: planner + QA.
    /// Env: XAFT_FLAGSHIP_MODEL
    pub flagship_model: Option<String>,
    /// Model for code generation: coder + fixer.
    /// Env: XAFT_STANDARD_MODEL
    pub standard_model: Option<String>,
    /// Model for lightweight tasks: summarizer.
    /// Env: XAFT_FAST_MODEL
    pub fast_model: Option<String>,
}

impl ModelTierConfig {
    /// Resolve each tier, falling back to `default_model` if not set.
    /// Env vars take priority over file config.
    pub fn resolve(&self, default_model: &str) -> ResolvedTiers {
        let flagship = std::env::var("XAFT_FLAGSHIP_MODEL")
            .ok()
            .or_else(|| self.flagship_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        let standard = std::env::var("XAFT_STANDARD_MODEL")
            .ok()
            .or_else(|| self.standard_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        let fast = std::env::var("XAFT_FAST_MODEL")
            .ok()
            .or_else(|| self.fast_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        ResolvedTiers { flagship, standard, fast }
    }
}

pub struct ResolvedTiers {
    pub flagship: String,
    pub standard: String,
    pub fast: String,
}
```

### `XaftConfig` struct addition

```rust
pub struct XaftConfig {
    // ... existing fields ...
    #[serde(default)]
    pub model_tiers: ModelTierConfig,
}
```

### `~/.config/xaft/config.toml` (user-facing)

```toml
[models]
flagship_model = "claude-opus-4-7"     # planner, QA
standard_model = "claude-sonnet-4-6"   # coder, fixer
fast_model     = "claude-haiku-4-5"    # summarizer
```

---

## Provider construction changes

### `xaft-runtime/src/provider.rs` — new `build_tiered_provider()`

```rust
use agtrs_providers_router::costed::CostedProvider;

/// Build a `CostedProvider` that routes calls to one of three tiers based on
/// the agent's system prompt content.
///
/// Routing predicates inspect the first `System` message — the same technique
/// used in agtrs example 06_codegen_cli.rs — because the system prompt is the
/// only reliable per-call signal available inside `LlmProvider::complete()`.
pub fn build_tiered_provider(
    base_config: &LlmConfig,
    tiers: &ResolvedTiers,
) -> Arc<dyn LlmProvider> {
    // If all tiers resolve to the same model, skip routing overhead.
    if tiers.flagship == tiers.standard && tiers.standard == tiers.fast {
        return build_single_provider(base_config);
    }

    let flagship = build_provider_for_model(base_config, &tiers.flagship);
    let standard = build_provider_for_model(base_config, &tiers.standard);
    let fast     = build_provider_for_model(base_config, &tiers.fast);

    Arc::new(
        CostedProvider::builder()
            // Tier 1: flagship — planner + QA (complex reasoning)
            .route_named(
                "flagship (planner / qa)",
                |_opts, msgs| {
                    let sys = first_system_text(msgs);
                    sys.contains("requirements analyst")
                        || sys.contains("code reviewer")
                        || sys.contains("planner")
                        || sys.contains("decompose")
                        || sys.contains("review each file")
                },
                flagship,
            )
            // Tier 2: standard — coder + fixer (code generation)
            .route_named(
                "standard (coder / fixer)",
                |_opts, msgs| {
                    let sys = first_system_text(msgs);
                    sys.contains("expert software engineer")
                        || sys.contains("bug fixer")
                        || sys.contains("write_file")
                        || sys.contains("edit_file")
                },
                standard,
            )
            // Tier 3: fast — summarizer + any unmatched call
            .default(fast)
    )
}

/// Extract the text of the first System message (lowercased for matching).
fn first_system_text(msgs: &[Message]) -> String {
    msgs.iter()
        .find(|m| m.role == Role::System)
        .map(|m| m.text().to_lowercase())
        .unwrap_or_default()
}

/// Build a single-model provider from `base_config` with `model` overridden.
fn build_provider_for_model(base: &LlmConfig, model: &str) -> Arc<dyn LlmProvider> {
    let cfg = LlmConfig { model: model.to_string(), ..base.clone() };
    build_single_provider(&cfg)  // existing provider_from_config logic
}
```

### `xaft-runtime/src/runtime.rs` — use tiered provider

```rust
// Before (single provider):
let llm = provider::build_provider(&config)?;

// After (tiered):
let tiers = config.model_tiers.resolve(&config.model);
let llm = if tiers.all_same() {
    provider::build_provider(&config)?
} else {
    provider::build_tiered_provider(&config.llm, &tiers)
};
```

All downstream code (orchestrator, SubagentTool, HandoffOrchestrator) receives the
same `Arc<dyn LlmProvider>` as before — no other call sites need changing.

---

## Predicate keyword table

Keywords in each tier predicate match the system prompts in `xaft-agents` modules.
If you rename a system prompt, update the matching predicate.

| Tier | Trigger keywords | Source prompt constant |
|------|-----------------|------------------------|
| Flagship | `"requirements analyst"`, `"code reviewer"`, `"planner"`, `"decompose"`, `"review each file"` | `planner.rs`, `qa.rs` |
| Standard | `"expert software engineer"`, `"bug fixer"`, `"write_file"`, `"edit_file"` | `coder.rs`, `fixer.rs` |
| Fast | (default — no predicate) | `summarizer.rs` |

### Predicate robustness guidelines

- Keywords must be **substrings of the system prompt** (lowercase match), not exact
  sentences — system prompts evolve.
- Add new keywords to the appropriate tier when adding a new agent to `xaft-agents`.
- Avoid overlap: a keyword matching in the wrong tier causes silent mispricing.
  Run `predicate_overlap_test` (below) when adding keywords.

---

## Observability

Every routed call logs the selected tier via `tracing::debug!`:

```
DEBUG xaft_runtime::provider: llm_route tier="flagship (planner / qa)" model="claude-opus-4-7"
DEBUG xaft_runtime::provider: llm_route tier="standard (coder / fixer)" model="claude-sonnet-4-6"
DEBUG xaft_runtime::provider: llm_route tier="fast (default)" model="claude-haiku-4-5"
```

`CostedProvider::route_named()` already emits this via the `label` field.

---

## Files to modify / create

| File | Change |
|---|---|
| `xaft-config/src/types.rs` | Add `ModelTierConfig`, `ResolvedTiers` |
| `xaft-config/src/lib.rs` | Re-export `ModelTierConfig`, `ResolvedTiers` |
| `xaft-runtime/src/provider.rs` | Add `build_tiered_provider()`, `build_provider_for_model()`, `first_system_text()` |
| `xaft-runtime/src/runtime.rs` | Call `build_tiered_provider()` when tiers differ |
| `xaft-runtime/tests/tiered_provider.rs` | **New** — unit + integration tests |

No changes needed to `orchestrator.rs`, `dispatch.rs`, or any TUI code.

---

## Testing

### Unit tests (`provider.rs`)

```
all_same_tiers_skips_routing        // When flagship==standard==fast, returns single provider
flagship_predicate_matches_planner_prompt
flagship_predicate_matches_qa_prompt
standard_predicate_matches_coder_prompt
standard_predicate_matches_fixer_prompt
fast_is_default_for_summarizer
predicate_overlap_test              // No system prompt matches both flagship AND standard
env_var_overrides_config_flagship
env_var_overrides_config_standard
env_var_overrides_config_fast
model_tier_config_deserializes_from_toml
model_tier_config_defaults_to_base_model_when_unset
```

### Integration tests (`tests/tiered_provider.rs`)

```rust
/// Verify that the planner system prompt routes to the flagship provider.
#[test]
fn planner_routes_to_flagship() {
    let tiers = ResolvedTiers {
        flagship: "opus".into(),
        standard: "sonnet".into(),
        fast: "haiku".into(),
    };
    let base = LlmConfig { model: "haiku".into(), ..Default::default() };
    let provider = build_tiered_provider(&base, &tiers);

    // The provider is a CostedProvider — inspect which sub-provider gets called
    // by checking the model used via a TrackingProvider wrapper.
    let planner_msg = vec![Message::system(planner::PLANNER_SYSTEM_PROMPT_FRAGMENT)];
    let selected = probe_model(&provider, &planner_msg);
    assert_eq!(selected, "opus");
}

// Similar tests for qa, coder, fixer, summarizer.
```

### End-to-end test (`tests/session_e2e.rs` additions)

```rust
#[tokio::test]
async fn tiered_config_runs_full_workflow() {
    // Three distinct mock providers, each records which agent called it.
    // Verify planner used flagship, coder used standard, summarizer used fast.
    let (flagship_calls, standard_calls, fast_calls) = run_tiered_mock_workflow(...).await;
    assert!(flagship_calls.iter().any(|p| p.contains("planner")));
    assert!(standard_calls.iter().any(|p| p.contains("coder")));
    assert!(fast_calls.iter().any(|p| p.contains("summarizer")));
}
```

---

## Acceptance Criteria

1. `config.toml` supports `[models]` section; missing section silently falls back to single model.
2. `XAFT_FLAGSHIP_MODEL`, `XAFT_STANDARD_MODEL`, `XAFT_FAST_MODEL` env vars override config.
3. When all three tiers are the same model, `build_tiered_provider` returns a plain provider (no routing overhead).
4. Planner and QA system prompts route to flagship; coder and fixer to standard; summarizer to fast — verified by tests.
5. No predicate overlap (a single prompt must not match two tier predicates simultaneously).
6. All existing `session_e2e.rs` tests pass with tiered config using identical models for all tiers.
7. `tracing::debug!` log line emitted for every routed call with tier label and model name.
