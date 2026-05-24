# Context Window Management

## Problem

A `CodeAgent` working on a large refactor may accumulate 60,000+ tokens of conversation history across 20 turns. LLM context windows are finite and expensive. `xaft` manages context proactively.

## Three-Tier Strategy

```
Tier 1: Sliding window (40K tokens)
    Keep recent messages in full fidelity.
    Drop oldest when window fills.

Tier 2: Summarization (at 80% capacity)
    Run SummaryAgent on oldest 20K tokens.
    Replace with 500-token summary.
    Retain system prompt + recent messages.

Tier 3: Selective context injection
    Use semantic index to inject only relevant context.
    Instead of: "here is the whole codebase"
    Use: "here are the 5 most relevant files for this step"
```

## Context Budget Allocation

```
Total context window: 200,000 tokens (claude-3-5-sonnet)
────────────────────────────────────────────────
System prompt:         2,000 tokens (static)
Workspace context:     3,000 tokens (dynamic per step)
Tool schemas:          2,000 tokens (tool count dependent)
Conversation history: 40,000 tokens (sliding window)
Tool results:         remaining (LLM responses are max_tokens)
────────────────────────────────────────────────
Reserved for response: 4,096 tokens (max_tokens_per_turn)
```

## Workspace Context Injection

Before each agent run, inject only relevant workspace context:

```rust
pub async fn build_workspace_context(
    step: &PlanStep,
    index: &RepoIndex,
    workspace: &WorkspaceEditor,
    token_budget: usize,
) -> Result<String, XaftError> {
    // Search index for relevant files
    let relevant = index.search(&step.description, 5).await?;

    let mut context = String::new();
    let mut tokens_used = 0;

    context.push_str("## Relevant Files\n\n");

    for result in &relevant {
        let content = workspace.read(&result.path).await?;
        let file_tokens = estimate_tokens(&content);

        if tokens_used + file_tokens > token_budget { break; }

        context.push_str(&format!("### {}\n```rust\n{content}\n```\n\n", result.path.display()));
        tokens_used += file_tokens;
    }

    Ok(context)
}
```

## Token Estimation

```rust
pub fn estimate_tokens(text: &str) -> usize {
    // Rough estimate: 1 token ≈ 4 characters for English/code
    // Use tiktoken-rs for precise counting when available
    text.len() / 4
}
```

## References

- agtrs: `agtrs-runtime/src/memory.rs` (ShortTermMemory)
- agtrs guide: `guides/10-memory.md`
