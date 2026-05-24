# PRD: Sandboxing & Isolation

> xaft — Autonomous Coding CLI built on agtrs
> Document: `safety/02_sandboxing.md`
> Version: 0.1.0-draft

---

## 1. Overview

xaft agents execute arbitrary tool calls that can read, write, and transform
files and run shell commands. Without sandboxing, a misaligned agent (or a
prompt-injection attack) could destroy the user's codebase, exfiltrate data,
or compromise the host system. This document specifies the multi-layer
sandboxing architecture that constrains every agent action to the smallest
privilege set required.

The sandboxing model follows the **principle of least privilege**: each agent
task starts with zero access, and capabilities are granted explicitly based on
the tool's declared requirements and the user's configuration.

---

## 2. Sandbox Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      xaft Agent Process                     │
│                                                             │
│  ┌───────────┐  ┌───────────┐  ┌──────────┐  ┌──────────┐ │
│  │ LLM Client│  │ Tool      │  │ Memory   │  │ Planner  │ │
│  │           │  │ Dispatcher│  │ Store    │  │          │ │
│  └─────┬─────┘  └─────┬─────┘  └────┬─────┘  └────┬─────┘ │
│        │              │              │              │       │
│  ══════╪══════════════╪══════════════╪══════════════╪══════ │
│        │       SANDBOX BOUNDARY       │              │       │
│  ══════╪══════════════╪══════════════╪══════════════╪══════ │
│        │              │              │              │       │
│  ┌─────▼──────────────▼──────────────▼──────────────▼─────┐ │
│  │              WorkspaceStore (path sanitizer)            │ │
│  └─────┬──────────────┬──────────────┬────────────────────┘ │
│        │              │              │                      │
│  ┌─────▼─────┐  ┌─────▼──────┐  ┌───▼──────────┐         │
│  │ Filesystem│  │ Command    │  │ Network      │         │
│  │ Sandbox   │  │ Sandbox    │  │ Sandbox      │         │
│  └─────┬─────┘  └─────┬──────┘  └───┬──────────┘         │
│        │              │              │                      │
│  ══════╪══════════════╪══════════════╪══════════ OS KERNEL │
│        ▼              ▼              ▼                      │
│    [workspace]    [subprocess]   [TCP/UDP]                  │
└─────────────────────────────────────────────────────────────┘
         │
    ═════╪═══════════════════════════════════
         │  FUTURE: Container Boundary
    ═════╪═══════════════════════════════════
         ▼
   ┌───────────┐
   │ Container │  (gVisor / Firecracker / Docker)
   │ Sandbox   │
   └───────────┘
```

---

## 3. Filesystem Sandboxing

### 3.1 WorkspaceStore Path Sanitization

The `WorkspaceStore` is the sole gateway for all filesystem operations. Every
path passed to a tool function is resolved, canonicalized, and validated against
the workspace root before any I/O occurs.

```rust
/// Centralized filesystem access with path sanitization.
pub struct WorkspaceStore {
    /// Absolute, canonical path to the workspace root.
    root: PathBuf,
    /// Set of explicitly allowed paths outside the workspace root.
    /// (e.g., global config dirs, shared cache)
    extra_allowed: Vec<PathBuf>,
    /// Paths that are always denied, even inside the workspace.
    /// (e.g., .env, credentials, .git/config for some operations)
    deny_patterns: Vec<GlobPattern>,
    /// Whether symlinks are allowed to escape the workspace root.
    allow_symlink_escape: bool,
}

impl WorkspaceStore {
    /// Sanitize and validate a user-provided path.
    ///
    /// Resolution steps:
    ///   1. Join the path to `self.root` if relative
    ///   2. Canonicalize (resolve symlinks, eliminate ..)
    ///   3. Verify the canonical path starts with `self.root`
    ///   4. Check deny_patterns
    ///   5. Return the validated absolute path
    pub fn sanitize_path(&self, raw: &str) -> Result<PathBuf, SandboxError> {
        let joined = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.root.join(raw)
        };

        // Canonicalize — this resolves symlinks and .. traversal
        let canonical = joined.canonicalize().map_err(|e| {
            SandboxError::PathResolution {
                raw: raw.to_string(),
                source: e,
            }
        })?;

