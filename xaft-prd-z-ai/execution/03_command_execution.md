# XAFT Command Execution — PRD

> Document ID: XAFT-EXEC-003
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

`xaft` must execute shell commands on behalf of the user — running tests, installing dependencies, building projects, and more. This is the most security-sensitive surface of the entire system. This document specifies the `ShellCommand` tool, sandboxing strategies (seccomp, namespaces, Docker), signal emission, policy enforcement, output capture, timeouts, working directory management, and environment variable handling.

---

## 2. Architecture

```
 ┌───────────────────────────────────────────────────────────────────────────┐
 │                      Command Execution Architecture                       │
 │                                                                           │
 │  ┌──────────────┐   ┌──────────────┐   ┌────────────────┐   ┌─────────┐ │
 │  │ ShellCommand  │──▶│  Policy      │──▶│  Sandbox       │──▶│ Process │ │
 │  │ Tool          │   │  Engine      │   │  (seccomp/     │   │ Spawn   │ │
 │  │               │   │  (allow/deny)│   │   ns/Docker)   │   │         │ │
 │  └──────────────┘   └──────┬───────┘   └────────────────┘   └────┬────┘ │
 │                            │                                      │      │
 │                            ▼                                      ▼      │
 │                    ┌───────────────┐                    ┌──────────────┐ │
 │                    │ Signal Bus    │◀───────────────────│ Output       │ │
 │                    │ (Started/     │                    │ Capture      │ │
 │                    │  Completed/   │                    │ (stdout/     │ │
 │                    │  Violation)   │                    │  stderr)     │ │
 │                    └───────────────┘                    └──────────────┘ │
 └───────────────────────────────────────────────────────────────────────────┘
```

---

## 3. ShellCommand Tool

### 3.1 Tool Definition

```rust
/// The primary command execution tool for the xaft agent.
pub struct ShellCommandTool {
    config: ShellCommandConfig,
    policy: CommandPolicy,
    sandbox: SandboxProvider,
    signal_bus: SignalBus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommandInput {
    /// The command to execute (shell string).
    pub command: String,

    /// Arguments to the command (if not using shell expansion).
    pub args: Option<Vec<String>>,

    /// Working directory for command execution.
    /// Relative paths are resolved against the repo root.
    pub working_dir: Option<PathBuf>,

    /// Environment variables to set for this command.
    pub env: Option<HashMap<String, String>>,

    /// Timeout in seconds (overrides global default).
    pub timeout_secs: Option<u64>,

    /// Whether to stream output in real-time (vs. capture at end).
    pub stream_output: Option<bool>,

    /// Human-readable description of what this command does.
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommandOutput {
    /// Exit code of the process.
    pub exit_code: Option<i32>,

    /// Captured stdout (truncated if exceeds max_capture_bytes).
    pub stdout: String,

    /// Captured stderr (truncated if exceeds max_capture_bytes).
    pub stderr: String,

    /// Duration of execution.
    pub duration: Duration,

    /// Whether the command was killed due to timeout.
    pub timed_out: bool,

    /// Whether the command was killed due to policy violation.
    pub killed_by_policy: bool,

    /// Signal that terminated the process (if any).
    pub terminate_signal: Option<i32>,
}
```

### 3.2 Tool Implementation

