# XAFT MCP Compatibility — Product Requirements Document

> **Status**: Draft v0.1  
> **Last Updated**: 2025-03-04  
> **Authors**: xaft core team  
> **Scope**: Model Context Protocol interoperability, MCP server mode, MCP client mode, transport layers, tool schema mapping, AgentMessageBus bridging, bidirectional tool flow

---

## 1. Overview

The [Model Context Protocol](https://spec.modelcontextprotocol.io/) (MCP) is an open standard for connecting AI agents to external tools and data sources. xaft must interoperate with MCP in two directions:

1. **Server mode**: Expose xaft's built-in and plugin tools to any MCP-compatible client (e.g., Claude Desktop, Cursor, Continue).
2. **Client mode**: Consume tools from external MCP servers, making them available to xaft agents as if they were native plugins.

This bidirectional bridge makes xaft both a tool consumer and a tool provider in the broader agent ecosystem.

### 1.1 Goals

| # | Goal | Metric |
|---|------|--------|
| G1 | Full MCP specification compliance (2025-03 spec) | Pass all MCP spec test cases |
| G2 | Zero-overhead tool invocation through bridge | <1 ms overhead vs. native plugin call |
| G3 | Bidirectional streaming support | SSE + stdio transports operational |
| G4 | Automatic schema translation between xaft Tool and MCP Tool | 100% of primitive types; 90%+ of complex types |
| G5 | Graceful degradation on transport failure | Retry with backoff; agent continues with reduced tool set |

### 1.2 Non-Goals

- Building a general-purpose MCP orchestration platform
- Supporting non-standard MCP extensions from specific clients
- Persisting MCP server connections across xaft restarts (session-level only in v1)
- MCP sampling (letting MCP servers call LLMs through xaft) — deferred to v2

---

## 2. Architecture

### 2.1 Dual-Mode Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            XAFT PROCESS                                 │
│                                                                         │
│  ┌─────────────────────────┐    ┌──────────────────────────────────┐   │
│  │    MCP SERVER MODULE    │    │      MCP CLIENT MODULE           │   │
│  │                         │    │                                  │   │
│  │  Exposes xaft tools     │    │  Consumes external MCP tools     │   │
│  │  to MCP clients:        │    │  from MCP servers:               │   │
│  │  - Claude Desktop       │    │  - filesystem MCP server         │   │
│  │  - Cursor               │    │  - GitHub MCP server             │   │
│  │  - Continue             │    │  - Custom corporate servers      │   │
│  │  - Any MCP client       │    │  - Database MCP servers          │   │
│  └───────────┬─────────────┘    └──────────────┬───────────────────┘   │
│              │                                  │                       │
│              ▼                                  ▼                       │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                     AgentMessageBus                              │   │
│  │  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌───────────┐ │   │
│  │  │ Tool Call  │  │ Tool Call  │  │ Tool Call  │  │ Tool Call │ │   │
│  │  │ Channel    │  │ Channel    │  │ Channel    │  │ Channel   │ │   │
│  │  │ (native)   │  │ (MCP-srv)  │  │ (MCP-cli)  │  │ (plugin)  │ │   │
│  │  └────────────┘  └────────────┘  └────────────┘  └───────────┘ │   │
│  └──────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Data Flow: Server Mode (xaft → MCP Client)

```
  MCP Client                 XAFT MCP Server              xaft Tool
  ──────────                 ─────────────────             ─────────
       │                           │                          │
       │  initialize               │                          │
       │──────────────────────────►│                          │
       │  capabilities             │                          │
       │◄──────────────────────────│                          │
       │                           │                          │
       │  tools/list               │                          │
       │──────────────────────────►│  PluginRegistry::tools() │
       │                           │─────────────────────────►│
       │  [tool schemas]           │  schemas mapped to MCP   │
       │◄──────────────────────────│◄─────────────────────────│
       │                           │                          │
       │  tools/call (id, args)    │                          │
       │──────────────────────────►│  AgentMessageBus::dispatch│
       │                           │─────────────────────────►│
       │                           │  Tool::execute()          │
       │  tool result              │◄─────────────────────────│
       │◄──────────────────────────│                          │
```

### 2.3 Data Flow: Client Mode (MCP Server → xaft Agent)

```
  xaft Agent               XAFT MCP Client              MCP Server
  ──────────               ─────────────────             ──────────
       │                          │                           │
       │  Tool::execute()         │                           │
       │─────────────────────────►│  tools/call (id, args)    │
       │                          │──────────────────────────►│
       │                          │  tool result              │
       │  ToolOutput              │◄──────────────────────────│
       │◄─────────────────────────│                           │
```

---

## 3. Transport Layer

### 3.1 Supported Transports

| Transport | Direction | Use Case | Implementation |
|-----------|-----------|----------|----------------|
| `stdio` | Server + Client | Local process communication | JSON-RPC over stdin/stdout |
| `HTTP+SSE` | Server | Remote access, web clients | Axum server + SSE stream |
| `StreamableHTTP` | Client | Modern MCP servers | POST + SSE response |

### 3.2 Transport Abstraction

```rust
#[async_trait]
pub trait McpTransport: Send + Sync + 'static {
    /// Send a JSON-RPC request and await a response.
    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, TransportError>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), TransportError>;

    /// Open a bidirectional stream for streaming results.
    async fn stream(&self, method: &str, params: serde_json::Value) -> Result<Box<dyn McpStream>, TransportError>;

    /// Close the transport gracefully.
    async fn close(&self) -> Result<(), TransportError>;
}

pub trait McpStream: Send + Sync {
    /// Poll the next chunk from the stream.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<serde_json::Value>>;
}
```

### 3.3 Stdio Transport

```rust
pub struct StdioTransport {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pending: HashMap<JsonRpcId, oneshot::Sender<serde_json::Value>>,
    next_id: AtomicU64,
}

impl StdioTransport {
    pub fn spawn(server_cmd: &str, args: &[&str]) -> Result<Self, TransportError> {
        let mut child = Command::new(server_cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().ok_or(TransportError::NoStdin)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(TransportError::NoStdout)?);

        Ok(Self {
            stdin,
            stdout,
            pending: HashMap::new(),
            next_id: AtomicU64::new(1),
        })
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, TransportError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method: method.to_string(),
            params,
        };

        // Write to child stdin
        let mut serialized = serde_json::to_vec(&msg)?;
        serialized.push(b'\n');
        self.stdin.write_all(&serialized).await?;

        // Read response line (simplified; real impl uses a reader task)
        let mut line = String::new();
        self.stdout.read_line(&mut line).await?;
        let resp: JsonRpcResponse = serde_json::from_str(&line)?;
        resp.result.ok_or(TransportError::JsonRpc(resp.error.unwrap()))
    }
}
```

### 3.4 HTTP+SSE Transport (Server Mode)

```rust
pub struct HttpSseTransport {
    addr: SocketAddr,
    connections: Arc<Mutex<Vec<SseConnection>>>,
    router: Router,
}

impl HttpSseTransport {
    pub fn new(addr: SocketAddr, handler: Arc<dyn McpRequestHandler>) -> Self {
        let connections = Arc::new(Mutex::new(Vec::new()));

        let app = Router::new()
            .route("/sse", get(sse_handler))
            .route("/message", post(message_handler))
            .with_state(AppState {
                handler,
                connections: connections.clone(),
            });

        Self { addr, connections, router: app }
    }

    pub async fn serve(&self) -> Result<(), TransportError> {
        let listener = TcpListener::bind(self.addr).await?;
        axum::serve(listener, self.router.clone()).await?;
        Ok(())
    }
}

async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel(32);

    // Send the endpoint URL as the first event
    let endpoint = format!("http://{}/message", state.addr);
    tx.send(Ok(Event::default().event("endpoint").data(&endpoint))).await.ok();

    state.connections.lock().await.push(SseConnection { tx });

    Sse::new(ReceiverStream::new(rx))
}
```

---

## 4. Tool Schema Mapping

### 4.1 xaft → MCP Schema Translation

```
┌────────────────────────┐          ┌──────────────────────────┐
│   xaft Tool Schema     │          │   MCP Tool Schema        │
│   (JSON Schema draft7) │          │   (JSON Schema draft7)   │
│                        │          │                          │
│   input_schema ────────┼─────────►│ inputSchema              │
│   output_schema        │   (1:1)  │                          │
│   description ─────────┼─────────►│ description              │
│   id ──────────────────┼─────────►│ name  (dot→underscore)  │
│                        │          │ annotations:             │
│   capabilities ────────┼─────────►│   x-xaft-capabilities   │
│   version ─────────────┼─────────►│   x-xaft-version        │
└────────────────────────┘          └──────────────────────────┘
```

### 4.2 Schema Mapper Implementation

```rust
pub struct SchemaMapper;

impl SchemaMapper {
    /// Convert a xaft Tool into an MCP Tool definition.
    pub fn xaft_to_mcp(tool: &dyn Tool) -> McpTool {
        McpTool {
            name: tool.id().replace('.', "_"), // com.example.my_tool → com_example_my_tool
            description: Some(tool.description().to_string()),
            input_schema: Self::adapt_input_schema(tool.input_schema()),
            annotations: Some(ToolAnnotations {
                title: Some(tool.name().to_string()),
                read_only_hint: Self::is_read_only(tool.capabilities()),
                destructive_hint: Self::is_destructive(tool.capabilities()),
                idempotent_hint: Self::is_idempotent(tool),
                open_world_hint: Some(tool.capabilities().iter().any(|c| matches!(c, Capability::Network))),
                extra: {
                    let mut map = serde_json::Map::new();
                    map.insert("x-xaft-id".into(), tool.id().into());
                    map.insert("x-xaft-version".into(), tool.version().to_string().into());
                    map
                },
            }),
        }
    }

    /// Convert an MCP Tool into a xaft Tool trait object.
    pub fn mcp_to_xaft(mcp_tool: McpTool, transport: Arc<dyn McpTransport>) -> Box<dyn Tool> {
        Box::new(McpToolAdapter {
            name: mcp_tool.name.clone(),
            description: mcp_tool.description.unwrap_or_default(),
            input_schema: mcp_tool.input_schema,
            transport,
        })
    }

    fn adapt_input_schema(xaft_schema: &serde_json::Value) -> serde_json::Value {
        // xaft uses absolute file paths; MCP expects URIs in some cases
        let mut adapted = xaft_schema.clone();
        if let Some(props) = adapted.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (_key, schema) in props.iter_mut() {
                if let Some(desc) = schema.get_mut("description") {
                    // Append xaft-specific hints
                    if let Some(s) = desc.as_str() {
                        *desc = serde_json::Value::String(format!("{} (xaft-native)", s));
                    }
                }
            }
        }
        adapted
    }
}
```

### 4.3 Type Compatibility Matrix

| xaft Type | MCP Type | Mapping | Notes |
|-----------|----------|---------|-------|
| `string` | `string` | 1:1 | Direct |
| `number` | `number` | 1:1 | Direct |
| `integer` | `integer` | 1:1 | Direct |
| `boolean` | `boolean` | 1:1 | Direct |
| `array` | `array` | 1:1 | Direct |
| `object` | `object` | 1:1 | Direct |
| `path` (xaft extension) | `string` + `format: uri` | Lossy | File URI encoding |
| `enum` | `enum` | 1:1 | Direct |
| `oneOf` | `oneOf` | 1:1 | Direct |
| `xAftFileContent` | `string` + `contentEncoding: base64` | Lossy | Binary→base64 |

---

## 5. AgentMessageBus Bridging

### 5.1 Bus Architecture

The `AgentMessageBus` is xaft's internal pub/sub system for inter-component communication. MCP integration requires bridging MCP tool calls to/from the bus.

```
┌──────────────┐     ┌───────────────────┐     ┌──────────────┐
│  xaft Agent  │────►│ AgentMessageBus   │◄────│ MCP Client   │
│              │     │                   │     │ Adapter      │
│  (native     │     │  Topics:          │     │              │
│   tool calls)│     │  - tool.call      │     │  Consumes:   │
│              │     │  - tool.result    │     │  tool.call   │
│              │     │  - agent.message  │     │  Produces:   │
│              │     │  - system.event   │     │  tool.result │
└──────────────┘     └───────────────────┘     └──────────────┘
                            │
                            │  tool.call topic
                            ▼
                     ┌──────────────┐
                     │ MCP Server   │
                     │ Adapter      │
                     │              │
                     │  Consumes:   │
                     │  tool.result │
                     │  Produces:   │
                     │  tool.call   │
                     └──────────────┘
                            │
                            ▼
                     MCP Clients (external)
```

### 5.2 Bridge Implementation

```rust
pub struct McpBusBridge {
    bus: Arc<AgentMessageBus>,
    registry: Arc<PluginRegistry>,
    transport: Arc<dyn McpTransport>,
}

impl McpBusBridge {
    /// Start the bridge: subscribe to bus events and forward to MCP.
    pub async fn start(&self) -> Result<(), BridgeError> {
        // Subscribe to tool calls from the bus (for server mode)
        let mut tool_call_rx = self.bus.subscribe::<ToolCallMessage>("tool.call");

        // Subscribe to tool results (for client mode)
        let mut tool_result_rx = self.bus.subscribe::<ToolResultMessage>("tool.result");

        loop {
            tokio::select! {
                // Server mode: xaft agent calls a tool that an MCP client exposed
                Some(msg) = tool_call_rx.recv() => {
                    if self.is_mcp_tool(&msg.tool_id) {
                        self.forward_to_mcp_server(msg).await?;
                    }
                }

                // Client mode: MCP client calls a tool that xaft exposes
                Some(result) = tool_result_rx.recv() => {
                    self.forward_result_to_mcp_client(result).await?;
                }

                // Handle incoming MCP requests (server mode)
                Some(incoming) = self.poll_mcp_incoming() => {
                    self.dispatch_mcp_request(incoming).await?;
                }
            }
        }
    }

    async fn forward_to_mcp_server(&self, msg: ToolCallMessage) -> Result<(), BridgeError> {
        let mcp_call = McpToolCall {
            name: msg.tool_id.replace('.', "_"),
            arguments: msg.arguments,
        };

        let result = self.transport
            .request("tools/call", serde_json::to_value(mcp_call)?)
            .await?;

        let tool_output = Self::mcp_result_to_xaft(result)?;
        self.bus.publish("tool.result", ToolResultMessage {
            call_id: msg.call_id,
            output: tool_output,
        }).await;

        Ok(())
    }
}
```

### 5.3 Message Envelope

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallMessage {
    pub call_id: Uuid,
    pub tool_id: String,           // "com.example.file-read"
    pub arguments: serde_json::Value,
    pub agent_id: Option<String>,
    pub session_id: SessionId,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub call_id: Uuid,
    pub output: ToolOutput,
    pub timestamp: DateTime<Utc>,
}
```

---

## 6. MCP Server Mode

### 6.1 Server Configuration

```toml
# In xaft.toml
[mcp.server]
enabled = true
transport = "http+sse"           # "stdio" | "http+sse"
host = "127.0.0.1"
port = 3001

# Which xaft tools to expose
[mcp.server.tools]
include = ["*"]                   # Glob patterns for tool IDs
exclude = ["com.xaft.internal.*"] # Never expose internal tools

# Which agent capabilities to advertise
[mcp.server.capabilities]
tools = true
resources = false                 # Future: expose file resources
prompts = false                   # Future: expose agent prompts
logging = true                    # Allow MCP clients to read xaft logs
sampling = false                  # Not supported in v1
```

### 6.2 Server Request Handler

```rust
pub struct XaftMcpServerHandler {
    registry: Arc<PluginRegistry>,
    bus: Arc<AgentMessageBus>,
    config: McpServerConfig,
}

#[async_trait]
impl McpRequestHandler for XaftMcpServerHandler {
    async fn handle_initialize(&self, params: InitializeParams) -> Result<InitializeResult, McpError> {
        Ok(InitializeResult {
            protocol_version: "2025-03-26".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: Some(true) }),
                logging: Some(LoggingCapability {}),
                resources: None,
                prompts: None,
                sampling: None,
            },
            server_info: Implementation {
                name: "xaft-mcp-server".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        })
    }

    async fn handle_tools_list(&self, _params: Option<serde_json::Value>) -> Result<ListToolsResult, McpError> {
        let tools: Vec<McpTool> = self.registry
            .iter_tools()
            .filter(|t| self.config.should_expose(t.id()))
            .map(|t| SchemaMapper::xaft_to_mcp(t.as_ref()))
            .collect();

        Ok(ListToolsResult { tools })
    }

    async fn handle_tools_call(&self, params: CallToolParams) -> Result<CallToolResult, McpError> {
        let tool_id = params.name.replace('_', "."); // Reverse the name mapping

        let tool = self.registry
            .get_tool(&tool_id)
            .ok_or(McpError::invalid_params(format!("unknown tool: {}", params.name)))?;

        let call = ToolCall {
            id: Uuid::new_v4().to_string(),
            arguments: params.arguments,
        };

        let ctx = self.create_tool_context()?;

        match tool.execute(call, &ctx).await {
            Ok(output) => Ok(CallToolResult {
                content: vec![Content::Text { text: output.to_string() }],
                is_error: Some(false),
            }),
            Err(e) => Ok(CallToolResult {
                content: vec![Content::Text { text: format!("Error: {}", e) }],
                is_error: Some(true),
            }),
        }
    }
}
```

### 6.3 Tool List Change Notifications

When plugins are loaded/unloaded at runtime (future), the server must notify connected clients:

```rust
impl XaftMcpServerHandler {
    pub async fn notify_tools_changed(&self) -> Result<(), McpError> {
        let connections = self.connections.lock().await;
        for conn in connections.iter() {
            conn.send_event(Event::default()
                .event("notification")
                .data(&serde_json::json!({
                    "method": "notifications/tools/list_changed",
                    "params": {}
                }))
            ).await.ok();
        }
        Ok(())
    }
}
```

---

## 7. MCP Client Mode

### 7.1 Client Configuration

```toml
# In xaft.toml
[[mcp.client]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
enabled = true

[[mcp.client]]
name = "github"
transport = "http+sse"
url = "http://localhost:3002/sse"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
enabled = true

[[mcp.client]]
name = "database"
transport = "stdio"
command = "./mcp-db-server"
args = ["--config", "db.yaml"]
capabilities_filter = ["tools"]  # Only consume tools, not resources
```

### 7.2 Client Manager

```rust
pub struct McpClientManager {
    clients: HashMap<String, McpClientEntry>,
    bus: Arc<AgentMessageBus>,
}

struct McpClientEntry {
    name: String,
    transport: Arc<dyn McpTransport>,
    tools: Vec<McpTool>,
    capabilities: ServerCapabilities,
    status: ClientStatus,
}

enum ClientStatus {
    Connecting,
    Connected,
    Disconnected { reason: String },
    Retrying { attempt: u32, next: Instant },
}

impl McpClientManager {
    /// Discover all configured MCP servers and connect.
    pub async fn discover(configs: &[McpClientConfig], bus: Arc<AgentMessageBus>) -> Result<Self, ClientError> {
        let mut clients = HashMap::new();

        for cfg in configs {
            if !cfg.enabled { continue; }

            let transport = match cfg.transport.as_str() {
                "stdio" => {
                    let t = StdioTransport::spawn(&cfg.command, &cfg.args)?;
                    Arc::new(t) as Arc<dyn McpTransport>
                }
                "http+sse" => {
                    let t = HttpSseClientTransport::new(&cfg.url, cfg.headers.clone()).await?;
                    Arc::new(t) as Arc<dyn McpTransport>
                }
                _ => return Err(ClientError::UnknownTransport(cfg.transport.clone())),
            };

            // Perform MCP initialize handshake
            let init_result = transport.request("initialize", serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "xaft", "version": env!("CARGO_PKG_VERSION") }
            })).await?;

            // Fetch tool list
            let tools_result = transport.request("tools/list", serde_json::Value::Null).await?;
            let tools: Vec<McpTool> = serde_json::from_value(tools_result)?;

            clients.insert(cfg.name.clone(), McpClientEntry {
                name: cfg.name.clone(),
                transport,
                tools,
                capabilities: init_result.capabilities,
                status: ClientStatus::Connected,
            });
        }

        Ok(Self { clients, bus })
    }

    /// Register all MCP tools as xaft Tool plugins.
    pub fn register_with(&self, registry: &mut PluginRegistry) -> Result<(), ClientError> {
        for (_name, entry) in &self.clients {
            for mcp_tool in &entry.tools {
                let xaft_tool = SchemaMapper::mcp_to_xaft(
                    mcp_tool.clone(),
                    entry.transport.clone(),
                );
                registry.register(xaft_tool, Source::McpClient { server: entry.name.clone() })?;
            }
        }
        Ok(())
    }
}
```

### 7.3 MCP Tool Adapter

Wraps an MCP tool as a xaft `Tool` trait object:

```rust
pub struct McpToolAdapter {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    transport: Arc<dyn McpTransport>,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn id(&self) -> &str { &self.name }
    fn name(&self) -> &str { &self.name }
    fn version(&self) -> &Version { &VERSION }
    fn capabilities(&self) -> &[Capability] { &[] }
    fn kinds(&self) -> &[PluginKind] { &[PluginKind::Tool] }