        // Boundary check: must be within root or extra_allowed
        let within_root = canonical.starts_with(&self.root);
        let within_extra = self.extra_allowed.iter()
            .any(|p| canonical.starts_with(p));

        if !within_root && !within_extra {
            return Err(SandboxError::PathEscape {
                raw: raw.to_string(),
                resolved: canonical,
                root: self.root.clone(),
            });
        }

        // Symlink escape check
        if !self.allow_symlink_escape && !within_root {
            // If the path is in extra_allowed via a symlink that escapes root,
            // deny it unless explicitly configured
            let is_symlink = std::fs::symlink_metadata(&joined)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink && !canonical.starts_with(&self.root) {
                return Err(SandboxError::SymlinkEscape {
                    raw: raw.to_string(),
                    target: canonical,
                });
            }
        }

        // Deny pattern check
        let relative = canonical.strip_prefix(&self.root)
            .unwrap_or(&canonical);
        for pattern in &self.deny_patterns {
            if pattern.matches_path(relative) {
                return Err(SandboxError::DeniedPattern {
                    raw: raw.to_string(),
                    pattern: pattern.to_string(),
                });
            }
        }

        Ok(canonical)
    }

    /// Check whether a path is within the workspace boundary without I/O.
    /// Used for pre-flight validation before expensive canonicalization.
    pub fn is_within_workspace(&self, raw: &str) -> bool {
        let path = Path::new(raw);
        if path.is_absolute() {
            path.starts_with(&self.root)
        } else {
            // Naive check: no .. components
            !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("path resolution failed for '{raw}': {source}")]
    PathResolution { raw: String, source: std::io::Error },

    #[error("path escape detected: '{raw}' resolves to {resolved}, outside root {root}")]
    PathEscape { raw: String, resolved: PathBuf, root: PathBuf },

    #[error("symlink escape: '{raw}' targets {target} outside workspace")]
    SymlinkEscape { raw: String, target: PathBuf },

    #[error("denied pattern: '{raw}' matches deny rule '{pattern}'")]
    DeniedPattern { raw: String, pattern: String },
}
```

### 3.2 Path Validation Flow

```
raw path string
       │
       ▼
 ┌───────────────┐
 │ Is absolute?  │──── No ───▶ join to workspace root
 └───────┬───────┘                  │
         │ Yes                      │
         ▼                          ▼
 ┌───────────────────────────────────────┐
 │        Canonicalize (resolve links)   │
 └───────────────────┬───────────────────┘
                     │
                     ▼
 ┌───────────────────────────────┐
 │  starts_with(workspace_root)? │
 │  OR in extra_allowed?         │
 └───────┬───────────┬───────────┘
         │ Yes       │ No
         ▼           ▼
 ┌───────────────┐  ERR: PathEscape
 │ Deny patterns │
 │ match?        │
 └───┬───────┬───┘
     │ No    │ Yes
     ▼       ▼
   OK      ERR: DeniedPattern
```

### 3.3 Workspace-Relative File Tools

All file manipulation tools are implemented exclusively through `WorkspaceStore`:

```rust
#[tool(name = "write_file", description = "Write content to a file")]
async fn write_file(
    ctx: &ToolContext,
    #[desc("Path relative to workspace root")] path: String,
    #[desc("File content")] content: String,
) -> Result<ToolOutput, ToolError> {
    // sanitize_path is called automatically by the framework
    let validated = ctx.workspace.sanitize_path(&path)?;

    // Ensure parent directory exists
    if let Some(parent) = validated.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(&validated, &content).await?;
    Ok(ToolOutput::text(format!("Wrote {} bytes to {}", content.len(), path)))
}
```

### 3.4 Deny Patterns (Default)

```
# Always denied (even inside workspace)
**/.env
**/.env.*
**/id_rsa
**/id_ed25519
**/.aws/credentials
**/.ssh/config
**/*.pem
**/*.key
**/credentials.json
**/service-account*.json
```

---

## 4. Command Execution Sandboxing

### 4.1 Subprocess Isolation Model

Shell commands executed by the agent run in constrained subprocesses. The
sandboxing applies at multiple levels:

```
┌─────────────────────────────────────────────────────────────┐
│                  Command Execution Pipeline                  │
│                                                             │
│  agent ──▶ shell_exec ──▶ CommandSandbox ──▶ subprocess    │
│                                 │                           │
│                          ┌──────┴──────┐                    │
│                          │ Validations │                    │
│                          │ • allowlist │                    │
│                          │ • denylist  │                    │
│                          │ • timeout   │                    │
│                          │ • resource  │                    │
│                          │ • env scrub │                    │
│                          └─────────────┘                    │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 CommandSandbox

