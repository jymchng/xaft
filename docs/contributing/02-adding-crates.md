# Adding a New Crate

This guide covers the process of adding a new crate to the xaft workspace. It describes the Cargo.toml conventions, dependency management rules, the directory structure expectations, and the integration points that every crate must satisfy to participate in the xaft runtime. Adding a crate is a significant architectural decision — it creates a new dependency boundary that will persist for the life of the project — so it should be undertaken only when the functionality cannot reasonably live in an existing crate.

---

## When to Add a Crate

Before adding a new crate, consider whether the functionality fits in an existing one. The criteria for a new crate are:

1. **Distinct responsibility**: The new functionality has a single, well-defined responsibility that does not overlap with any existing crate. A "monitoring" crate that does both metrics collection and alerting is two responsibilities; it should either be split or placed in an existing crate that handles one of them.

2. **Independent dependency set**: The new functionality requires dependencies that are not shared by any existing crate. If a new tool crate depends on `aws-sdk-s3` but no other crate uses AWS SDKs, that is a good reason for a separate crate — it prevents the AWS dependency from bloating the build for users who do not need S3 tools.

3. **Clear boundary**: The new functionality has a clear API boundary with minimal cross-cutting concerns. If adding the functionality requires adding many `pub` methods or creating many adapter types, it may be better placed in an existing crate where the integration is simpler.

If the functionality passes these criteria, proceed with the crate creation process. If not, consider adding it as a module within an existing crate.

---

## Cargo.toml Conventions

Every crate in the xaft workspace follows a consistent Cargo.toml structure. Here is the template for a new crate:

```toml
[package]
name = "xaft-<name>"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "<one-line description of the crate's responsibility>"

[dependencies]
# Framework crates (agtrs-*)
agtrs-runtime = { path = "../agtrs-runtime" }

# Xaft crates — only lower-layer dependencies
# xaft-config = { path = "../xaft-config" }

# Third-party crates
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
async-trait = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
xaft-runtime = { path = "../xaft-runtime", features = ["test-util"] }

[features]
default = []
test-util = []  # Exposes test-only types and constructors
```

### Workspace Inheritance

Version, edition, rust-version, and license are inherited from the workspace root via `workspace = true`. This ensures that all crates use the same version number and Rust edition, which prevents version skew and simplifies release management. When the workspace version is bumped for a release, every crate is bumped simultaneously.

### Dependency Workspace References

Third-party dependencies that are used by multiple crates should be declared in the workspace root's `[workspace.dependencies]` section and referenced with `workspace = true` in each crate. This ensures that all crates use the same version of shared dependencies, avoiding duplicate versions and potential type mismatches. Dependencies used by only one crate can be declared locally without workspace references.

```toml
# Workspace root Cargo.toml
[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.35", features = ["full"] }
tracing = "0.1"
thiserror = "1.0"
async-trait = "0.1"
```

### Feature Flags

Every crate should have a `test-util` feature that exposes test-only types and constructors. This feature is not enabled by default and is only activated in `dev-dependencies`. The `test-util` feature should never be enabled in a production build — it exposes types that bypass safety checks (like `MockProvider`) and constructors that skip validation (like `for_testing()`).

If the crate has optional functionality that can be toggled (for example, a "tui" feature that enables terminal rendering, or an "mcp" feature that enables Model Context Protocol support), define additional feature flags. Each feature should be additive — enabling a feature adds functionality but never removes it. This is a Cargo convention that ensures feature unification works correctly when multiple crates depend on the same crate with different feature sets.

---

## Directory Structure

Each crate follows a consistent internal structure:

```
xaft-<name>/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Public API re-exports, crate-level documentation
│   ├── error.rs        # Thiserror error enum for the crate
│   ├── <module>.rs     # One file per major module
│   └── <module>/
│       ├── mod.rs      # Module definition and re-exports
│       └── <sub>.rs    # Sub-module implementations
└── tests/
    ├── <feature>_test.rs  # Integration test per major feature
    └── common/
        └── mod.rs          # Shared test utilities
```

The `lib.rs` file serves as the crate's public API surface. It re-exports all public types from the internal modules, so consumers can write `use xaft_name::TypeName` instead of `use xaft_name::module::TypeName`. This flat re-export style makes the API easier to discover and reduces the impact of internal reorganizations.

The `error.rs` file defines the crate's error enum using `thiserror`. Every error that the crate can produce should be represented as a variant of this enum. See [Error Handling](../internals/02-error-handling.md) for the conventions on error design.

### Crate-Level Documentation

The `lib.rs` file must include a crate-level doc comment that explains the crate's responsibility, its position in the architecture, and its key types:

```rust
//! # xaft-<name>
//!
//! <One-paragraph description of what the crate does and why it exists.>
//!
//! ## Architecture
//!
//! <Where this crate sits in the dependency hierarchy. What it depends on
//! and what depends on it.>
//!
//! ## Key Types
//!
//! - [`TypeName`] — <One-line description of the type's purpose.>
//! - [`TraitName`] — <One-line description of the trait's contract.>
```

This documentation is rendered by `cargo doc` and is the first thing developers see when exploring the crate. It should provide enough context to understand the crate's role without reading the source code.

---

## Dependency Management

### Dependency Direction