```rust
impl Tool for ShellCommandTool {
    type Input  = ShellCommandInput;
    type Output = ShellCommandOutput;

    fn name(&self) -> &str { "shell_command" }

    async fn execute(&self, input: Self::Input) -> Result<Self::Output, ToolError> {
        // ── Phase 1: Policy Check ──
        let policy_result = self.policy.evaluate(&input.command)?;
        match policy_result {
            PolicyResult::Allow => {},
            PolicyResult::Deny(reason) => {
                self.signal_bus.emit(Signal::CommandPolicyViolation {
                    command: input.command.clone(),
                    reason: reason.clone(),
                });
                return Err(ToolError::PolicyViolation { command: input.command, reason });
            }
            PolicyResult::RequireApproval(reason) => {
                // In interactive mode, prompt the user
                // In non-interactive mode, deny
                return Err(ToolError::ApprovalRequired { command: input.command, reason });
            }
        }

        // ── Phase 2: Signal Emission ──
        self.signal_bus.emit(Signal::ShellCommandStarted {
            command: input.command.clone(),
            working_dir: input.working_dir.clone(),
            description: input.description.clone(),
            pid: None, // filled in after spawn
        });

        // ── Phase 3: Sandbox Setup ──
        let sandbox_config = self.sandbox.configure(&input)?;

        // ── Phase 4: Process Spawn ──
        let timeout = input.timeout_secs
            .or(self.config.default_timeout_secs)
            .unwrap_or(120);

        let start = Instant::now();

        let mut child = self.spawn_process(&input, &sandbox_config)?;

        // Update signal with PID
        self.signal_bus.emit(Signal::ShellCommandStarted {
            pid: Some(child.id()),
            ..ShellCommandStartedSignal::from_input(&input)
        });

        // ── Phase 5: Output Capture ──
        let output_result = self.capture_output(&mut child, timeout, input.stream_output.unwrap_or(true)).await;

        let duration = start.elapsed();

        // ── Phase 6: Signal Completion ──
        let output = match output_result {
            Ok(out) => {
                self.signal_bus.emit(Signal::ShellCommandCompleted {
                    command: input.command.clone(),
                    exit_code: out.status.code(),
                    duration,
                    stdout_len: out.stdout.len(),
                    stderr_len: out.stderr.len(),
                });

                ShellCommandOutput {
                    exit_code: out.status.code(),
                    stdout: self.truncate_output(&out.stdout),
                    stderr: self.truncate_output(&out.stderr),
                    duration,
                    timed_out: false,
                    killed_by_policy: false,
                    terminate_signal: None,
                }
            }
            Err(CaptureError::Timeout) => {
                self.signal_bus.emit(Signal::ShellCommandTimedOut {
                    command: input.command.clone(),
                    timeout_secs: timeout,
                });

                ShellCommandOutput {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Command timed out after {}s", timeout),
                    duration,
                    timed_out: true,
                    killed_by_policy: false,
                    terminate_signal: Some(9), // SIGKILL
                }
            }
            Err(CaptureError::PolicyViolation(reason)) => {
                self.signal_bus.emit(Signal::CommandPolicyViolation {
                    command: input.command.clone(),
                    reason: reason.clone(),
                });

                ShellCommandOutput {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Killed by policy: {}", reason),
                    duration,
                    timed_out: false,
                    killed_by_policy: true,
                    terminate_signal: Some(9),
                }
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed {
                    command: input.command,
                    error: e.to_string(),
                });
            }
        };

        Ok(output)
    }
}
```

---

## 4. Sandboxing

### 4.1 Sandbox Strategies

```
 ┌──────────────────────────────────────────────────────────────────────┐
 │                      Sandbox Strategies                              │
 │                                                                      │
 │   Level 0: None                                                     │
 │   ┌────────────────────────────────────────────────────────────┐    │
 │   │ No isolation. Command runs with full user privileges.      │    │
 │   │ Only for: trusted CI environments, user-explicit opt-in.   │    │
 │   └────────────────────────────────────────────────────────────┘    │
 │                                                                      │
 │   Level 1: seccomp-bpf                                              │
 │   ┌────────────────────────────────────────────────────────────┐    │
 │   │ Syscall filtering. Allow: read, write, open, close,       │    │
 │   │ stat, fstat, mmap, mprotect, munmap, brk, access,        │    │
 │   │ chdir, getpid, clock_gettime, clone (for threads),        │    │
 │   │ exit_group, futex, sigaction, rt_sigprocmask,             │    │
 │   │ ioctl (limited), pipe, dup, dup2, poll, epoll_wait       │    │
 │   │ Deny: execve (for child spawning), mount, umount,        │    │
 │   │ chroot, pivot_root, ptrace, reboot, kexec_load           │    │
 │   └────────────────────────────────────────────────────────────┘    │
 │                                                                      │
 │   Level 2: Linux Namespaces                                         │
 │   ┌────────────────────────────────────────────────────────────┐    │
 │   │ PID namespace (isolated process tree)                      │    │
 │   │ Mount namespace (restricted filesystem view)               │    │
 │   │ Network namespace (no external network or loopback only)   │    │
 │   │ User namespace (mapped UID, no root privileges)           │    │
 │   │ IPC namespace (isolated SysV/POSIX IPC)                   │    │
 │   │ UTS namespace (isolated hostname)                         │    │
 │   └────────────────────────────────────────────────────────────┘    │
 │                                                                      │
 │   Level 3: Docker Container                                         │
 │   ┌────────────────────────────────────────────────────────────┐    │
 │   │ Full container isolation with:                             │    │
 │   │ - Read-only root filesystem                               │    │
 │   │ - Repo mounted as volume (read-write)                     │    │
 │   │ - Resource limits (CPU, memory, PIDs)                     │    │
 │   │ - No network by default (optional: limited outbound)      │    │
 │   │ - Drop all capabilities                                   │    │
 │   │ - seccomp profile + AppArmor profile                      │    │
 │   └────────────────────────────────────────────────────────────┘    │
 └──────────────────────────────────────────────────────────────────────┘
```