```rust
pub struct CommandSandbox {
    /// Commands that are always allowed (prefix match)
    allowlist: Vec<String>,
    /// Commands/patterns that are always denied
    denylist: Vec<Regex>,
    /// Maximum execution time per command
    default_timeout: Duration,
    /// Maximum output size (bytes) — truncated beyond this
    max_output_bytes: usize,
    /// Environment variables to REMOVE from subprocess
    env_scrub: Vec<String>,
    /// Environment variables to INJECT into subprocess
    env_inject: HashMap<String, String>,
    /// Whether to run in a pseudo-terminal
    pty_mode: bool,
    /// Working directory (always workspace root)
    working_dir: PathBuf,
}

impl CommandSandbox {
    pub async fn execute(&self, command: &str) -> Result<CommandResult, SandboxError> {
        // 1. Parse command into binary + args
        let parsed = self.parse_command(command)?;

        // 2. Check denylist first (always takes precedence)
        for pattern in &self.denylist {
            if pattern.is_match(command) {
                return Err(SandboxError::CommandDenied {
                    command: command.to_string(),
                    reason: format!("Matches denylist pattern: {}", pattern),
                });
            }
        }

        // 3. Check allowlist
        let allowed = self.allowlist.iter()
            .any(|prefix| parsed.binary.starts_with(prefix));
        if !allowed {
            return Err(SandboxError::CommandDenied {
                command: command.to_string(),
                reason: format!(
                    "'{}' is not in the command allowlist. Allowed: {:?}",
                    parsed.binary, self.allowlist
                ),
            });
        }

        // 4. Build subprocess with sanitized environment
        let mut cmd = tokio::process::Command::new(&parsed.binary);
        cmd.args(&parsed.args)
           .current_dir(&self.working_dir)
           .stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());

        // Scrub sensitive environment variables
        for key in &self.env_scrub {
            cmd.env_remove(key);
        }
        // Inject safe environment
        cmd.envs(&self.env_inject);
        // Always set a marker so child processes know they're sandboxed
        cmd.env("XAFT_SANDBOXED", "1");

        // 5. Execute with timeout
        let output = tokio::time::timeout(
            self.default_timeout,
            cmd.output(),
        ).await.map_err(|_| SandboxError::Timeout {
            command: command.to_string(),
            duration: self.default_timeout,
        })??;

        // 6. Truncate output if needed
        let stdout = self.truncate_output(&output.stdout);
        let stderr = self.truncate_output(&output.stderr);

        Ok(CommandResult {
            exit_code: output.status.code(),
            stdout,
            stderr,
            truncated: output.stdout.len() > self.max_output_bytes
                || output.stderr.len() > self.max_output_bytes,
            duration: self.default_timeout, // approximate
        })
    }
}
```

### 4.3 Environment Scrubbing

Default environment variables removed from subprocess:

```
┌──────────────────────────────────────────────────────┐
│            SCRUBBED ENVIRONMENT VARIABLES            │
├──────────────────────┬───────────────────────────────┤
│ Variable             │ Reason                        │
├──────────────────────┼───────────────────────────────┤
│ AWS_SECRET_ACCESS_KEY│ Cloud credential              │
│ AWS_ACCESS_KEY_ID    │ Cloud credential              │
│ GITHUB_TOKEN         │ Git credential                │
│ GH_TOKEN             │ Git credential                │
│ OPENAI_API_KEY       │ LLM credential                │
│ ANTHROPIC_API_KEY    │ LLM credential                │
│ DATABASE_URL         │ DB connection string           │
│ SSH_AUTH_SOCK        │ SSH agent socket               │
│ GNUPGHOME            │ GPG home                       │
│ KUBECONFIG           │ Kubernetes credential          │
│ DOCKER_CONFIG        │ Docker credential              │
│ NPM_TOKEN            │ Package registry credential    │
│ CARGO_REGISTRY_TOKEN │ Rust registry credential       │
│ VAULT_TOKEN          │ HashiCorp Vault token           │
└──────────────────────┴───────────────────────────────┘
```

