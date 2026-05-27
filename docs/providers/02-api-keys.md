# API Key Resolution

API keys are the primary authentication mechanism for LLM API providers. The xaft configuration system supports a five-tier lookup chain for resolving API keys, providing flexibility in how keys are stored and managed while maintaining security best practices. This document describes the resolution chain, the precedence rules, and the security considerations for each tier.

## Five-Tier Lookup Chain

When a provider needs an API key to authenticate an LLM API call, it follows a five-tier resolution chain. The chain is evaluated from the most specific (tier 1) to the most general (tier 5), and the first tier that produces a non-empty key value wins. This design allows keys to be overridden at any level while providing reasonable fallback behavior.

```mermaid
flowchart TD
    A[API Key Request] --> B1[Tier 1: ProviderConfig.api_key<br/>Direct value in config]
    B1 -->|Empty| B2[Tier 2: ProviderConfig.api_key_env<br/>Named environment variable]
    B2 -->|Empty| B3[Tier 3: XAFT_{PROVIDER}_API_KEY<br/>Convention-based env var]
    B3 -->|Empty| B4[Tier 4: Global credential store<br/>OS keychain / credential file]
    B4 -->|Empty| B5[Tier 5: Provider-specific convention<br/>e.g., ANTHROPIC_API_KEY]
    B5 -->|Empty| C[Error: No API key found]

    B1 -->|Found| D[Use key]
    B2 -->|Found| D
    B3 -->|Found| D
    B4 -->|Found| D
    B5 -->|Found| D

    style B1 fill:#2b6cb0,color:#fff
    style B2 fill:#2c7a7b,color:#fff
    style B3 fill:#38a169,color:#fff
    style B4 fill:#d69e2e,color:#fff
    style B5 fill:#dd6b20,color:#fff
    style C fill:#e53e3e,color:#fff
```

### Tier 1: Direct Configuration Value

The first tier checks the `api_key` field in `ProviderConfig`. If this field is set to a non-empty string, its value is used directly as the API key. This is the most explicit and intentional key specification — the user has deliberately placed a key value in the configuration.

However, storing API keys directly in configuration files is a security risk, especially if the configuration file is checked into version control. The `api_key` field should be used only in local, non-committed configuration files (e.g., `~/.config/xaft/config.toml`) or in conjunction with environment variable interpolation (`api_key = "${MY_API_KEY}"`) to avoid storing the actual key value in the file.

**Security Note**: When `api_key` is used without interpolation, the key value is stored in plain text in the configuration file. If the file is committed to version control, the key is exposed. Always prefer interpolation (`${...}`) or environment variable references (`api_key_env`) over direct values in project-level configuration files.

### Tier 2: Named Environment Variable

The second tier checks the `api_key_env` field in `ProviderConfig`. This field specifies the name of an environment variable that contains the API key. For example, `api_key_env = "MY_CUSTOM_ANTHROPIC_KEY"` causes the resolver to read the `MY_CUSTOM_ANTHROPIC_KEY` environment variable.

This tier is the recommended way to configure API keys for most use cases. It keeps the key out of configuration files entirely (the config file only contains the variable name, not the key value), and it integrates naturally with secret management tools that inject environment variables (e.g., Docker secrets, Kubernetes secrets, HashiCorp Vault, AWS Secrets Manager).

```toml
[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
```

### Tier 3: Convention-Based Environment Variable

The third tier checks a convention-based environment variable name derived from the provider name. The convention is `XAFT_{PROVIDER_NAME}_API_KEY`, where `PROVIDER_NAME` is the provider's key in the configuration HashMap, uppercased with hyphens replaced by underscores. For example, for a provider named `anthropic`, the resolver checks `XAFT_ANTHROPIC_API_KEY`; for `openai`, it checks `XAFT_OPENAI_API_KEY`.

This tier provides a zero-configuration fallback — users can set a single environment variable for each provider without modifying any configuration files. It is particularly useful for CI/CD environments where configuration files are not available and environment variables are the standard mechanism for injecting secrets.

### Tier 4: Global Credential Store

The fourth tier checks the operating system's credential store for a stored API key. The implementation varies by platform:

- **macOS**: Uses the Keychain via the `security` command-line tool. Keys are stored under the service name `xaft` and the account name `{provider_name}`.
- **Linux**: Uses the Secret Service API (via `libsecret`) if available, or falls back to a GPG-encrypted file at `~/.config/xaft/credentials.gpg`.
- **Windows**: Uses the Windows Credential Manager via the `credential-manager` crate.

The credential store is accessed through a `CredentialStore` trait that abstracts platform differences. Keys can be stored in the credential store using the `xaft auth login` command, which prompts for the key and stores it securely. This tier is the most secure option for local development, as the key is protected by the operating system's encryption and access controls.

### Tier 5: Provider-Specific Convention

The fifth tier checks the standard environment variable convention used by the provider's own tools and SDKs. This is a compatibility layer that allows xaft to pick up keys that the user has already configured for other tools:

| Provider | Environment Variable |
|----------|---------------------|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| OpenAI Compatible | Varies; typically `{PROVIDER}_API_KEY` |

This tier ensures that users who have already set up API keys for the provider's CLI tools or SDKs can use xaft without any additional configuration. It is the lowest-priority tier because the provider-specific variables may not always be appropriate for xaft (e.g., the user might want to use a different key for xaft than for the provider's own tools).

## Resolution Example

Consider the following configuration:

```toml
[provider.anthropic]
api_key = ""                    # Tier 1: empty, skip
api_key_env = "MY_ANTHROPIC_KEY" # Tier 2: check this env var

[provider.openai]
# No api_key or api_key_env specified
```

And the following environment variables:

```
MY_ANTHROPIC_KEY=sk-ant-abc123
XAFT_OPENAI_API_KEY=sk-openai-xyz789
OPENAI_API_KEY=sk-openai-old
```

The resolution proceeds as follows:

**Anthropic provider:**
1. Tier 1 (`api_key`): Empty string → skip
2. Tier 2 (`api_key_env = "MY_ANTHROPIC_KEY"`): Found `sk-ant-abc123` → **use this key**

**OpenAI provider:**
1. Tier 1 (`api_key`): Not specified → skip
2. Tier 2 (`api_key_env`): Not specified → skip
3. Tier 3 (`XAFT_OPENAI_API_KEY`): Found `sk-openai-xyz789` → **use this key**

Note that the `OPENAI_API_KEY` variable (Tier 5) is not checked because Tier 3 found a key first. If `XAFT_OPENAI_API_KEY` were not set, Tier 4 (credential store) would be checked, and if that also failed, Tier 5 (`OPENAI_API_KEY`) would provide the fallback key `sk-openai-old`.

## Error Handling

If all five tiers fail to produce a non-empty API key, the resolver returns an `ApiKeyNotFound` error with a detailed message that includes:

- The provider name
- The tiers that were checked
- The specific environment variable names that were looked up
- Instructions for configuring the key (e.g., "Set the ANTHROPIC_API_KEY environment variable or add api_key to your configuration")

This detailed error message helps users quickly identify why the key was not found and how to fix the problem. The error is surfaced through the TUI as a `RuntimeError` event and through the CLI as a startup error.

## Key Validation

After resolving an API key, the provider performs a basic format validation before using it. The validation rules are provider-specific:

- **Anthropic**: Keys must start with `sk-ant-` and be at least 80 characters long.
- **OpenAI**: Keys must start with `sk-` and be at least 40 characters long.
- **OpenAI Compatible**: No format validation (different providers use different key formats).

If the resolved key fails format validation, a warning is logged but the key is still used. The warning helps users catch typos and misconfiguration early, but it is not a hard error because the format rules may change or the provider may accept keys with non-standard formats. The definitive validation occurs when the key is used in an actual API call — if the key is invalid, the API will return a 401 Unauthorized error, which the retry layer treats as a non-retryable error.

## Key Rotation

API keys can be rotated without restarting the session. When the `ConfigWatcher` detects a change to a configuration file containing an `api_key` or `api_key_env` field, it publishes the updated configuration through the `watch` channel. The provider checks the updated configuration on the next API call and uses the new key. Environment variables are re-read at call time (not cached), so changing an environment variable value also takes effect on the next call.

For the credential store (Tier 4), the provider reads the key from the store on every call. This is a fast operation (typically under 1ms) because the credential store is accessed via an in-process API, not a network call. If the user updates the key in the credential store (e.g., via `xaft auth login`), the new key is picked up immediately without any configuration change or process restart.