### 4.2 Sandbox Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Sandbox isolation level.
    pub level: SandboxLevel,

    /// For Level 2/3: whether to allow network access.
    pub allow_network: bool,        // default: false

    /// For Level 3: Docker image to use.
    pub docker_image: Option<String>, // default: "xaft/sandbox:latest"

    /// For Level 3: memory limit (e.g., "512m").
    pub memory_limit: Option<String>, // default: "1g"

    /// For Level 3: CPU limit (e.g., "1.0" = 1 core).
    pub cpu_limit: Option<f64>,     // default: 2.0

    /// For Level 3: PID limit.
    pub pid_limit: Option<u64>,    // default: 256

    /// For Level 2/3: writable paths (in addition to repo).
    pub writable_paths: Vec<PathBuf>,

    /// For Level 1/2/3: denied syscalls (additions to default deny list).
    pub denied_syscalls: Vec<String>,

    /// For Level 2/3: environment variables to pass through.
    pub pass_env: Vec<String>,     // default: ["PATH", "HOME", "LANG", "TERM"]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxLevel {
    None = 0,
    Seccomp = 1,
    Namespaces = 2,
    Docker = 3,
}

pub trait SandboxProvider: Send + Sync {
    fn configure(&self, input: &ShellCommandInput) -> Result<SandboxConfig, SandboxError>;
    fn apply(&self, command: &mut Command, config: &SandboxConfig) -> Result<(), SandboxError>;
}
```

### 4.3 Seccomp Implementation

```rust
pub struct SeccompSandbox {
    config: SandboxConfig,
}

impl SandboxProvider for SeccompSandbox {
    fn configure(&self, input: &ShellCommandInput) -> Result<SandboxConfig, SandboxError> {
        Ok(self.config.clone())
    }

    fn apply(&self, command: &mut Command, config: &SandboxConfig) -> Result<(), SandboxError> {
        // Prepend seccomp wrapper
        // We use a helper binary that installs the BPF filter before exec
        command.arg0("/usr/lib/xaft/seccomp-wrapper");
        command.env("XAFT_SECCOMP_PROFILE", self.generate_profile(config)?);
        Ok(())
    }
}

impl SeccompSandbox {
    fn generate_profile(&self, config: &SandboxConfig) -> Result<String, SandboxError> {
        // Generate a JSON seccomp profile compatible with libseccomp
        let allowed_syscalls = vec![
            "read", "write", "open", "openat", "close", "stat", "fstat",
            "lstat", "poll", "lseek", "mmap", "mprotect", "munmap", "brk",
            "rt_sigaction", "rt_sigprocmask", "ioctl", "access", "pipe",
            "select", "sched_yield", "mremap", "nanosleep", "clock_gettime",
            "clock_getres", "fork", "vfork", "execve", "wait4", "kill",
            "uname", "fcntl", "flock", "fsync", "fdatasync", "truncate",
            "ftruncate", "getdents", "getcwd", "chdir", "rename", "mkdir",
            "rmdir", "creat", "link", "unlink", "symlink", "readlink",
            "chmod", "fchmod", "chown", "fchown", "lchown", "umask",
            "gettimeofday", "getrlimit", "getrusage", "sysinfo", "times",
            "getuid", "getgid", "geteuid", "getegid", "getgroups",
            "getpgid", "getppid", "getpgrp", "setsid", "setreuid",
            "setregid", "getresuid", "getresgid", "sigaltstack",
            "mknod", "statfs", "fstatfs", "arch_prctl",
            "set_tid_address", "set_robust_list", "futex",
            "sched_getaffinity", "exit_group", "exit",
            "clock_nanosleep", "epoll_create", "epoll_wait",
            "epoll_ctl", "dup", "dup2", "dup3", "pipe2",
            "pread64", "pwrite64", "readv", "writev",
            "prlimit64", "getrandom", "fcntl",
        ];

        let denied = &config.denied_syscalls;
        let profile = serde_json::json!({
            "defaultAction": "SCMP_ACT_ERRNO",
            "architectures": ["SCMP_ARCH_X86_64"],
            "syscalls": allowed_syscalls.iter()
                .filter(|s| !denied.contains(&s.to_string()))
                .map(|s| serde_json::json!({
                    "names": [s],
                    "action": "SCMP_ACT_ALLOW"
                }))
                .collect::<Vec<_>>()
        });

        Ok(serde_json::to_string(&profile)?)
    }
}
```

### 4.4 Namespace Implementation

```rust
pub struct NamespaceSandbox {
    config: SandboxConfig,
}

impl SandboxProvider for NamespaceSandbox {
    fn configure(&self, input: &ShellCommandInput) -> Result<SandboxConfig, SandboxError> {
        Ok(self.config.clone())
    }

    fn apply(&self, command: &mut Command, config: &SandboxConfig) -> Result<(), SandboxError> {
        // Use the `unshare` command or clone() with namespace flags
        // We use a helper binary for namespace setup
        let mut sandbox_cmd = Command::new("/usr/lib/xaft/ns-sandbox");

        // PID namespace
        sandbox_cmd.arg("--pid");

        // Mount namespace: mount repo as the working tree
        sandbox_cmd.arg("--mount");
        sandbox_cmd.arg(format!("--bind-ro={}", config.writable_paths.first().unwrap().display()));

        // Network namespace: no external network
        if !config.allow_network {
            sandbox_cmd.arg("--net=none");
        } else {
            sandbox_cmd.arg("--net=loopback");
        }

        // User namespace
        sandbox_cmd.arg("--user");

        // IPC namespace
        sandbox_cmd.arg("--ipc");

        // UTS namespace
        sandbox_cmd.arg("--uts");

        // Pass through environment
        for env_var in &config.pass_env {
            sandbox_cmd.arg(format!("--pass-env={}", env_var));
        }

        // The actual command follows "--"
        sandbox_cmd.arg("--");
        sandbox_cmd.arg("sh").arg("-c").arg(&input.command);

        *command = sandbox_cmd;
        Ok(())
    }
}
```

### 4.5 Docker Implementation

```rust
pub struct DockerSandbox {
    config: SandboxConfig,
}