    fn input_schema(&self) -> &serde_json::Value { &self.input_schema }
    fn output_schema(&self) -> &serde_json::Value { &OUTPUT_SCHEMA }
    fn description(&self) -> &str { &self.description }

    async fn execute(&self, call: ToolCall, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let params = CallToolParams {
            name: self.name.clone(),
            arguments: call.arguments,
        };

        let result = self.transport
            .request("tools/call", serde_json::to_value(params).map_err(|e| ToolError::Fatal { message: e.to_string() })?)
            .await
            .map_err(|e| ToolError::Recoverable {
                message: format!("MCP transport error: {}", e),
                suggestion: Some("Check that the MCP server is running".to_string()),
            })?;

        let call_result: CallToolResult = serde_json::from_value(result)
            .map_err(|e| ToolError::Fatal { message: format!("MCP response parse error: {}", e) })?;

        if call_result.is_error.unwrap_or(false) {
            Err(ToolError::Recoverable {
                message: call_result.content.iter()
                    .filter_map(|c| if let Content::Text { text } = c { Some(text.clone()) } else { None })
                    .collect::<Vec<_>>()
                    .join("\n"),
                suggestion: None,
            })
        } else {
            Ok(ToolOutput::Text(call_result.content.iter()
                .filter_map(|c| if let Content::Text { text } = c { Some(text.clone()) } else { None })
                .collect::<Vec<_>>()
                .join("\n")))
        }
    }