Dependencies must flow downward through the crate layers. The dependency graph must be a directed acyclic graph (DAG) with no cycles. The layers are:

1. **Application layer**: `xaft` (binary only)
2. **Feature layer**: `xaft-cli`, `xaft-runtime`, `xaft-agent`, `xaft-tools`, `xaft-tui`, `xaft-session`, `xaft-config`
3. **Framework layer**: `agtrs-runtime`, `agtrs-anthropic`, `agtrs-openai`, `agtrs-git`, `agtrs-shell`, `agtrs-workspace`, `agtrs-store`

A feature crate can depend on framework crates and on other feature crates only through `xaft-runtime`. The runtime crate is the integration point where all feature crates meet; it is the only feature crate that depends on multiple other feature crates. This hub-and-spoke topology keeps the dependency graph simple and prevents circular dependencies.

### Adding a Dependency on an Existing Crate

When your new crate depends on an existing crate, add it to `[dependencies]` in your crate's `Cargo.toml`:

```toml
[dependencies]
xaft-config = { path = "../xaft-config" }
```

Then ensure that the dependency does not create a cycle. If `xaft-config` already depends on your crate (directly or transitively), you cannot depend on `xaft-config`. In this case, you must either extract the shared types into a lower-layer crate or use trait-based dependency inversion.

### Using Trait-Based Dependency Inversion

If your new crate needs functionality from a higher-layer crate, define a trait in your crate and implement it in the higher-layer crate. This is the standard technique for avoiding upward dependencies:

```rust
// In the lower-layer crate (xaft-tools):
pub trait WorkspaceAccess: Send + Sync {
    fn read_file(&self, path: &str) -> Result<String, ToolError>;
    fn write_file(&self, path: &str, content: &str) -> Result<(), ToolError>;
}

// In the higher-layer crate (xaft-runtime):
impl WorkspaceAccess for FsWorkspaceStore {
    fn read_file(&self, path: &str) -> Result<String, ToolError> {
        WorkspaceStore::read_file(self, path).map_err(|e| ToolError::Internal(e.to_string()))
    }
    // ...
}
```

This pattern allows the lower crate to use the functionality without depending on the higher crate. The runtime crate provides the concrete implementation at construction time, typically through the builder pattern.

---

## Integration with the Runtime

Every new crate that provides functionality used during agent execution must integrate with the runtime. This integration happens through three mechanisms:

### 1. Builder Registration

If the crate provides components that the runtime needs to construct (like tools or providers), add registration methods to the relevant builder:

```rust
// In xaft-tools/src/registry.rs
impl ToolRegistryBuilder {
    pub fn register_my_new_tools(mut self) -> Self {
        self.tools.insert("my_tool".to_string(), Arc::new(MyTool::new()));
        self
    }
}
```

The builder pattern ensures that the new components are registered at construction time and are available for the entire session. Components cannot be added or removed mid-session.

### 2. Signal Bus Subscription

If the crate needs to react to runtime events (like `ModelCallComplete` for cost tracking or `FileEdited` for auto-formatting), subscribe to the appropriate signal type during bootstrap:

```rust
// In xaft-runtime/src/bootstrap.rs
signal_bus.subscribe::<ModelCallComplete>("cost_tracker", |event| {
    cost_tracker.record(event.model, event.tokens, event.cost);
});
```

Signal bus subscriptions are typed, so the new crate only receives the events it cares about. The subscription is set up during bootstrap and runs for the lifetime of the runtime.

### 3. Configuration Integration

If the crate has configurable behavior, add a configuration section to `XaftConfig` and the corresponding TOML parsing:

```rust
// In xaft-config/src/config.rs
#[derive(Debug, Deserialize)]
pub struct XaftConfig {
    pub general: GeneralConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub my_new_feature: MyNewFeatureConfig,  // New section
}
```

```toml
# In the user's xaft.toml
[my_new_feature]
enabled = true
option_a = "value"
```

The configuration is loaded during bootstrap and passed to the crate's initialization functions. Use `#[serde(default)]` on new configuration sections so that existing configuration files without the new section continue to work.

---

## Registering the Crate in the Workspace

After creating the crate directory and Cargo.toml, register it in the workspace root's `Cargo.toml`:

```toml
[workspace]
members = [
    "xaft",
    "xaft-cli",
    "xaft-config",
    "xaft-runtime",
    "xaft-agent",
    "xaft-tools",
    "xaft-tui",
    "xaft-session",
    "xaft-<name>",  # Add your new crate
    # Framework crates
    "agtrs-runtime",
    "agtrs-anthropic",
    # ...
]
```

Also add the crate to the `Cargo.lock` by running `cargo build` or `cargo check` from the workspace root. This ensures that the workspace metadata is consistent and that CI can build the crate.

---

## CI Configuration

If the crate has special CI requirements (for example, it needs a database service or a specific Rust toolchain), add a CI configuration entry. The default CI pipeline runs `cargo test --workspace` for all crates, which is sufficient for most cases. If your crate requires additional setup, add it to the CI workflow file:

```yaml
# .github/workflows/ci.yml
- name: Test xaft-<name>
  run: cargo test -p xaft-<name> --features test-util
```

Keep the CI configuration minimal. Crates that require external services for testing should use the `test-util` feature to provide in-memory alternatives, so that the default test suite can run without any external dependencies.