impl SandboxProvider for DockerSandbox {
    fn configure(&self, input: &ShellCommandInput) -> Result<SandboxConfig, SandboxError> {
        // Verify Docker daemon is available
        let output = Command::new("docker").arg("info").output()?;
        if !output.status.success() {
            return Err(SandboxError::DockerUnavailable);
        }
        Ok(self.config.clone())
    }

    fn apply(&self, command: &mut Command, config: &SandboxConfig) -> Result<(), SandboxError> {
        let image = config.docker_image.as_deref().unwrap_or("xaft/sandbox:latest");

        let mut docker_cmd = Command::new("docker");
        docker_cmd.arg("run");

        // Remove container after exit
        docker_cmd.arg("--rm");

        // Read-only root filesystem
        docker_cmd.arg("--read-only");

        // Mount repo as volume
        docker_cmd.arg(format!(
            "-v{}:/workspace:rw",
            config.writable_paths.first().unwrap().display()
        ));

        // Working directory inside container
        docker_cmd.arg("-w").arg("/workspace");

        // Network isolation
        if !config.allow_network {
            docker_cmd.arg("--network=none");
        }

        // Resource limits
        if let Some(ref mem) = config.memory_limit {
            docker_cmd.arg(format!("--memory={}", mem));
        }
        if let Some(cpu) = config.cpu_limit {
            docker_cmd.arg(format!("--cpus={}", cpu));
        }
        if let Some(pids) = config.pid_limit {
            docker_cmd.arg(format!("--pids-limit={}", pids));
        }

        // Drop all capabilities
        docker_cmd.arg("--cap-drop=ALL");

        // Security options
        docker_cmd.arg("--security-opt=no-new-privileges");
        docker_cmd.arg("--security-opt=seccomp=/etc/xaft/seccomp-profile.json");

        // Pass through environment variables
        for env_var in &config.pass_env {
            docker_cmd.arg(format!("--env={}", env_var));
        }

        // Image
        docker_cmd.arg(image);

        // The command
        docker_cmd.arg("sh").arg("-c");

        *command = docker_cmd;
        Ok(())
    }
}
```

---

## 5. Shell Signals

### 5.1 Signal Types

```rust
/// Signals emitted by the command execution system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signal {
    /// A shell command has started executing.
    ShellCommandStarted {
        command: String,
        working_dir: Option<PathBuf>,
        description: String,
        pid: Option<u32>,
    },

    /// A shell command has completed.
    ShellCommandCompleted {
        command: String,
        exit_code: Option<i32>,
        duration: Duration,
        stdout_len: usize,
        stderr_len: usize,
    },

    /// A shell command timed out.
    ShellCommandTimedOut {
        command: String,
        timeout_secs: u64,
    },

    /// A command was killed due to policy violation.
    CommandPolicyViolation {
        command: String,
        reason: String,
    },

    /// A sandbox violation was detected.
    SandboxViolation {
        command: String,
        violation_type: SandboxViolationType,
        details: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxViolationType {
    SyscallViolation(String),
    NetworkAccessAttempt(String),
    FilesystemAccessViolation(PathBuf),
    ResourceLimitExceeded(String),
}
```

### 5.2 Signal Flow

```
 ┌───────────────────┐
 │  ShellCommandTool │
 └─────────┬─────────┘
           │
           │  1. Emit ShellCommandStarted
           │     (command, pid, working_dir)
           │
           ▼
 ┌───────────────────┐
 │  Process Spawned  │
 └─────────┬─────────┘
           │
           │  2. (Optional) Emit SandboxViolation
           │     if sandbox detects violation
           │     → Kill process
           │
           ▼
 ┌───────────────────┐
 │  Process Running  │
 └─────────┬─────────┘
           │
           │  3a. On completion:
           │      Emit ShellCommandCompleted
           │      (exit_code, duration, output sizes)
           │
           │  3b. On timeout:
           │      Emit ShellCommandTimedOut
           │      (command, timeout)
           │
           │  3c. On policy violation:
           │      Emit CommandPolicyViolation
           │      (command, reason)
           │
           ▼
 ┌───────────────────┐
 │  TUI / Logger     │  ← Consumes signals for display
 └───────────────────┘
```

---

## 6. Command Policy Engine

### 6.1 Policy Model

```rust
/// Policy engine for controlling which commands can be executed.
pub struct CommandPolicy {
    /// Global allow/deny rules.
    rules: Vec<PolicyRule>,
    /// Default action when no rule matches.
    default_action: PolicyAction,
    /// Pattern matcher for command strings.
    matcher: PolicyMatcher,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Unique identifier for this rule.
    pub id: String,
    /// Pattern to match against the command string.
    pub pattern: CommandPattern,
    /// Action to take when the pattern matches.
    pub action: PolicyAction,
    /// Human-readable reason for this rule.
    pub reason: String,
    /// Priority (higher = evaluated first).
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandPattern {
    /// Exact command match.
    Exact(String),
    /// Glob pattern (e.g., "cargo *").
    Glob(String),
    /// Regular expression.
    Regex(String),
    /// Prefix match.
    Prefix(String),
    /// Match any command containing this substring.
    Contains(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PolicyAction {
    /// Allow the command to execute.
    Allow,
    /// Deny the command; return error.
    Deny,
    /// Require user approval before execution.
    RequireApproval,
}

#[derive(Debug, Clone)]
pub enum PolicyResult {
    Allow,
    Deny(String),
    RequireApproval(String),
}
```

### 6.2 Built-in Default Rules

```rust
impl CommandPolicy {
    /// Create the default policy with sensible built-in rules.
    pub fn default_policy() -> Self {
        let rules = vec![
            // ─── Allowed Commands ────────────────────────────────
            PolicyRule {
                id: "allow-cargo".into(),
                pattern: CommandPattern::Prefix("cargo ".into()),
                action: PolicyAction::Allow,
                reason: "Rust build tools are safe".into(),
                priority: 100,
            },
            PolicyRule {
                id: "allow-npm".into(),
                pattern: CommandPattern::Prefix("npm ".into()),
                action: PolicyAction::Allow,
                reason: "Node.js package manager".into(),
                priority: 100,
            },
            PolicyRule {
                id: "allow-pip".into(),
                pattern: CommandPattern::Prefix("pip ".into()),
                action: PolicyAction::Allow,
                reason: "Python package manager".into(),
                priority: 100,
            },
            PolicyRule {
                id: "allow-make".into(),
                pattern: CommandPattern::Prefix("make ".into()),
                action: PolicyAction::Allow,
                reason: "Build tool".into(),
                priority: 100,
            },
            PolicyRule {
                id: "allow-git".into(),
                pattern: CommandPattern::Prefix("git ".into()),
                action: PolicyAction::Allow,
                reason: "Git operations (xaft manages these)".into(),
                priority: 90,
            },
            PolicyRule {
                id: "allow-test-runners".into(),
                pattern: CommandPattern::Glob("*test*".into()),
                action: PolicyAction::Allow,
                reason: "Test runners are safe".into(),
                priority: 80,
            },

            // ─── Denied Commands ─────────────────────────────────
            PolicyRule {
                id: "deny-rm-rf".into(),
                pattern: CommandPattern::Regex(r"rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+|-r[a-zA-Z]*\s+|.*--recursive.*--force.*)".into()),
                action: PolicyAction::Deny,
                reason: "Destructive filesystem operation".into(),
                priority: 200,
            },
            PolicyRule {
                id: "deny-curl-pipe-sh".into(),
                pattern: CommandPattern::Regex(r"curl\s+.*\|\s*sh".into()),
                action: PolicyAction::Deny,
                reason: "Remote code execution pattern".into(),
                priority: 200,
            },
            PolicyRule {
                id: "deny-wget-pipe-sh".into(),
                pattern: CommandPattern::Regex(r"wget\s+.*\|\s*sh".into()),
                action: PolicyAction::Deny,
                reason: "Remote code execution pattern".into(),
                priority: 200,
            },
            PolicyRule {
                id: "deny-chmod-777".into(),
                pattern: CommandPattern::Regex(r"chmod\s+(777|-R\s+777)".into()),
                action: PolicyAction::Deny,
                reason: "Insecure permissions".into(),
                priority: 200,
            },
            PolicyRule {
                id: "deny-sudo".into(),
                pattern: CommandPattern::Prefix("sudo ".into()),
                action: PolicyAction::RequireApproval,
                reason: "Elevated privileges require approval".into(),
                priority: 190,
            },
            PolicyRule {
                id: "deny-dd".into(),
                pattern: CommandPattern::Prefix("dd ".into()),
                action: PolicyAction::Deny,
                reason: "Raw device access is dangerous".into(),
                priority: 200,
            },
            PolicyRule {
                id: "deny-mkfs".into(),
                pattern: CommandPattern::Prefix("mkfs.".into()),
                action: PolicyAction::Deny,
                reason: "Filesystem formatting is destructive".into(),
                priority: 200,
            },
        ];

        Self {
            rules,
            default_action: PolicyAction::RequireApproval,
            matcher: PolicyMatcher::new(),
        }
    }
}
```

### 6.3 Policy Evaluation

```rust
impl CommandPolicy {
    /// Evaluate a command against all policy rules.
    pub fn evaluate(&self, command: &str) -> Result<PolicyResult, PolicyError> {
        // Sort rules by priority (highest first)
        let mut sorted_rules: Vec<&PolicyRule> = self.rules.iter().collect();
        sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        for rule in sorted_rules {
            if self.matcher.matches(command, &rule.pattern)? {
                return Ok(match &rule.action {
                    PolicyAction::Allow => PolicyResult::Allow,
                    PolicyAction::Deny => PolicyResult::Deny(rule.reason.clone()),
                    PolicyAction::RequireApproval => {
                        PolicyResult::RequireApproval(rule.reason.clone())
                    }
                });
            }
        }

        // Default action
        Ok(match &self.default_action {
            PolicyAction::Allow => PolicyResult::Allow,
            PolicyAction::Deny => PolicyResult::Deny("Default deny policy".into()),
            PolicyAction::RequireApproval => {
                PolicyResult::RequireApproval("No matching allow rule".into())
            }
        })
    }
}
```

---

## 7. Output Capture

### 7.1 Capture Model

```rust
#[derive(Debug, Clone)]
pub struct OutputCaptureConfig {
    /// Maximum bytes to capture from stdout.
    pub max_stdout_bytes: usize,     // default: 1_000_000 (1MB)
    /// Maximum bytes to capture from stderr.
    pub max_stderr_bytes: usize,     // default: 500_000 (500KB)
    /// Whether to capture output in real-time (streaming) or at end.
    pub streaming: bool,             // default: true
    /// Encoding for output (default: UTF-8 with lossy replacement).
    pub encoding: OutputEncoding,    // default: Utf8Lossy
    /// Whether to strip ANSI escape codes from output.
    pub strip_ansi: bool,            // default: true
}

#[derive(Debug, Clone)]
pub enum OutputEncoding {
    Utf8Lossy,
    Utf8Strict,
    Base64,  // for binary output
}

pub struct OutputCapture {
    config: OutputCaptureConfig,
    stdout_buf: SharedBuffer,
    stderr_buf: SharedBuffer,
}

impl OutputCapture {
    /// Capture output from a child process with optional streaming.
    pub async fn capture(
        &mut self,
        child: &mut Child,
    ) -> Result<CapturedOutput, CaptureError> {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_buf = self.stdout_buf.clone();
        let stderr_buf = self.stderr_buf.clone();
        let config = self.config.clone();

        let stdout_handle = tokio::spawn(async move {
            if let Some(mut reader) = stdout {
                let mut buf = Vec::new();
                let mut total_read = 0;
                let mut stream = BufReader::new(reader);

                loop {
                    let mut chunk = [0u8; 8192];
                    match stream.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            total_read += n;
                            if total_read <= config.max_stdout_bytes {
                                buf.extend_from_slice(&chunk[..n]);
                            }
                            // Always stream to shared buffer for real-time display
                            if config.streaming {
                                let _ = stdout_buf.append(&chunk[..n]);
                            }
                        }
                        Err(e) => break,
                    }
                }
                buf
            } else {
                vec![]
            }
        });

        let stderr_handle = tokio::spawn(async move {
            if let Some(mut reader) = stderr {
                let mut buf = Vec::new();
                let mut total_read = 0;
                let mut stream = BufReader::new(reader);

                loop {
                    let mut chunk = [0u8; 8192];
                    match stream.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            total_read += n;
                            if total_read <= config.max_stderr_bytes {
                                buf.extend_from_slice(&chunk[..n]);
                            }
                            if config.streaming {
                                let _ = stderr_buf.append(&chunk[..n]);
                            }
                        }
                        Err(e) => break,
                    }
                }
                buf
            } else {
                vec![]
            }
        });

        let stdout_bytes = stdout_handle.await?;
        let stderr_bytes = stderr_handle.await?;

        let stdout_str = self.decode_output(&stdout_bytes);
        let stderr_str = self.decode_output(&stderr_bytes);

        Ok(CapturedOutput {
            stdout: stdout_str,
            stderr: stderr_str,
        })
    }

    fn decode_output(&self, bytes: &[u8]) -> String {
        match self.config.encoding {
            OutputEncoding::Utf8Lossy => {
                let s = String::from_utf8_lossy(bytes).to_string();
                if self.config.strip_ansi {
                    strip_ansi_escapes::strip_str(&s)
                } else {
                    s
                }
            }
            OutputEncoding::Utf8Strict => {
                String::from_utf8(bytes.to_vec()).unwrap_or_default()
            }
            OutputEncoding::Base64 => {
                base64::encode(bytes)
            }
        }
    }
}
```

---

## 8. Timeout Management

### 8.1 Timeout Hierarchy

```
 ┌─────────────────────────────────────────────────────────────┐
 │                  Timeout Configuration                       │
 │                                                              │
 │  1. Command-level timeout (ShellCommandInput.timeout_secs)  │
 │     - Per-command override                                   │
 │     - Highest priority                                       │
 │                                                              │
 │  2. Step-level timeout (Step.timeout_secs)                   │
 │     - Set during planning                                    │
 │     - Applies to all commands within the step               │
 │                                                              │
 │  3. Global default timeout (ShellCommandConfig)             │
 │     - Default: 120 seconds                                   │
 │     - Lowest priority                                        │
 │                                                              │
 │  Timeout resolution:                                         │
 │  command > step > global                                     │
 │                                                              │
 │  Timeout behavior:                                           │
 │  1. Send SIGTERM (graceful)                                  │
 │  2. Wait grace_period (default: 5s)                         │
 │  3. Send SIGKILL (forceful)                                  │
 │  4. Mark as timed_out                                        │
 └─────────────────────────────────────────────────────────────┘
