# Escalation & Plan Parsing

The transition from free-form LLM output to structured workflow actions is one of the most fragile points in any agent system. The LLM produces natural language; the orchestrator needs typed data structures. The `parse_plan_result()` function bridges this gap with a heuristic parsing strategy that gracefully degrades from strict to lenient, ensuring that the workflow always produces a usable plan — even if the LLM's output doesn't perfectly conform to the expected format. This page covers the parsing pipeline, escalation policies, and the classification logic that determines whether a task requires code changes.

---

## The Parsing Problem

LLMs are not JSON APIs. When asked to produce a structured plan, they may:

- Emit valid JSON — the ideal case, but rare without explicit JSON-mode prompting.
- Use numbered lists (`1.`, `2.`, `3.`) or parenthesized numbers (`1)`, `2)`, `3)`) instead of JSON arrays.
- Mix structured sections with free-form commentary.
- Omit required fields or add unexpected ones.
- Use inconsistent indentation, line breaks, or delimiters.

A brittle parser that only accepts one format will fail frequently, causing the workflow to error out on tasks that are otherwise well-understood. The xaft parsing strategy is to try the strictest format first and progressively fall back to more lenient interpretations, always producing a `PlanResult` rather than an error.

---

## `parse_plan_result()`

```rust
pub fn parse_plan_result(raw: &str) -> PlanResult
```

The function implements a three-stage parsing pipeline:

```mermaid
flowchart TD
    Raw[Raw LLM output] --> S1{Stage 1:<br/>JSON parse?}
    S1 -->|Success| JSON[Extract fields<br/>from JSON object]
    S1 -->|Failure| S2{Stage 2:<br/>Numbered list pattern?}
    S2 -->|Match| List[Extract steps from<br/>1. / 1) patterns]
    S2 -->|No match| S3{Stage 3:<br/>Contains INFORMATIONAL?}
    S3 -->|Yes| Info[PlanResult::Informational]
    S3 -->|No| Default[PlanResult::Coding<br/>with raw text as summary]
    JSON --> Result[PlanResult]
    List --> Result
    Info --> Result
    Default --> Result
```

### Stage 1: JSON Parse

The parser first attempts to parse the entire output as a JSON object. If successful, it extracts the following fields:

| JSON Key | Type | Maps To |
|----------|------|---------|
| `type` | `"informational"` or `"coding"` | Determines `PlanResult` variant |
| `answer` | `string` | `PlanResult::Informational.answer` |
| `summary` | `string` | `CodingPlan.summary` |
| `steps` | `string[]` or `object[]` | `CodingPlan.steps` |
| `files_to_modify` | `string[]` | `CodingPlan.files_to_modify` |
| `constraints` | `string[]` | `CodingPlan.constraints` |

When `steps` is an array of objects, each object may contain `description`, `target_file`, and `tool_hint` fields. When it's an array of strings, each string becomes a `PlanStep.description` with `target_file` and `tool_hint` set to `None`.

The JSON parser also handles partial JSON — if the LLM wraps its output in markdown code fences (````json ... ````) or includes JSON within a larger text block, the parser extracts the JSON substring and parses that. This heuristic uses a simple brace-matching algorithm to find the outermost `{...}` pair.

### Stage 2: Numbered List Pattern

If JSON parsing fails, the parser scans for numbered list patterns using a regex:

```
/^(\d+)[.)]\s+(.+)$/m
```

This matches lines like:

```
1. Create the CacheTool struct in src/tools/cache.rs
2. Implement the Tool trait with get, set, delete operations
3. Register CacheTool in src/tools/mod.rs
```

Each matched line becomes a `PlanStep` with the captured text as the `description`. The parser also attempts to extract file paths from the step description using a heuristic: any substring matching `src/...rs`, `lib/...`, or similar common path patterns is extracted as a `target_file`. This heuristic is imperfect but works well for typical Rust and web project structures.

Additionally, the parser looks for section headers like "Summary:", "Files to modify:", "Constraints:" to extract the corresponding `CodingPlan` fields. Section headers are matched case-insensitively and support optional colons and whitespace.

### Stage 3: Fallback to Default

If neither JSON nor numbered-list parsing succeeds, the parser applies the final fallback:

1. **Check for "INFORMATIONAL:" prefix**: If the raw text starts with "INFORMATIONAL:" (case-insensitive), the parser extracts the text after the prefix as the informational answer. This handles the case where the LLM correctly classified the task but didn't use JSON format.

2. **Default to `CodingPlan`**: If no informational marker is found, the parser wraps the entire raw text as a `CodingPlan` with `summary = raw_text`, `steps = []`, `files_to_modify = []`, and `constraints = []`. This is the most lenient interpretation — it assumes the task requires coding and lets the Coder agent interpret the raw text as a plan. While this produces less structured output, it ensures that the workflow never dead-ends due to a parsing failure. The Coder agent is generally capable of extracting actionable information from free-form plan text.

---

## Task Classification

Before parsing begins, the planner must classify the task as informational or coding. This classification happens at two levels:

### LLM-Level Classification

The planner's prompt explicitly instructs the LLM to classify the task. The LLM may:

- Return a JSON object with `"type": "informational"` or `"type": "coding"`.
- Prefix its output with "INFORMATIONAL:" for informational tasks.
- Implicitly classify by producing a coding plan (anything with steps and files is treated as coding).

