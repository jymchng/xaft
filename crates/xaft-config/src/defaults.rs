//! Default implementations for all configuration types.

use std::collections::HashMap;

use crate::types::*;

impl Default for XaftConfig {
    fn default() -> Self {
        let mut agent = HashMap::new();
        agent.insert("default".to_string(), AgentPreset::default());
        agent.insert(
            "code-review".to_string(),
            AgentPreset {
                model: "claude-3-5-sonnet-20241022".to_string(),
                provider: "anthropic".to_string(),
                system_prompt: "You are a code review agent. Focus on security vulnerabilities, \
                                performance issues, and code style consistency."
                    .to_string(),
                max_turns: 15,
                temperature: 0.2,
                allowed_tools: vec!["file-read".to_string(), "grep".to_string()],
                denied_tools: vec!["shell".to_string()],
                ..Default::default()
            },
        );
        agent.insert(
            "refactor".to_string(),
            AgentPreset {
                model: "claude-3-5-sonnet-20241022".to_string(),
                provider: "anthropic".to_string(),
                max_turns: 40,
                denied_tools: vec!["shell".to_string()],
                ..Default::default()
            },
        );
        agent.insert(
            "debug".to_string(),
            AgentPreset {
                model: "claude-3-5-sonnet-20241022".to_string(),
                provider: "anthropic".to_string(),
                max_turns: 50,
                temperature: 0.3,
                ..Default::default()
            },
        );
        agent.insert(
            "docs".to_string(),
            AgentPreset {
                model: "claude-3-5-sonnet-20241022".to_string(),
                provider: "anthropic".to_string(),
                max_turns: 20,
                temperature: 0.5,
                allowed_tools: vec!["file-read".to_string(), "file-edit".to_string()],
                ..Default::default()
            },
        );

        let mut provider = HashMap::new();
        provider.insert(
            "anthropic".to_string(),
            ProviderConfig {
                provider_type: ProviderType::Anthropic,
                base_url: "https://api.anthropic.com".to_string(),
                max_retries: 3,
                timeout_secs: 120,
                ..Default::default()
            },
        );
        provider.insert(
            "openai".to_string(),
            ProviderConfig {
                provider_type: ProviderType::Openai,
                base_url: "https://api.openai.com/v1".to_string(),
                max_retries: 3,
                timeout_secs: 120,
                ..Default::default()
            },
        );

        let mut tool = HashMap::new();
        tool.insert(
            "file-read".to_string(),
            ToolConfig {
                enabled: true,
                extra: default_file_read_extra(),
            },
        );
        tool.insert(
            "file-edit".to_string(),
            ToolConfig {
                enabled: true,
                extra: default_file_edit_extra(),
            },
        );
        tool.insert(
            "shell".to_string(),
            ToolConfig {
                enabled: true,
                extra: default_shell_extra(),
            },
        );
        tool.insert(
            "grep".to_string(),
            ToolConfig {
                enabled: true,
                extra: default_grep_extra(),
            },
        );

        Self {
            core: CoreConfig::default(),
            agent,
            provider,
            tool,
            guardrail: GuardrailConfig::default(),
            mcp: McpConfig::default(),
            tui: TuiConfig::default(),
            plugins: PluginConfig::default(),
            model_tiers: ModelTierConfig::default(),
            memory: MemoryConfig::default(),
        }
    }
}

fn default_file_read_extra() -> HashMap<String, serde_json::Value> {
    let mut m = HashMap::new();
    m.insert("max_file_size".to_string(), serde_json::json!("10MB"));
    m.insert("max_line_length".to_string(), serde_json::json!(2000u64));
    m
}

fn default_file_edit_extra() -> HashMap<String, serde_json::Value> {
    let mut m = HashMap::new();
    m.insert("confirm_on_write".to_string(), serde_json::json!(false));
    m.insert("max_concurrent_edits".to_string(), serde_json::json!(5u64));
    m
}

fn default_shell_extra() -> HashMap<String, serde_json::Value> {
    let mut m = HashMap::new();
    m.insert("shell".to_string(), serde_json::json!("/bin/bash"));
    m.insert("timeout_secs".to_string(), serde_json::json!(300u64));
    m.insert(
        "blocked_commands".to_string(),
        serde_json::json!(["rm -rf /", "mkfs", "dd if=/dev/zero"]),
    );
    m.insert("max_output_bytes".to_string(), serde_json::json!(65536u64));
    m
}

fn default_grep_extra() -> HashMap<String, serde_json::Value> {
    let mut m = HashMap::new();
    m.insert("engine".to_string(), serde_json::json!("ripgrep"));
    m.insert("max_results".to_string(), serde_json::json!(100u64));
    m
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            data_dir: default_data_dir(),
            telemetry: true,
        }
    }
}

/// Return the default xaft data directory (`~/.xaft`).
pub fn default_data_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".xaft")
}

impl Default for AgentPreset {
    fn default() -> Self {
        Self {
            model: "claude-3-5-sonnet-20241022".to_string(),
            provider: "anthropic".to_string(),
            system_prompt: String::new(),
            max_turns: 25,
            temperature: 0.0,
            top_p: 1.0,
            stop_sequences: Vec::new(),
            allowed_tools: vec!["*".to_string()],
            denied_tools: Vec::new(),
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: ProviderType::Anthropic,
            api_key: String::new(),
            api_key_env: None,
            base_url: String::new(),
            organization: String::new(),
            max_retries: 3,
            timeout_secs: 120,
            headers: HashMap::new(),
            rpm_limit: None,
            tpm_limit: None,
            models: HashMap::new(),
        }
    }
}