```

### 8.2 Implementation

```rust
pub struct TimeoutManager {
    default_timeout: Duration,
    grace_period: Duration,    // default: 5s
}

impl TimeoutManager {
    pub async fn run_with_timeout(
        &self,
        child: &mut Child,
        timeout: Duration,
    ) -> Result<ExitStatus, CaptureError> {
        let grace = self.grace_period;

        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(e)) => Err(CaptureError::IoError(e)),
            Err(_) => {
                // Timeout elapsed
                tracing::warn!(
                    "Command timed out after {:?}, sending SIGTERM",
                    timeout
                );

                // Send SIGTERM
                let _ = child.kill().await;

                // Wait for grace period
                match tokio::time::timeout(grace, child.wait()).await {
                    Ok(Ok(status)) => {
                        tracing::info!("Process exited after SIGTERM: {:?}", status);
                        Err(CaptureError::Timeout)
                    }
                    _ => {
                        // Grace period elapsed, send SIGKILL
                        tracing::warn!("Grace period elapsed, sending SIGKILL");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        Err(CaptureError::Timeout)
                    }
                }
            }
        }
    }
}
```

---

## 9. Working Directory and Environment Variables

### 9.1 Working Directory Resolution

```rust
impl ShellCommandTool {
    fn resolve_working_dir(
        &self,
        input: &ShellCommandInput,
    ) -> Result<PathBuf, ToolError> {
        match &input.working_dir {
            Some(dir) => {
                if dir.is_absolute() {
                    // Verify it's within the repo
                    if !dir.starts_with(&self.config.repo_root) {
                        return Err(ToolError::WorkingDirOutsideRepo {
                            path: dir.clone(),
                            repo_root: self.config.repo_root.clone(),
                        });
                    }
                    Ok(dir.clone())
                } else {
                    let resolved = self.config.repo_root.join(dir);
                    if !resolved.exists() {
                        return Err(ToolError::WorkingDirNotFound {
                            path: resolved,
                        });
                    }
                    Ok(resolved)
                }
            }
            None => Ok(self.config.repo_root.clone()),
        }
    }
}
```

### 9.2 Environment Variable Management

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    /// Environment variables to inherit from the parent process.
    pub inherit: Vec<String>,       // default: ["PATH", "HOME", "LANG", "TERM", "SHELL"]

    /// Environment variables to set (overriding inherited values).
    pub set: HashMap<String, String>,

    /// Environment variables to explicitly remove.
    pub remove: Vec<String>,

    /// Whether to pass through all parent environment variables.
    pub pass_through_all: bool,     // default: false

    /// Secrets/tokens that should be masked in output.
    pub masked_vars: Vec<String>,   // default: ["*KEY*", "*TOKEN*", "*SECRET*", "*PASSWORD*"]
}

impl ShellCommandTool {
    fn build_environment(
        &self,
        input: &ShellCommandInput,
    ) -> Result<HashMap<String, String>, ToolError> {
        let mut env = HashMap::new();

        // Step 1: Inherit specified variables
        if self.config.env.pass_through_all {
            for (key, value) in std::env::vars() {
                env.insert(key, value);
            }
        } else {
            for var_name in &self.config.env.inherit {
                if let Ok(value) = std::env::var(var_name) {
                    env.insert(var_name.clone(), value);
                }
            }
        }

        // Step 2: Apply global set overrides
        for (key, value) in &self.config.env.set {
            env.insert(key.clone(), value.clone());
        }

        // Step 3: Apply command-specific env overrides
        if let Some(cmd_env) = &input.env {
            for (key, value) in cmd_env {
                env.insert(key.clone(), value.clone());
            }
        }

        // Step 4: Remove specified variables
        for var_name in &self.config.env.remove {
            env.remove(var_name);
        }

        Ok(env)
    }

    /// Mask sensitive values in command output.
    fn mask_secrets(&self, output: &str) -> String {
        let mut masked = output.to_string();
        for (key, value) in &self.config.env.set {
            if self.is_sensitive_key(key) && !value.is_empty() {
                masked = masked.replace(value, "***REDACTED***");
            }
        }
        masked
    }

    fn is_sensitive_key(&self, key: &str) -> bool {
        let sensitive_patterns = ["KEY", "TOKEN", "SECRET", "PASSWORD", "API_KEY", "PRIVATE"];
        let upper = key.to_uppercase();
        sensitive_patterns.iter().any(|p| upper.contains(p))
    }
}
```