    async fn initialize(&self, _ctx: &PluginContext) -> Result<(), PluginError> { Ok(()) }
}
```

---

## 8. Security Considerations

### 8.1 Trust Model

```
┌────────────────────────────────────────────────────┐
│                    TRUST BOUNDARY                   │
│                                                    │
│  HIGH TRUST          MEDIUM TRUST        LOW TRUST │
│  ──────────          ───────────        ──────────│
│  Built-in tools  ←→  MCP Server      ←→  MCP      │
│  (in-process)        (xaft exposes)       Client   │
│                      (controlled access)   (3rd    │
│                                           party)   │
└────────────────────────────────────────────────────┘
```

### 8.2 Capability Mapping for MCP Tools

When consuming MCP tools, xaft assigns conservative capabilities:

| MCP Tool Annotation | xaft Capability |
|---------------------|-----------------|
| `readOnlyHint: true` | `fs_read` only |
| `destructiveHint: true` | Flagged in guardrail |
| `openWorldHint: true` | `network` + `shell` |
| No annotations | All capabilities denied (manual review) |

```rust
impl McpClientManager {
    fn infer_capabilities(tool: &McpTool) -> Vec<Capability> {
        let mut caps = Vec::new();

        if let Some(ann) = &tool.annotations {
            if ann.read_only_hint.unwrap_or(false) {
                caps.push(Capability::FsRead { patterns: vec!["**/*".to_string()] });
            }
            if ann.open_world_hint.unwrap_or(false) {
                caps.push(Capability::Network);
                caps.push(Capability::Shell { commands: vec!["*".to_string()] });
            }
        }

        // Default: no capabilities → tool can only return data, no side effects
        if caps.is_empty() {
            caps.push(Capability::None);
        }

        caps
    }
}
```

### 8.3 Rate Limiting

MCP server calls are rate-limited to prevent runaway agents:

```rust
pub struct McpRateLimiter {
    per_tool: Arc<Mutex<HashMap<String, TokenBucket>>>,
    global: TokenBucket,
}

