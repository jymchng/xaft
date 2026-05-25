# xaft — Justfile
# Run `just` to see available recipes.
# Install just: cargo install just

set dotenv-load := true

export RUSTFLAGS := "-Awarnings"

# Target directory — override to avoid root-owned target/
export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", "/root/root-local")

# Default: show all recipes
default:
    @just --list

# ── Build ─────────────────────────────────────────────────────────────

# Build the workspace (debug)
build:
    cargo build --workspace

# Build the xaft binary in release mode
build-release:
    cargo build --workspace --release

# Check compilation without artifacts (faster than build)
check:
    cargo check --workspace

# ── Format & Lint ─────────────────────────────────────────────────────

# Format all source files
fmt:
    cargo fmt --all

# Check formatting without modifying (CI-safe)
fmt-check:
    cargo fmt --all -- --check

# Run clippy on the whole workspace
lint:
    cargo clippy --workspace -- -D warnings

# ── Test ──────────────────────────────────────────────────────────────

# Run all tests
test:
    cargo test --workspace

# Run lib (unit) tests only
test-unit:
    cargo test --workspace --lib

# Run integration tests only
test-integration:
    cargo test --workspace --test '*'

# Run a single test by substring match — e.g. just test-one dry_run
test-one name:
    cargo test --workspace {{ name }}

# Run tests with stdout visible
test-verbose:
    cargo test --workspace -- --nocapture

# Run tests for a single crate — e.g. just test-crate xaft-runtime
test-crate crate:
    cargo test -p {{ crate }}

# ── Full CI gate ──────────────────────────────────────────────────────

# Format check + lint + test (mirrors CI)
ci: fmt-check lint test

# ── Coverage ──────────────────────────────────────────────────────────

# Generate coverage report with tarpaulin
coverage:
    cargo tarpaulin \
        --workspace \
        --engine llvm \
        --timeout 300 \
        --skip-clean

# ── Documentation ─────────────────────────────────────────────────────

# Build and open rustdoc
doc:
    cargo doc --workspace --no-deps --open

# Build docs without opening (CI)
doc-build:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps

# ── Run ───────────────────────────────────────────────────────────────

# Run xaft with a task — e.g. just run "fix the type error in src/"
run task:
    cargo run --bin xaft -- run "{{ task }}"

# Dry-run a task (no file changes)
dry-run task:
    cargo run --bin xaft -- run --dry-run "{{ task }}"

# Show xaft version
version:
    cargo run --bin xaft -- version

# ── Examples ──────────────────────────────────────────────────────────

# Run xaft on the bundled python-password-generator example
example-fix:
    cd xaft-examples/python-password-generator && \
        cargo run --bin xaft --manifest-path ../../Cargo.toml -- \
        run "Replace all uses of random.choice with secrets.choice for cryptographic security"

# ── Version Management ────────────────────────────────────────────────

# Show current workspace version
version-show:
    #!/usr/bin/env bash
    python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['package']['version'])"

# Bump version: just bump patch | minor | major
bump part:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 - "{{ part }}" <<'PY'
    import pathlib, re, subprocess, sys, tomllib

    cargo = pathlib.Path("Cargo.toml")
    data = tomllib.loads(cargo.read_text())
    cur = data["workspace"]["package"]["version"]
    part = sys.argv[1]
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", cur)
    if not m:
        sys.exit(f"unsupported version format: {cur}")
    maj, minor, patch = map(int, m.groups())
    match part:
        case "major": new = f"{maj+1}.0.0"
        case "minor": new = f"{maj}.{minor+1}.0"
        case "patch": new = f"{maj}.{minor}.{patch+1}"
        case _: sys.exit("Usage: just bump [major|minor|patch]")

    text = cargo.read_text()
    text = re.sub(r'(?<=\[workspace\.package\]\n)(?:.*\n)*?version\s*=\s*"[^"]*"',
                  lambda m: m.group().replace(cur, new), text, count=1)
    # simpler: replace first occurrence in [workspace.package]
    text = text.replace(f'version = "{cur}"', f'version = "{new}"', 1)
    cargo.write_text(text)
    print(f"Bumped {cur} → {new}")
    PY

# ── Workspace Utilities ───────────────────────────────────────────────

# Show dependency tree
tree:
    cargo tree -p xaft

# Clean build artifacts
clean:
    cargo clean

# Rebuild from scratch
rebuild: clean build

# Check required tools are installed
doctor:
    #!/usr/bin/env bash
    ok=0
    check() { command -v "$1" >/dev/null 2>&1 && echo "  ✓ $1" || { echo "  ✗ $1  ($2)"; ok=1; }; }
    echo "Checking required tools..."
    check cargo   "install rustup"
    check rustfmt "rustup component add rustfmt"
    check just    "cargo install just"
    check python3 "install python 3.11+"
    [[ $ok -eq 0 ]] && echo "All tools present." || echo "Some tools missing — see above."
    exit $ok

# Show outdated dependencies
outdated:
    cargo outdated --workspace

# Audit for security vulnerabilities
audit:
    cargo audit

# Watch for changes and re-run tests (requires cargo-watch)
watch:
    cargo watch -x "test --workspace"