---

## 10. TUI Presentation

### 10.1 Command Execution Panel

```
┌─ Command Execution ──────────────────────────────────────────────────┐
│                                                                      │
│  🔄 Running: cargo test --lib                                        │
│  Working Dir: /tmp/xaft-worktree-e4f5/src                            │
│  Timeout: 120s   |   Sandbox: namespaces   |   PID: 42857            │
│  Duration: 4.2s                                                      │
│                                                                      │
│  ┌─ stdout ──────────────────────────────────────────────────────┐  │
│  │ running 23 tests                                              │  │
│  │ test api::tests::test_login ... ok                            │  │
│  │ test api::tests::test_jwt_validation ... ok                   │  │
│  │ test api::tests::test_middleware ... ok                       │  │
│  │ test middleware::jwt::tests::test_create_token ... ok          │  │
│  │ ... (19 more tests)                                           │  │
│  │                                                                │  │
│  │ test result: ok. 23 passed; 0 failed; 0 ignored               │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  ┌─ stderr ──────────────────────────────────────────────────────┐  │
│  │ (empty)                                                        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  [K] Kill   [P] Pause Output   [S] Skip   [Esc] Close              │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

### 10.2 Policy Violation Display

```
┌─ ⚠️ Policy Violation ────────────────────────────────────────────────┐
│                                                                      │
│  Command: sudo apt-get install libssl-dev                            │
│  Rule:   deny-sudo (priority 190)                                    │
│  Reason: Elevated privileges require approval                        │
│                                                                      │
│  This command requires sudo, which elevates privileges beyond        │
│  the xaft sandbox. Running it could modify system-level state.       │
│                                                                      │
│  [A] Approve Once   [R] Reject   [E] Edit Command   [Q] Quit       │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 11. Configuration

