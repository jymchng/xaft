# PRD 41 — `/init` Command: Generate AGENTS.md Project Memory File

## Status: Draft
## Depends on: PRD 32a–32e, PRD 24 (memory store), PRD 25 (xaft memory)

---

## 1. Problem

`/init` returns `"/init: not yet connected to runtime."`. The command is
supposed to generate an `AGENTS.md` file in the project root — a
machine-readable manifest of conventions, tooling, and project context that
agents read on each session start (analogous to `CLAUDE.md` in this repo).

Without `/init`, users must write `AGENTS.md` by hand or not at all.

---

## 2. Goals

1. `/init` introspects the current working directory and generates an
   `AGENTS.md` file with useful sections pre-filled.
2. If `AGENTS.md` already exists: prompt for overwrite confirmation; on
   deny, offer `/init --append` or exit.
3. The generated file includes:
   - Project name (from `Cargo.toml`, `package.json`, `pyproject.toml`, or
     directory name as fallback).
   - Detected language / build system with build + test commands.
   - Top-level directory tree (depth 2, excluding `.git`, `target`,
     `node_modules`, `__pycache__`).
   - Empty placeholder sections: `## Goals`, `## Conventions`,
     `## Off-limits paths`.
4. The user can edit the file after generation; xaft reads it at session
   start via the memory store integration (PRD 24/25).

### Example output (printed to transcript)

```
Generated AGENTS.md (312 bytes)
  Detected: Rust / Cargo workspace
  Build:    cargo build --release
  Test:     cargo test --workspace
  Edit /AGENTS.md to add project-specific conventions.
```

---

## 3. Non-Goals

- LLM-generated content in `AGENTS.md` (pure heuristic detection only).
- Watching `AGENTS.md` for changes at runtime.
- Supporting multiple AGENTS files per subdirectory (single root file only).

---

## 4. Technical Design

### 4.1 Project detector

A small `ProjectDetector` struct in `init.rs`:

```rust
struct ProjectInfo {
    name: String,
    language: String,
    build_cmd: String,
    test_cmd: String,
    dir_tree: String,   // formatted depth-2 tree
}
```

Detection priority (first match wins):
1. `Cargo.toml` → Rust/Cargo
2. `package.json` → Node.js/npm (or yarn/pnpm by lockfile)
3. `pyproject.toml` / `setup.py` → Python
4. `go.mod` → Go
5. `pom.xml` / `build.gradle` → Java/Maven or Gradle
6. Fallback: `Unknown`

### 4.2 File generation

```rust
fn render_agents_md(info: &ProjectInfo) -> String {
    format!(
        "# AGENTS.md — {name}\n\n\
         ## Build\n\n```\n{build}\n```\n\n\
         ## Test\n\n```\n{test}\n```\n\n\
         ## Structure\n\n```\n{tree}\n```\n\n\
         ## Goals\n\n<!-- Describe what this project does -->\n\n\
         ## Conventions\n\n<!-- Coding conventions, style guides -->\n\n\
         ## Off-limits\n\n<!-- Paths the agent must never modify -->\n",
        name = info.name, build = info.build_cmd,
        test = info.test_cmd, tree = info.dir_tree,
    )
}
```

### 4.3 Overwrite check + confirmation

If `AGENTS.md` exists, the handler emits `XaftConfirmationRequested`
(or uses the same mechanism as `CommitHandler`). On approval, overwrites.
On denial, prints `"Aborted. Run /init --append to add to the existing file."`.

`--append` mode: adds only missing sections to the bottom of the file.

### 4.4 Handler structure

```rust
fn is_async(&self) -> bool { true }
fn requires_confirmation(&self) -> bool {
    // checked at runtime: true if AGENTS.md already exists
    false  // base value; real check is in execute_async
}
```

---

## 5. Implementation Steps

1. Implement `ProjectDetector` with the 6-case heuristic.
2. Implement `render_agents_md`.
3. Implement directory tree walker (depth 2, with exclusions).
4. Implement `InitHandler::execute_async` with overwrite-check flow.
5. Implement `--append` mode.
6. Tests (see §6).

---

## 6. Tests

```
test init_detects_rust_cargo_workspace
test init_detects_nodejs_npm
test init_detects_python_pyproject
test init_unknown_project_uses_directory_name
test init_generates_valid_markdown
test init_append_mode_does_not_duplicate_sections
test init_overwrite_requires_confirmation
test init_dir_tree_excludes_gitignored_dirs
```
