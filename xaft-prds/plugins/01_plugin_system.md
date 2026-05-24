# Plugin System

## Design Goals

The `xaft` plugin system allows teams to extend the tool registry, add domain-specific agents, and integrate with proprietary internal services without modifying the `xaft` core.

## Plugin Trait

```rust
// xaft-plugin/src/plugin.rs
#[async_trait]
pub trait XaftPlugin: Send + Sync {
    /// Unique plugin identifier (e.g., "my-company/db-tools")
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn description(&self) -> &str;

    /// Called once at session startup — register tools, agents, hooks
    async fn initialize(&self, registry: &mut PluginRegistry) -> Result<(), PluginError>;

    /// Called once at session teardown
    async fn shutdown(&self) -> Result<(), PluginError>;

    /// Optional: validate configuration before use
    fn validate_config(&self, config: &toml::Value) -> Result<(), PluginError> { Ok(()) }
}

pub struct PluginRegistry {
    tools: Vec<(String, Arc<ErasedTool>)>,
    agents: Vec<Arc<dyn Agent + Send + Sync>>,
    hooks: Vec<Arc<dyn ToolHook>>,
    signal_handlers: Vec<Box<dyn FnMut(&dyn Any) + Send>>,
}

impl PluginRegistry {
    pub fn register_tool(&mut self, name: &str, tool: Arc<ErasedTool>) {
        self.tools.push((name.to_string(), tool));
    }

    pub fn register_global_hook(&mut self, hook: Arc<dyn ToolHook>) {
        self.hooks.push(hook);
    }

    pub fn register_agent(&mut self, agent: Arc<dyn Agent + Send + Sync>) {
        self.agents.push(agent);
    }
}
```

## Plugin Loading

Plugins load in order:
1. Built-in plugins (always loaded)
2. User plugins from `~/.config/xaft/plugins/`
3. Project plugins from `.xaft/plugins/`

```toml
# .xaft/config.toml
[[plugins]]
id = "my-company/db-tools"
path = ".xaft/plugins/db-tools"    # local plugin
config = { connection_string = "postgres://localhost/mydb" }

[[plugins]]
id = "xaft-github"
version = "^1.0"                    # from plugin registry (future)
config = { token_env = "GITHUB_TOKEN" }
```

## Native Rust Plugin (dylib)

For maximum performance, plugins compile to a shared library:

```rust
// my_plugin/src/lib.rs
use xaft_plugin::prelude::*;

pub struct MyDbPlugin {
    pool: sqlx::PgPool,
}

#[async_trait]
impl XaftPlugin for MyDbPlugin {
    fn id(&self) -> &str { "my-company/db-tools" }
    fn version(&self) -> &str { "1.0.0" }
    fn description(&self) -> &str { "Database inspection tools for PostgreSQL" }

    async fn initialize(&self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
        registry.register_tool("query_db", Arc::new(QueryDbTool::new(self.pool.clone())));
        registry.register_tool("describe_table", Arc::new(DescribeTableTool::new(self.pool.clone())));
        registry.register_tool("list_tables", Arc::new(ListTablesTool::new(self.pool.clone())));
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        self.pool.close().await;
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn xaft_plugin_create(config: *const c_char) -> *mut dyn XaftPlugin {
    let config_str = unsafe { CStr::from_ptr(config).to_str().unwrap_or("{}") };
    let config: PluginConfig = serde_json::from_str(config_str).unwrap_or_default();
    Box::into_raw(Box::new(MyDbPlugin::new(config)))
}
```

## Script Plugin (subprocess)

Simpler plugins run as subprocesses and communicate via stdin/stdout JSON protocol:

```bash
#!/usr/bin/env python3
# .xaft/plugins/my-script-plugin/plugin.py
# Protocol: JSON-RPC 2.0 over stdin/stdout

import json, sys

def handle_tool_call(name, input):
    if name == "my_tool":
        return {"result": f"processed: {input['query']}"}
    raise ValueError(f"unknown tool: {name}")

for line in sys.stdin:
    req = json.loads(line)
    if req["method"] == "list_tools":
        print(json.dumps({"result": [{"name": "my_tool", "description": "..."}]}))
    elif req["method"] == "call_tool":
        result = handle_tool_call(req["params"]["name"], req["params"]["input"])
        print(json.dumps({"result": result}))
    sys.stdout.flush()
```

## Built-in Plugin Registry

| Plugin ID | Tools provided | Status |
|---|---|---|
| `xaft-core-tools` | `read_file`, `write_file`, `list_files`, `search_files`, `apply_patch` | Built-in |
| `xaft-git` | `git_status`, `git_diff`, `git_commit`, `git_log`, `git_push` | Built-in |
| `xaft-shell` | `run_cargo`, `run_command` | Built-in |
| `xaft-index` | `search_code`, `find_symbol`, `get_deps` | Built-in |
| `xaft-github` | `create_pr`, `list_issues`, `get_review` | Optional |
| `xaft-linear` | `create_ticket`, `get_tickets` | Optional |
| `xaft-jira` | `create_issue`, `update_issue` | Optional |

## References

- agtrs: `agtrs-runtime/src/tool.rs` (Tool trait)
- Next: [MCP Compatibility →](02_mcp_compatibility.md)