```toml
# .xaft.toml

[shell]
default_timeout_secs = 120
grace_period_secs = 5
max_stdout_bytes = 1000000
max_stderr_bytes = 500000
streaming = true
strip_ansi = true
sandbox_level = "namespaces"      # none | seccomp | namespaces | docker

[shell.sandbox]
allow_network = false
docker_image = "xaft/sandbox:latest"
memory_limit = "1g"
cpu_limit = 2.0
pid_limit = 256
pass_env = ["PATH", "HOME", "LANG", "TERM"]

[shell.env]
pass_through_all = false
inherit = ["PATH", "HOME", "LANG", "TERM", "SHELL"]
masked_vars = ["*KEY*", "*TOKEN*", "*SECRET*", "*PASSWORD*"]

[shell.env.set]
# RUST_BACKTRACE = "1"
# RUST_LOG = "info"

[[shell.policy.rules]]
id = "allow-cargo"
pattern = "cargo *"
pattern_type = "prefix"
action = "allow"
reason = "Rust build tools are safe"
priority = 100

[[shell.policy.rules]]
id = "deny-rm-rf"
pattern = "rm\\s+(-[a-zA-Z]*f[a-zA-Z]*\\s+|-r[a-zA-Z]*\\s+)"
pattern_type = "regex"
action = "deny"
reason = "Destructive filesystem operation"
priority = 200

[shell.policy]
default_action = "require_approval"  # allow | deny | require_approval
```