impl McpRateLimiter {
    pub fn new(config: &McpRateLimitConfig) -> Self {
        Self {
            per_tool: Arc::new(Mutex::new(HashMap::new())),
            global: TokenBucket::new(config.global_rps, config.global_burst),
        }
    }

    pub async fn acquire(&self, tool_name: &str) -> Result<(), RateLimitError> {
        self.global.acquire().await?;

        let mut per_tool = self.per_tool.lock().await;
        let bucket = per_tool.entry(tool_name.to_string())
            .or_insert_with(|| TokenBucket::new(10, 5)); // default: 10 rps, burst 5

        bucket.acquire().await
    }
}
```

---

## 9. Error Handling and Resilience

### 9.1 Retry Strategy

```
  MCP Tool Call
       │
       ├── Success ────────────────► Return ToolOutput
       │
       ├── Transport Error ────────► Retry (exponential backoff)
       │     ├── Retry 1 (100ms)
       │     ├── Retry 2 (200ms)
       │     ├── Retry 3 (400ms)
       │     └── All retries exhausted ──► ToolError::Recoverable
       │
       ├── MCP Error (method not found) ──► ToolError::Fatal
       │
       └── MCP Error (invalid params) ──► ToolError::Recoverable
```

### 9.2 Connection Health Monitoring

```rust
pub struct McpHealthMonitor {
    clients: Arc<Mutex<HashMap<String, ClientHealth>>>,
}

