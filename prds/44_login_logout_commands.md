# PRD 44 — `/login` + `/logout` Commands: Provider Authentication

## Status: Draft
## Depends on: PRD 32a–32e, PRD 22 (costed provider)

---

## 1. Problem

Both commands return `"not yet connected to runtime."`. Credentials today
are supplied only via environment variables or config files. There is no
interactive flow to acquire, validate, or revoke credentials from within
the TUI.

---

## 2. Goals

### `/login [provider]`

1. With no arg or `anthropic`: guide the user through API key entry.
   - Print `"Enter your Anthropic API key (sk-ant-…): "` inline.
   - Mask input (show `●●●●●●●●` while typing, not the key characters).
   - On submit, validate by making a lightweight API call (`/models`
     endpoint, no tokens consumed).
   - On success, persist to `~/.config/xaft/credentials.toml` (keyring or
     plaintext with `0600` permissions).
   - Print `"✓ Logged in to Anthropic. Key stored."`.
2. With `openai` / `gemini`: same flow adapted to the provider's key format.

### `/logout [provider]`

1. Remove stored credentials for the specified provider (or all providers
   if no arg).
2. Requires confirmation: `"Remove API key for anthropic? [y/N]"`.
3. Does NOT clear environment variables — only the persisted credential
   file.
4. Print `"Credentials removed. Set ANTHROPIC_API_KEY to continue."`.

---

## 3. Non-Goals

- OAuth flows (API-key-only in v1).
- Syncing credentials to a secrets manager.
- Multi-account support (one key per provider).

---

## 4. Technical Design

### 4.1 Masked input widget

Extend `InputBar` with a `masked: bool` field. When `true`, `draw_prompt_block`
renders each character as `●` instead of the actual glyph. The buffer still
stores the real characters.

`AppState` sets `self.input_bar.masked = true` when an
`XaftMaskedInputRequested` signal is received, and `false` after submission.

### 4.2 `CredentialStore`

New file `crates/xaft-config/src/credentials.rs`:

```rust
pub struct CredentialStore {
    path: PathBuf,   // ~/.config/xaft/credentials.toml
}

impl CredentialStore {
    pub fn set(&self, provider: &str, key: &str) -> Result<(), ConfigError>;
    pub fn get(&self, provider: &str) -> Result<Option<String>, ConfigError>;
    pub fn remove(&self, provider: &str) -> Result<bool, ConfigError>;
    pub fn list_providers(&self) -> Result<Vec<String>, ConfigError>;
}
```

File format:
```toml
[anthropic]
api_key = "sk-ant-…"

[openai]
api_key = "sk-…"
```

Permissions enforced via `std::fs::set_permissions(path, 0o600)` after
every write.

### 4.3 Key validation

Per-provider validator:

```rust
trait ProviderKeyValidator {
    async fn validate(&self, key: &str) -> Result<String, String>;
    // Ok(model_name) on success, Err(message) on failure
}
```

`AnthropicKeyValidator` calls `GET /v1/models` with the supplied key;
accepts 200, rejects 401/403.

### 4.4 `LoginHandler` flow

```
is_async = true

1. Parse provider (default "anthropic").
2. Print "Enter your <Provider> API key:".
3. Emit XaftMaskedInputRequested.
4. AppState captures the next submission as the key (not a slash command).
5. Re-route to LoginHandler::continue_with_key(key).
6. Validate key → success or error.
7. CredentialStore::set(provider, key).
8. Print success/failure.
```

Because this needs a multi-step interactive flow, `LoginHandler` uses a
state machine: `Idle → WaitingForKey → Validating → Done`.

The handler stores its state in an `Arc<Mutex<LoginState>>`. `AppState`
checks whether any slash handler is in a `WaitingForKey` state before
routing Enter to the normal submission path.

### 4.5 `LogoutHandler` flow

1. Parse provider.
2. Show confirmation prompt (same pattern as `CommitHandler`).
3. On confirm: `CredentialStore::remove(provider)`.
4. Print result.

---

## 5. Implementation Steps

1. Implement `CredentialStore` in `xaft-config/src/credentials.rs`.
2. Implement `ProviderKeyValidator` and `AnthropicKeyValidator`.
3. Add `masked` field to `InputBar`; render `●` when masked.
4. Define `XaftMaskedInputRequested` signal.
5. Handle masked-input state in `AppState` (intercept next submit).
6. Implement `LoginHandler` state machine.
7. Implement `LogoutHandler`.
8. Wire `CredentialStore` into `build_default_registry`.
9. Tests (see §6).

---

## 6. Tests

```
test credential_store_set_and_get
test credential_store_remove_unknown_provider_returns_false
test credential_store_file_permissions_are_0600
test login_handler_invalid_key_shows_error
test login_handler_valid_key_stored
test login_handler_masked_input_hides_characters
test logout_handler_requires_confirmation
test logout_handler_removes_stored_key
test logout_handler_no_session_no_env_prints_note
```