### 4.4 Resource Limits

```rust
/// Per-command resource limits enforced via cgroups (Linux) or
/// job objects (Windows).
pub struct ResourceLimits {
    /// Maximum CPU time (seconds)
    pub max_cpu_seconds: u32,
    /// Maximum resident set size (bytes)
    pub max_memory_bytes: u64,
    /// Maximum number of processes/threads
    pub max_processes: u32,
    /// Maximum file descriptors
    pub max_fds: u32,
    /// Maximum files created
    pub max_files_created: u32,
    /// Maximum total bytes written
    pub max_bytes_written: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_cpu_seconds: 300,       // 5 minutes
            max_memory_bytes: 512 * 1024 * 1024,  // 512 MiB
            max_processes: 64,
            max_fds: 256,
            max_files_created: 1000,
            max_bytes_written: 50 * 1024 * 1024,  // 50 MiB
        }
    }
}
```

---

## 5. Network Access Control

### 5.1 Network Policy

Network access is controlled at the domain and port level. By default, xaft
operates under a **deny-all** network policy with explicit allowlisting.

```rust
pub struct NetworkSandbox {
    /// Domains that are allowed (suffix match)
    allowed_domains: Vec<String>,
    /// IP ranges that are allowed (CIDR)
    allowed_cidrs: Vec<IpCidr>,
    /// Ports that are allowed
    allowed_ports: Vec<u16>,
    /// Whether to block all outbound connections
    block_outbound: bool,
    /// Whether to block all inbound connections
    block_inbound: bool,
    /// DNS resolution policy
    dns_policy: DnsPolicy,
}

pub enum DnsPolicy {
    /// Allow all DNS resolutions
    AllowAll,
    /// Only allow DNS for allowed_domains
    Restricted,
    /// Use a custom DNS resolver that only resolves allowed domains
    CustomResolver { resolver: String },
}

impl NetworkSandbox {
    /// Check whether a network request is permitted.
    pub fn check_request(&self, url: &Url) -> Result<(), SandboxError> {
        if self.block_outbound {
            return Err(SandboxError::NetworkDenied {
                url: url.to_string(),
                reason: "All outbound network access is blocked".into(),
            });
        }

        // Check domain allowlist
        let host = url.host_str().unwrap_or("");
        let domain_allowed = self.allowed_domains.iter()
            .any(|d| host == d || host.ends_with(&format!(".{}", d)));

        if !domain_allowed {
            return Err(SandboxError::NetworkDenied {
                url: url.to_string(),
                reason: format!(
                    "Domain '{}' is not in the allowlist: {:?}",
                    host, self.allowed_domains
                ),
            });
        }

        // Check port
        let port = url.port_or_known_default()
            .ok_or_else(|| SandboxError::NetworkDenied {
                url: url.to_string(),
                reason: "Unknown port".into(),
            })?;

        if !self.allowed_ports.contains(&port) {
            return Err(SandboxError::NetworkDenied {
                url: url.to_string(),
                reason: format!("Port {} is not allowed", port),
            });
        }

        Ok(())
    }
}
```

### 5.2 Default Network Allowlist

```
┌──────────────────────────────────────────────────────┐
│            DEFAULT NETWORK ALLOWLIST                 │
├──────────────────────────┬───────────────────────────┤
│ Domain                   │ Purpose                   │
├──────────────────────────┼───────────────────────────┤
│ api.openai.com           │ LLM provider              │
│ api.anthropic.com        │ LLM provider              │
│ generativelanguage.goo…  │ LLM provider (Gemini)     │
│ api.github.com           │ GitHub API                │
│ github.com               │ Git operations            │
│ crates.io                │ Rust package registry     │
│ index.crates.io          │ Rust crate index          │
│ registry.npmjs.org       │ NPM registry              │
│ pypi.org                 │ Python package index      │
│ repo1.maven.org          │ Maven repository          │
│ 443                      │ Allowed port: HTTPS       │
│ 22                       │ Allowed port: SSH (git)   │
└──────────────────────────┴───────────────────────────┘
```

### 5.3 Network Request Flow