struct ClientHealth {
    name: String,
    last_success: Instant,
    consecutive_failures: u32,
    latency_ema: f64,         // Exponential moving average (ms)
    status: HealthStatus,
}

enum HealthStatus {
    Healthy,
    Degraded { latency_ms: f64 },
    Unhealthy { since: Instant },
}

impl McpHealthMonitor {
    pub async fn check_all(&self) -> Vec<HealthReport> {
        let clients = self.clients.lock().await;
        clients.values().map(|c| HealthReport {
            name: c.name.clone(),
            status: c.status.clone(),
            latency_ema: c.latency_ema,
        }).collect()
    }

    pub fn record_success(&self, client: &str, latency: Duration) {
        // Update EMA: new_ema = alpha * latency + (1 - alpha) * old_ema
        // Reset consecutive failures
    }

    pub fn record_failure(&self, client: &str) {
        // Increment consecutive failures
        // If >= 3, mark Unhealthy
    }
}
```

---

## 10. Logging and Debugging

### 10.1 MCP Logging Protocol

xaft exposes its log stream to MCP clients via the `logging/setLevel` and `notifications/message` protocol:

```rust
impl XaftMcpServerHandler {
    async fn handle_logging_set_level(&self, params: LoggingSetLevelParams) -> Result<(), McpError> {
        let level = match params.level.as_str() {
            "debug" => tracing::Level::DEBUG,
            "info" => tracing::Level::INFO,
            "warning" => tracing::Level::WARN,
            "error" => tracing::Level::ERROR,
            _ => return Err(McpError::invalid_params(format!("unknown log level: {}", params.level))),
        };

        self.log_level.store(level, Ordering::Relaxed);
        Ok(())
    }
}
```

### 10.2 Debugging Support

```toml
# In xaft.toml
[mcp.debug]
# Log all MCP protocol messages to file
trace_log = "/tmp/xaft-mcp-trace.jsonl"
# Include full request/response bodies (not just headers)
verbose = true
# Record tool call timing
timing = true
```

---

## 11. Testing Strategy

| Level | Test | Approach |
|-------|------|----------|
| Unit | Schema mapper | Property-based testing with `proptest` |
| Integration | Server mode | Mock MCP client → xaft server → real tool |
| Integration | Client mode | xaft client → test MCP server (npm `@modelcontextprotocol/sdk`) |
| Transport | Stdio | Spawn child process, verify JSON-RPC protocol |
| Transport | HTTP+SSE | Hyper test harness, verify SSE stream |
| Fuzz | JSON-RPC parsing | `cargo-fuzz` on incoming messages |
| E2E | Full bridge | Agent loop uses MCP tool end-to-end |

---

## 12. Milestones

| Phase | Deliverable | Timeline |
|-------|-------------|----------|
| P1 | Transport abstractions + stdio client | Week 1 |
| P2 | Schema mapper + MCP tool adapter | Week 2 |
| P3 | MCP server mode (stdio + HTTP SSE) | Week 3-4 |
| P4 | AgentMessageBus bridge + health monitor | Week 5 |
| P5 | Rate limiting + security hardening | Week 6 |
| P6 | Integration tests + trace logging | Week 7 |

---

## 13. Open Questions

1. **Resource exposure**: Should xaft expose its file tree as MCP `resources`? This would let MCP clients browse the project without calling tools.
2. **Prompt exposure**: Should xaft expose agent system prompts as MCP `prompts`? Would allow MCP clients to pick xaft agent presets.
3. **Sampling**: Should xaft support MCP `sampling` so MCP servers can request LLM completions through xaft's provider? Significant security implications.
4. **Progress tokens**: MCP supports progress reporting for long-running tools. Should xaft stream progress from native tools through MCP?
5. **Authentication**: Should the HTTP+SSE transport support OAuth2 or API key auth for remote access?
