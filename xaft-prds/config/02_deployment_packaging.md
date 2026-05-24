# Deployment & Packaging

## Release Targets

| Platform | Format | Distribution |
|---|---|---|
| Linux x86_64 | Static binary (musl) | GitHub Releases, cargo-binstall |
| Linux ARM64 | Static binary (musl) | GitHub Releases |
| macOS ARM64 (Apple Silicon) | Signed binary | GitHub Releases, Homebrew |
| macOS x86_64 | Signed binary | GitHub Releases, Homebrew |
| Windows x86_64 | .exe + installer | GitHub Releases, winget |

## Build Configuration

```toml
# Cargo.toml [profile.release]
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"      # smaller binary, no unwinding overhead

# Target maximum performance on all cores
[target.x86_64-unknown-linux-musl]
rustflags = ["-C", "target-cpu=x86-64-v3"]
```

## Static Binary (Linux)

```bash
# Build static binary using musl (zero system library deps)
cargo build --release --target x86_64-unknown-linux-musl
# Output: ~12MB stripped static binary

# Verify no dynamic deps
ldd target/x86_64-unknown-linux-musl/release/xaft
# → statically linked
```

## Homebrew Formula

```ruby
class Xaft < Formula
  desc "Autonomous coding CLI built on the agtrs framework"
  homepage "https://github.com/yourorg/xaft"
  version "0.1.0"

  on_macos do
    on_arm { url "https://github.com/yourorg/xaft/releases/download/v#{version}/xaft-aarch64-apple-darwin.tar.gz" }
    on_intel { url "https://github.com/yourorg/xaft/releases/download/v#{version}/xaft-x86_64-apple-darwin.tar.gz" }
  end

  def install
    bin.install "xaft"
    generate_completions_from_executable(bin/"xaft", "completions")
  end
end
```

## Shell Completions

```bash
# Generate shell completions
xaft completions bash   >> ~/.bash_completion
xaft completions zsh    >> ~/.zshrc
xaft completions fish   > ~/.config/fish/completions/xaft.fish
xaft completions nushell > ~/.config/nushell/completions/xaft.nu
```

## Docker Image

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y git ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/xaft /usr/local/bin/xaft
WORKDIR /workspace
VOLUME ["/workspace"]
ENTRYPOINT ["xaft"]
```

## CI/CD Release Pipeline

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags: ["v*"]
jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest,  target: x86_64-unknown-linux-musl
          - os: ubuntu-latest,  target: aarch64-unknown-linux-musl
          - os: macos-latest,   target: aarch64-apple-darwin
          - os: macos-latest,   target: x86_64-apple-darwin
          - os: windows-latest, target: x86_64-pc-windows-msvc
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: "${{ matrix.target }}" }
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: softprops/action-gh-release@v2
        with: { files: "target/${{ matrix.target }}/release/xaft*" }
```

## Installation

```bash
# cargo-binstall (fastest)
cargo binstall xaft

# From source
cargo install xaft

# Script installer
curl -fsSL https://get.xaft.dev | sh

# Homebrew
brew install xaft