```
Agent calls net_request(url)
       │
       ▼
┌─────────────────────┐
│  NetworkSandbox     │
│  .check_request()   │
└─────────┬───────────┘
          │
    ┌─────┴─────┐
    │ Allowed?  │
    └──┬─────┬──┘
    Yes│     │No
       ▼     ▼
  ┌────────┐ ERR: NetworkDenied
  │ Execute│
  │ HTTP   │
  │ request│
  └───┬────┘
      │
      ▼
┌─────────────────┐
│ Response size   │
│ check (max 1MB) │
└────────┬────────┘
         │
         ▼
    Return result
```

---

## 6. WorktreeGuard: Git Isolation

### 6.1 Problem

When an agent modifies files, we need a way to:
1. Isolate changes from the user's working tree
2. Provide atomic rollback
3. Enable review-before-merge workflows

### 6.2 WorktreeGuard Design

`WorktreeGuard` creates a **git worktree** for each agent task, ensuring the
agent's modifications are isolated to a separate directory.

```rust
/// Manages a git worktree for agent task isolation.
pub struct WorktreeGuard {
    /// Path to the main repository
    repo_root: PathBuf,
    /// Path to the agent's worktree
    worktree_path: PathBuf,
    /// Branch name for this worktree
    branch_name: String,
    /// Original HEAD commit for rollback
    base_commit: GitHash,
    /// Whether the worktree has been created
    initialized: bool,
}

impl WorktreeGuard {
    /// Create a new worktree guard for a task.
    pub async fn new(repo_root: &Path, task_id: &str) -> Result<Self, WorktreeError> {
        let branch_name = format!("xaft/task-{}", task_id);
        let worktree_path = repo_root.join(format!(".xaft/worktrees/{}", task_id));

        // Get current HEAD
        let base_commit = git::get_head(repo_root).await?;

        // Create worktree with a new branch from HEAD
        git::create_worktree(
            repo_root,
            &worktree_path,
            &branch_name,
        ).await?;

        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            worktree_path,
            branch_name,
            base_commit,
            initialized: true,
        })
    }

    /// Get the workspace path that the agent should operate on.
    /// This is the worktree path, NOT the main repo path.
    pub fn workspace_path(&self) -> &Path {
        &self.worktree_path
    }

    /// Commit all changes in the worktree.
    pub async fn commit(&self, message: &str) -> Result<GitHash, WorktreeError> {
        git::add_all(&self.worktree_path).await?;
        let hash = git::commit(&self.worktree_path, message).await?;
        Ok(hash)
    }

    /// Generate a diff of all changes since worktree creation.
    pub async fn diff(&self) -> Result<String, WorktreeError> {
        git::diff_against(&self.worktree_path, &self.base_commit).await
    }

    /// Merge the worktree branch back into the main branch.
    pub async fn merge_back(&self, target_branch: &str) -> Result<(), WorktreeError> {
        git::checkout(&self.repo_root, target_branch).await?;
        git::merge(&self.repo_root, &self.branch_name).await?;
        Ok(())
    }

    /// Restore the worktree to its original state (discard all changes).
    pub async fn restore(&self) -> Result<(), WorktreeError> {
        git::checkout(&self.worktree_path, &self.base_commit.to_string()).await?;
        git::clean(&self.worktree_path).await?;
        Ok(())
    }

    /// Clean up the worktree entirely.
    pub async fn teardown(self) -> Result<(), WorktreeError> {
        if self.initialized {
            git::remove_worktree(&self.repo_root, &self.worktree_path).await?;
            git::delete_branch(&self.repo_root, &self.branch_name).await?;
        }
        Ok(())
    }
}
```

### 6.3 Worktree Lifecycle

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────┐
│ Task Start   │────▶│ WorktreeGuard::  │────▶│ Agent works  │
│              │     │ new()            │     │ in worktree  │
└──────────────┘     │                  │     │ (isolated)   │
                     │ • Create branch  │     └──────┬───────┘
                     │ • Create worktree│            │
                     │ • Record base    │            ▼
                     └──────────────────┘     ┌──────────────┐
                                              │ Task         │
                                              │ Completes    │
                                              └──────┬───────┘
                                                     │
                                        ┌────────────┼────────────┐
                                        │            │            │
                                        ▼            ▼            ▼
                                   ┌─────────┐ ┌──────────┐ ┌──────────┐
                                   │ Merge   │ │ Restore  │ │ Teardown │
                                   │ back    │ │ (discard)│ │ (delete) │
                                   │ (apply) │ │ (abort)  │ │ (clean)  │
                                   └────┬────┘ └────┬─────┘ └────┬─────┘
                                        │           │            │
                                        ▼           ▼            ▼
                                   User's main   Worktree      Worktree
                                   branch has    reset to      removed,
                                   agent's       base_commit   branch
                                   changes                     deleted
