# Installation

## Requirements

- **Rust 1.86 or newer** (the workspace is edition 2024, `rust-version = "1.86"`)
- An LLM provider API key: Anthropic (default), OpenAI, Ollama, or LiteLLM

Check your toolchain:

```bash
rustc --version   # must be >= 1.86
cargo --version
```

## Build from source

```bash
git clone https://github.com/jymchng/xaft
cd xaft
cargo build --workspace
```

The `xaft` binary lands in `target/debug/xaft`. For a release build:

```bash
cargo build --release --workspace
# binary at target/release/xaft
```

You can add it to your PATH, e.g.:

```bash
# add to ~/.bashrc or ~/.zshrc
export PATH="$PATH:$PWD/target/release"
```

## Install via cargo (once published)

```bash
cargo install xaft
```

## Shell completions

xaft can generate completions for bash, zsh, and fish:

```bash
xaft completions bash >> ~/.bashrc
xaft completions zsh > ~/.zfunc/_xaft
xaft completions fish > ~/.config/fish/completions/xaft.fish
```

(Verified: the `completions` subcommand accepts `bash`, `zsh`, `fish` — see
`ShellArg` in `crates/xaft-cli/src/args.rs`.)

## Verify the install

```bash
xaft version
```

## Next

[Configure a provider →](02-configuration.md)