The parser respects all three signals, prioritizing explicit over implicit.

### Heuristic-Level Classification

When the LLM's output is ambiguous, the parser applies heuristics:

| Heuristic | Signal | Classification |
|-----------|--------|----------------|
| Contains file paths (e.g., `src/`, `lib/`) | Coding likely | Coding |
| Contains action verbs (e.g., "add", "create", "fix", "refactor") | Coding likely | Coding |
| Contains question words (e.g., "what", "how", "why") | Informational likely | Informational |
| No actionable content | Informational likely | Informational |

These heuristics are applied only in the Stage 3 fallback — Stages 1 and 2 have enough structure to determine classification without heuristics.

---

## Escalation Policies

Escalation in xaft refers to the process by which the workflow handles situations where the initial plan is insufficient or the coding task proves more complex than expected. There are three escalation mechanisms:

### 1. Planner → Coder Escalation

When the planner produces a coding plan, it implicitly escalates from analysis to implementation. This is the normal flow. However, the planner can also perform **partial escalation** — handing off to the Coder with a plan that acknowledges uncertainty:

```
Summary: Add rate limiting to the API endpoint

Steps:
1. Read the current API handler in src/api/mod.rs to understand the routing structure
2. Identify the middleware chain and determine where rate limiting should be inserted
3. Add rate limiting middleware (approach depends on step 2 findings)
4. Add unit tests for rate-limited endpoints

Uncertainties:
- The middleware approach depends on the current routing framework (Axum vs Actix)
- The rate limit configuration format (fixed window vs sliding window) is not specified
```

This partial plan is perfectly valid. The Coder reads the codebase first (step 1–2) and then makes informed decisions about steps 3–4. The planner doesn't need to know every detail — it just needs to set the right direction.

### 2. QA → Fixer Escalation

When the QA agent finds issues, it escalates to the Fixer via `RequestFixTool`. This is the most common escalation path and is designed to be tight:

- The QA agent must provide specific, actionable fix requests. Vague complaints like "this doesn't look right" are insufficient — the QA agent must identify the exact problem, the file and line, and the expected behavior.
- The Fixer addresses only the QA agent's findings. It does not re-plan or re-architect — it fixes what was flagged and hands back to QA.

This constrained escalation prevents scope creep. Without it, the Fixer might "improve" code that QA already approved, introducing new issues and creating an infinite loop.

### 3. Workflow-Level Escalation

When the handoff count approaches `max_handoffs`, the orchestrator can escalate by:

- **Truncating the QA→Fixer loop**: If the workflow has been cycling between QA and Fixer for more than half the handoff budget, the orchestrator may decide to accept the current state rather than continue cycling. This is controlled by a configurable threshold (default: 70% of `max_handoffs`).
- **Returning a partial result**: Instead of failing with `AgtrsError::MaxHandoffsExceeded`, the orchestrator returns a `WorkflowResult` with `approved: false` and includes the most recent QA feedback so the user can assess the situation.

These escalation policies ensure that workflows always produce *some* output, even when they can't reach a perfect conclusion. The user gets enough information to decide whether to accept the result, re-run the workflow with different parameters, or manually intervene.

---

## Plan Quality Metrics

To help diagnose planning failures, xaft tracks several metrics that are logged at workflow completion:

| Metric | Description |
|--------|-------------|
| `plan_parse_stage` | Which parsing stage succeeded (1=JSON, 2=numbered list, 3=fallback) |
| `classification_confidence` | Whether the classification was explicit (JSON/INFORMATIONAL) or heuristic |
| `steps_count` | Number of steps in the final plan |
| `files_count` | Number of files in `files_to_modify` |
| `fix_cycles` | Number of QA→Fixer→QA iterations |
| `total_handoffs` | Total handoff count at termination |

These metrics are invaluable for iterative improvement. If you see `plan_parse_stage: 3` frequently, your planner's prompt needs better formatting instructions. If `fix_cycles` is consistently > 2, the planner's plans may be too vague or the Coder's implementation quality needs attention.

---

## Best Practices for Reliable Parsing

### For Prompt Engineers

1. **Request JSON output explicitly**: Include "respond in JSON format" in the planner's system prompt. LLMs are significantly more likely to produce parseable JSON when explicitly asked.

2. **Provide a JSON template**: Show the expected structure with example values. This gives the LLM a concrete format to emulate.

3. **Use the INFORMATIONAL prefix**: For tasks that don't require code changes, instruct the planner to start its response with "INFORMATIONAL:". This is the most reliable classification signal.

4. **Avoid ambiguous task descriptions**: Tasks like "make it better" or "fix the issue" produce ambiguous plans. Encourage users to provide specific, well-scoped tasks.

### For Workflow Designers

1. **Set `max_handoffs` based on task complexity**: Simple tasks need 4–6 handoffs; complex tasks may need 10–14. Setting the limit too low causes premature termination; too high wastes resources on stuck workflows.

2. **Monitor `plan_parse_stage` metrics**: If Stage 3 (fallback) is common, invest in prompt engineering before adding more complex parsing logic.

3. **Design QA prompts for specificity**: Vague QA feedback produces vague fix requests, which produce vague fixes, which produce more QA issues. Break the cycle by requiring the QA agent to cite specific lines, state expected behavior, and provide examples.