impl Default for ShellToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shell: "/bin/bash".to_string(),
            timeout_secs: 300,
            blocked_commands: vec![
                "rm -rf /".to_string(),
                "mkfs".to_string(),
                "dd if=/dev/zero".to_string(),
            ],
            max_output_bytes: 65536,
        }
    }
}

impl Default for FileReadToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_size: "10MB".to_string(),
            max_line_length: 2000,
        }
    }
}

impl Default for FileEditToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            confirm_on_write: false,
            max_concurrent_edits: 5,
        }
    }
}

impl Default for GrepToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            engine: "ripgrep".to_string(),
            max_results: 100,
        }
    }
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            file_destruction: true,
            secret_leakage: true,
            cost_limit: true,
            command_approval: false,
            cost_limit_config: CostLimitConfig::default(),
            secret_leakage_config: SecretLeakageConfig::default(),
        }
    }
}

impl Default for CostLimitConfig {
    fn default() -> Self {
        Self {
            max_spend: 10.0,
            max_tokens_per_request: 100_000,
            warn_at_percent: 80,
        }
    }
}

impl Default for SecretLeakageConfig {
    fn default() -> Self {
        Self {
            patterns: vec![
                r"sk-[a-zA-Z0-9]{48}".to_string(),
                r"sk-ant-[a-zA-Z0-9-]{95}".to_string(),
                r"ghp_[a-zA-Z0-9]{36}".to_string(),
                r"AKIA[0-9A-Z]{16}".to_string(),
            ],
            action: SecretAction::Redact,
        }
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: "http+sse".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3001,
            tools: McpToolFilter::default(),
        }
    }
}

impl Default for McpToolFilter {
    fn default() -> Self {
        Self {
            include: vec!["*".to_string()],
            exclude: Vec::new(),
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: TuiTheme::Dark,
            mouse: false,
            timestamps: true,
            conversation_height: 40,
            keybindings: default_keybindings(),
            layout: TuiLayoutConfig::default(),
            preserve_output_on_exit: true,
            use_alternate_screen: true,
            persist_final_frame: true,
            show_exit_summary: true,
        }
    }
}

fn default_keybindings() -> KeybindingConfig {
    let mut bindings = HashMap::new();

    // Navigation
    bindings.insert(
        "ctrl+n".to_string(),
        KeyAction::Single("new_task".to_string()),
    );
    bindings.insert("ctrl+q".to_string(), KeyAction::Single("quit".to_string()));
    bindings.insert(
        "ctrl+s".to_string(),
        KeyAction::Single("stop_agent".to_string()),
    );
    bindings.insert(
        "ctrl+r".to_string(),
        KeyAction::Single("resume_agent".to_string()),
    );
    bindings.insert(
        "ctrl+p".to_string(),
        KeyAction::Single("command_palette".to_string()),
    );

    // Panels
    bindings.insert(
        "ctrl+1".to_string(),
        KeyAction::Single("focus_conversation".to_string()),
    );
    bindings.insert(
        "ctrl+2".to_string(),
        KeyAction::Single("focus_task_tree".to_string()),
    );
    bindings.insert(
        "ctrl+3".to_string(),
        KeyAction::Single("focus_file_diff".to_string()),
    );
    bindings.insert(
        "ctrl+4".to_string(),
        KeyAction::Single("focus_tools".to_string()),
    );

    // Scrolling
    bindings.insert(
        "ctrl+up".to_string(),
        KeyAction::Single("scroll_up".to_string()),
    );
    bindings.insert(
        "ctrl+down".to_string(),
        KeyAction::Single("scroll_down".to_string()),
    );
    bindings.insert(
        "pageup".to_string(),
        KeyAction::Single("page_up".to_string()),
    );
    bindings.insert(
        "pagedown".to_string(),
        KeyAction::Single("page_down".to_string()),
    );

    // Agent interaction
    bindings.insert(
        "ctrl+space".to_string(),
        KeyAction::Single("interrupt_agent".to_string()),
    );
    bindings.insert(
        "ctrl+enter".to_string(),
        KeyAction::Single("submit_input".to_string()),
    );
    bindings.insert(
        "alt+enter".to_string(),
        KeyAction::Single("newline_in_input".to_string()),
    );
    bindings.insert(
        "shift+enter".to_string(),
        KeyAction::Single("newline_in_input".to_string()),
    );
    bindings.insert(
        "ctrl+j".to_string(),
        KeyAction::Single("newline_in_input".to_string()),
    );

    KeybindingConfig { bindings }
}

impl Default for TuiLayoutConfig {
    fn default() -> Self {
        Self {
            conversation_width: 60,
            sidebar_width: 40,
            sidebar_panels: vec![
                SidebarPanel::TaskTree,
                SidebarPanel::FileDiff,
                SidebarPanel::Tools,
            ],
            file_diff_height: 15,
        }
    }
}

impl Default for TuiLayoutState {
    fn default() -> Self {
        let layout = TuiLayoutConfig::default();
        Self {
            conversation_width: layout.conversation_width,
            sidebar_width: layout.sidebar_width,
            sidebar_panels: layout.sidebar_panels,
            file_diff_height: layout.file_diff_height,
            scroll_positions: HashMap::new(),
            focused_panel: FocusedPanel::Conversation,
            input_height: 3,
        }
    }
}

impl Default for PluginSecurityConfig {
    fn default() -> Self {
        Self {
            require_signature: false,
            unsigned_capabilities: vec![
                "fs_read".to_string(),
                "fs_write".to_string(),
                "shell".to_string(),
            ],
        }
    }
}