```

### 6.4 WorkspaceStore Integration

The `WorkspaceStore` is aware of `WorktreeGuard` and uses the worktree path as
its root when a guard is active:

```rust
impl WorkspaceStore {
    /// Create a WorkspaceStore scoped to a WorktreeGuard's path.
    pub fn from_worktree(guard: &WorktreeGuard) -> Self {
        Self {
            root: guard.workspace_path().to_path_buf(),
            extra_allowed: vec![],
            deny_patterns: Self::default_deny_patterns(),
            allow_symlink_escape: false,
        }
    }
}
```

---

## 7. Future: Container Sandboxing

### 7.1 Motivation

Process-level sandboxing provides path and command restrictions, but it cannot
prevent:
- A determined agent from exploiting kernel vulnerabilities
- Resource exhaustion at the OS level
- Side-channel attacks between concurrent agent tasks

Container-level sandboxing provides stronger isolation guarantees.

### 7.2 ContainerSandbox Trait

```rust
/// Future trait for container-based sandboxing.
#[async_trait]
pub trait ContainerSandbox: Send + Sync {
    /// Create a new container for an agent task.
    async fn create(&self, spec: ContainerSpec) -> Result<ContainerHandle, SandboxError>;

    /// Execute a command inside the container.
    async fn exec(&self, handle: &ContainerHandle, cmd: &str) -> Result<CommandResult, SandboxError>;

    /// Copy files between host and container.
    async fn copy_in(&self, handle: &ContainerHandle, host_path: &Path, container_path: &Path) -> Result<(), SandboxError>;
    async fn copy_out(&self, handle: &ContainerHandle, container_path: &Path, host_path: &Path) -> Result<(), SandboxError>;

    /// Stop and remove the container.
    async fn destroy(&self, handle: ContainerHandle) -> Result<(), SandboxError>;
}

pub struct ContainerSpec {
    /// Container image to use
    pub image: String,
    /// Mount the workspace as a read-write volume
    pub workspace_mount: String,
    /// Memory limit
    pub memory_limit: u64,
    /// CPU limit (cores)
    pub cpu_limit: f64,
    /// Network policy
    pub network: ContainerNetwork,
    /// Environment variables
    pub env: HashMap<String, String>,
}

pub enum ContainerNetwork {
    /// No network access
    None,
    /// Only specific domains/ports
    Restricted { allowlist: Vec<String> },
    /// Full network (for trusted tasks only)
    Full,
}
```

### 7.3 Container Backends

```
┌─────────────────────────────────────┐
│       ContainerSandbox trait        │
└───────────────┬─────────────────────┘
                │
    ┌───────────┼───────────┐
    │           │           │
    ▼           ▼           ▼
┌────────┐ ┌────────┐ ┌──────────┐
│ Docker │ │ gVisor │ │Firecracker│
│        │ │ runsc  │ │ microVM  │
│ Broad  │ │ Strong │ │ Strongest│
│ compat │ │ isol.  │ │ isol.    │
│ Weakest│ │ Medium │ │ Highest  │
│ isol.  │ │ compat │ │ overhead │
└────────┘ └────────┘ └──────────┘
```

### 7.4 Container Lifecycle

```
xaft task start
       │
       ▼
┌──────────────────────┐
│ ContainerSandbox::   │
│ create(spec)         │
│                      │
│ • Pull image         │
│ • Set up volumes     │
│ • Configure network  │
│ • Apply resource     │
│   limits             │
└──────────┬───────────┘
           ▼