---

## 12. Error Taxonomy

| Error                                | Code   | Recovery                                    |
|--------------------------------------|--------|---------------------------------------------|
| `ToolError::PolicyViolation`        | C-001  | Edit command or add allow rule              |
| `ToolError::ApprovalRequired`       | C-002  | User approves in TUI                        |
| `CaptureError::Timeout`             | C-003  | Increase timeout or optimize command        |
| `SandboxError::DockerUnavailable`   | C-004  | Use lower sandbox level or install Docker   |
| `SandboxError::SeccompViolation`    | C-005  | Add syscall to allowed list                 |
| `ToolError::WorkingDirOutsideRepo`  | C-006  | Use path within repo root                   |
| `ToolError::WorkingDirNotFound`     | C-007  | Create directory or use existing one        |
| `CaptureError::OutputTruncated`     | C-008  | Increase max_capture_bytes                  |

---

## 13. Future Considerations

1. **Command result caching** — Cache idempotent command results (e.g., `cargo check`) to avoid re-execution.
2. **Parallel command execution** — Allow independent commands within a step to run concurrently.
3. **Interactive command support** — Handle commands that require stdin input (e.g., `git rebase -i`).
4. **Remote execution** — Run commands on remote machines via SSH for distributed testing.
5. **Command recording** — Record all commands and outputs for debugging and audit trails.
