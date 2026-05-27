# Naming Conventions

## Purpose

Consistent naming is the foundation of a navigable codebase. The xaft project spans dozens of crates, hundreds of structs, and a live event system—without strict naming rules, developers waste time guessing whether something is called `FileReadTool`, `file_read`, or `ReadFile`. This document codifies the naming conventions that every contributor must follow so that names are predictable, searchable, and self-documenting. A good naming convention eliminates ambiguity: when you see a name, you should immediately know what kind of thing it is (crate, struct, trait, tool, signal, config key) and which domain it belongs to.

## Mental Model

Think of naming as a type system for identifiers. Just as Rust's type system prevents mixing `String` and `u32`, our naming system prevents mixing categories. The prefix or suffix tells you the category; the body tells you the domain. Crate names use kebab-case with a `xaft-` prefix so they sort together in `cargo.toml`. Struct names use PascalCase with a suffix that reveals the layer (`Tool`, `Agent`, `Provider`). Trait names use PascalCase with an `-able` suffix for capability traits or a plain noun for role traits. Tool names use snake_case because they appear as strings in LLM function-calling JSON. Signal names use `Xaft<EventName>` so they are greppable across the entire codebase and distinguishable from third-party events. Config sections use snake_case in TOML because TOML is the surface syntax and snake_case is idiomatic there.

## Extension Patterns

When adding a new domain crate, name it `xaft-<domain>` where `<domain>` is a short, lowercase, hyphen-separated noun phrase (e.g., `xaft-git-ops`, `xaft-session-store`). When adding a new tool, name the struct `<Verb><Noun>Tool` (e.g., `WriteFileTool`, `BashExecTool`) and the tool string `<verb>_<noun>` (e.g., `write_file`, `bash_exec`). When adding a new agent, name the struct `<Role>Agent` (e.g., `PlannerAgent`, `EditorAgent`). When adding a new provider, name the struct `<Vendor>Provider` (e.g., `AnthropicProvider`, `OpenAiProvider`). When adding a new signal, name it `Xaft<Verb><Noun>` (e.g., `XaftToolCallStarted`, `XaftCostUpdated`). When adding a new config section, use snake_case (e.g., `[guardrail]`, `[cost_limit_config]`).

## Common Pitfalls

- **Inconsistent crate prefixes**: A crate named `xaft_git` (underscore) instead of `xaft-git` (hyphen) breaks the `cargo` convention and looks out of place in the workspace `Cargo.toml`. Always use hyphens in crate names.
- **Missing suffixes on structs**: A struct named `FileRead` instead of `FileReadTool` makes it unclear whether this is a tool, an agent, or a plain data structure. Always include the categorical suffix.
- **CamelCase tool names**: A tool string named `readFile` instead of `read_file` violates the snake_case convention and will not match the LLM function-calling schema that xaft generates.
- **Signal names without the `Xaft` prefix**: An event named `ToolCallStarted` instead of `XaftToolCallStarted` is not greppable across the codebase and may clash with third-party event names.
- **PascalCase config keys**: A TOML key named `CostLimit` instead of `cost_limit` is not idiomatic TOML and will confuse `serde` deserialization with default settings.

## Invariants

1. Every crate in the workspace must start with `xaft-` followed by a kebab-case domain name.
2. Every tool struct must end with the suffix `Tool`. Every agent struct must end with `Agent`. Every provider struct must end with `Provider`.
3. Every tool string (the name exposed to the LLM) must be `snake_case` with no uppercase letters.
4. Every signal name must start with `Xaft` followed by PascalCase (e.g., `XaftSessionCompleted`).
5. Every config section and key in TOML must be `snake_case`.
6. Module names (Rust `mod`) must be `snake_case` and must match the file name exactly.
7. Trait names for capabilities must end in `able` (e.g., `Interruptable`, `Serializable`). Trait names for roles must be a noun (e.g., `Provider`, `Dispatch`).

## Examples

```rust
// Crate name in Cargo.toml: xaft-git-ops
// File: xaft-git-ops/src/worktree.rs

/// Tool struct: PascalCase + Tool suffix
pub struct CreateWorktreeTool {
    workspace_root: PathBuf,
}

/// Tool string exposed to LLM: snake_case
impl Tool for CreateWorktreeTool {
    fn name(&self) -> &str { "create_worktree" }
}

/// Agent struct: PascalCase + Agent suffix
pub struct PlannerAgent {
    model: String,
    tools: Vec<ToolKind>,
}

/// Provider struct: PascalCase + Provider suffix
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
}

/// Signal name: Xaft prefix + PascalCase
pub struct XaftWorktreeCreated {
    pub path: PathBuf,
    pub branch: String,
}

/// Config section: snake_case in TOML
/// [guardrail]
/// cost_limit_config = { daily = 10.0 }
```