┌──────────────────────┐
│ Agent runs inside    │
│ container:           │
│                      │
│ • File ops → volume  │
│ • Shell → container  │
│ • Network → policy   │
│ • Resources → limits │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Task complete:       │
│                      │
│ • copy_out results   │
│ • destroy container  │
│ • commit to git      │
└──────────────────────┘
```

### 7.5 Gradual Isolation Tiers

```toml
[sandbox]
# Isolation tier: "process" (default), "container", "vm"
tier = "process"

[sandbox.container]
image = "xaft/agent:latest"
memory_limit = "512Mi"
cpu_limit = 1.0
network = "restricted"

[sandbox.container.network]
allow = [
    "api.openai.com:443",
    "crates.io:443",
    "github.com:22",
]
```

```
┌─────────────────────────────────────────────────────────────────┐
│                 ISOLATION TIERS                                  │
├──────────┬──────────────┬───────────────┬───────────────────────┤
│ Aspect   │ Process      │ Container     │ VM (Firecracker)      │
├──────────┼──────────────┼───────────────┼───────────────────────┤
│ FS       │ WorkspaceSt. │ Volume mount  │ Block device           │
│ Commands │ Allowlist    │ Container sh  │ VM shell               │
│ Network  │ Domain allow │ Net policies  │ Network NS / bridge    │
│ Resources│ ulimit       │ cgroups       │ VM config              │
│ Escape   │ Easy         │ Hard          │ Very hard              │
│ Overhead │ ~0ms         │ ~200ms start  │ ~1s start              │
│ Compat   │ Full         │ Near-full     │ Linux-only             │
└──────────┴──────────────┴───────────────┴───────────────────────┘
```

---

## 8. Configuration

```toml
[sandbox]
tier = "process"

[sandbox.filesystem]
# Workspace root (auto-detected if not set)
root = "."
# Extra allowed paths outside the workspace
extra_allowed = ["/tmp/xaft-cache"]
# Deny patterns (glob)
deny_patterns = ["**/.env", "**/.env.*", "**/*.pem", "**/*.key"]
# Allow symlinks that escape workspace root
allow_symlink_escape = false

[sandbox.commands]
allow = ["ls", "cat", "rg", "cargo", "npm", "npx", "git", "python3", "node", "make", "cmake"]
deny_patterns = ["sudo .*", "rm -rf /", "mkfs.*", "dd if=.*of=/dev/"]
default_timeout_secs = 300
max_output_bytes = 1_048_576
pty_mode = false

[sandbox.commands.env]
scrub = ["AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "OPENAI_API_KEY"]
inject = { XAFT_SANDBOXED = "1", TERM = "dumb" }

[sandbox.commands.resources]
max_cpu_seconds = 300
max_memory_bytes = 536870912  # 512 MiB
max_processes = 64

[sandbox.network]
block_outbound = false
allowed_domains = ["api.openai.com", "crates.io", "github.com"]
allowed_ports = [443, 22]
dns_policy = "restricted"
```

---

## 9. Security Considerations

| # | Threat | Mitigation |
|---|--------|------------|
| 1 | Path traversal via `../../etc/passwd` | WorkspaceStore canonicalization |
| 2 | Symlink escape pointing outside workspace | Symlink resolution + boundary check |
| 3 | Agent reads `.env` file with secrets | Deny patterns on sensitive files |
| 4 | Agent runs `sudo` or destructive commands | Command denylist + allowlist |
| 5 | Agent exfiltrates data via network | Domain allowlist + port restrictions |
| 6 | Agent forks bomb via shell command | Resource limits (cgroups/ulimit) |
| 7 | Agent overwrites git history | WorktreeGuard isolation |
| 8 | Environment variable leakage | Env scrubbing |
| 9 | TOCTOU on path validation | Atomic operations, validate-on-use |
| 10 | Container escape | Tier selection (gVisor / Firecracker) |

---

## 10. Open Questions

| # | Question | Status |
|---|----------|--------|
| 1 | Should workspace auto-detection walk up to `.git` root? | Open |
| 2 | macOS sandboxing via Seatbelt profiles? | Planned |
| 3 | How to handle tools that legitimately need network access (e.g., `cargo publish`)? | Open |
| 4 | Per-tool sandbox profiles vs. global sandbox config? | Open |
| 5 | Audit logging for sandbox violations — local vs. remote? | Open |
| 6 | Container image building and caching strategy? | Deferred |
| 7 | GPU access inside containers (for ML workloads)? | Deferred |
