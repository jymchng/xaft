# Structured Output

> How xauft forces typed JSON output via tool-calling: `StructuredLlm<T>`,
> `JsonSchema` generation via `schemars`, `ReturnMode`, `SubagentTool<T>`
> structured output, planner `PlanOutput`, error handling, and lenient
> deserializers.

---

## 1. Overview

LLMs natively produce free-form text. xauft needs **typed, structured
output** for:

1. **Plan generation** — planner outputs `PlanOutput` with typed steps.
2. **Sub-agent delegation** — `SubagentTool<T>` expects typed output from sub-agents.
3. **Code review** — `CodeReviewOutput` with structured issues.
4. **Tool call schemas** — tool inputs/outputs are typed.

xauft achieves this by **wrapping structured output as a tool call**: the
LLM is given a single "output tool" whose JSON schema matches the desired
output type. The LLM must call this tool to produce its answer.

```
  ┌─────────────────┐
  │   User Prompt   │
  └────────┬────────┘
           │
           ▼
  ┌─────────────────────────────────────────┐
  │         LLM Request                     │
  │                                         │
  │  Messages: [task description]           │
  │  Tools: [                               │
  │    {                                    │
  │      name: "submit_output",             │
  │      schema: PlanOutput JSON schema     │
  │    }                                    │
  │  ]                                      │
  │  tool_choice: "required"                │
  └────────────────┬────────────────────────┘
                   │
                   ▼
  ┌─────────────────────────────────────────┐
  │         LLM Response                    │
  │                                         │
  │  tool_calls: [                          │
  │    {                                    │
  │      name: "submit_output",             │
  │      input: {                           │
  │        "steps": [...],                  │
  │        "reasoning": "..."               │
  │      }                                  │
  │    }                                    │
  │  ]                                      │
  └────────────────┬────────────────────────┘
                   │
                   ▼
  ┌─────────────────────────────────────────┐
  │  Deserialize tool_call.input → T        │
  │  T = PlanOutput                         │
  └─────────────────────────────────────────┘
```

---

## 2. StructuredLlm\<T\>

### 2.1 Core Type

`StructuredLlm<T>` is the primary interface for generating typed output
from an LLM.

```rust
pub struct StructuredLlm<'a, T, P: LlmProvider> {
    provider: &'a P,
    model: &'a str,
    system_prompt: String,
    /// JSON schema for the output type T.
    schema: RootSchema,
    /// Name of the "output tool" presented to the LLM.
    output_tool_name: String,
    /// Description of the output tool.
    output_tool_description: String,
    /// Return mode: how to present the result.
    return_mode: ReturnMode,
    /// Maximum retries on deserialization failure.
    max_retries: u32,
    /// Temperature for generation.
    temperature: f64,
    /// Maximum tokens for generation.
    max_tokens: usize,
    _phantom: PhantomData<T>,
}

/// How the structured result is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnMode {
    /// Return the deserialized T directly.
    /// Most common mode for internal use.
    DirectJson,

    /// Return as a StructuredLlm wrapper that includes metadata
    /// (schema, raw response, token usage, etc.).
    StructuredLlm,
}
```

### 2.2 Construction

```rust
impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Send + 'static,
{
    /// Create a new StructuredLlm for type T.
    pub fn new(
        provider: &'a P,
        model: &'a str,
        system_prompt: impl Into<String>,
    ) -> Self {
        let schema = schemars::schema_for!(T);
        Self {
            provider,
            model,
            system_prompt: system_prompt.into(),
            schema,
            output_tool_name: "submit_output".into(),
            output_tool_description: format!(
                "Submit the structured output. The output must conform to the provided JSON schema."
            ),
            return_mode: ReturnMode::DirectJson,
            max_retries: 2,
            temperature: 0.0,
            max_tokens: 4096,
            _phantom: PhantomData,
        }
    }

    /// Customize the output tool name.
    pub fn with_output_tool_name(mut self, name: impl Into<String>) -> Self {
        self.output_tool_name = name.into();
        self
    }

    /// Set the return mode.
    pub fn with_return_mode(mut self, mode: ReturnMode) -> Self {
        self.return_mode = mode;
        self
    }

    /// Set the temperature.
    pub fn with_temperature(mut self, temp: f64) -> Self {
        self.temperature = temp;
        self
    }

    /// Set maximum tokens.
    pub fn with_max_tokens(mut self, tokens: usize) -> Self {
        self.max_tokens = tokens;
        self
    }

    /// Set maximum retries on deserialization failure.
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }
}
```

### 2.3 Generation

```rust
impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + 'static,
{
    /// Generate structured output from a prompt.
    pub async fn generate(
        &self,
        prompt: &str,
    ) -> Result<T, StructuredOutputError> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            // Build the request
            let request = self.build_request(prompt, attempt)?;

            // Call the LLM
            let response = self.provider.complete(request).await
                .map_err(StructuredOutputError::ProviderError)?;

            // Extract structured output from tool call
            let result = self.extract_output(&response);

            match result {
                Ok(output) => {
                    return Ok(output);
                }
                Err(StructuredOutputError::MalformedJson { raw, error }) => {
                    if attempt <= self.max_retries {
                        tracing::warn!(
                            attempt = attempt,
                            error = %error,
                            "Malformed JSON, retrying with error feedback"
                        );
                        // Continue loop with error feedback in prompt
                        continue;
                    }
                    return Err(StructuredOutputError::MalformedJson { raw, error });
                }
                Err(StructuredOutputError::NoToolCall { content }) => {
                    if attempt <= self.max_retries {
                        tracing::warn!(
                            attempt = attempt,
                            "No tool call in response, retrying with stronger prompt"
                        );
                        continue;
                    }
                    return Err(StructuredOutputError::NoToolCall { content });
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Build the LLM request with the output tool.
    fn build_request(
        &self,
        prompt: &str,
        attempt: usize,
    ) -> Result<LlmRequest, StructuredOutputError> {
        let schema_value = serde_json::to_value(&self.schema)
            .map_err(|e| StructuredOutputError::SchemaError(e.to_string()))?;

        let tool_definition = ToolDefinition {
            name: self.output_tool_name.clone(),
            description: self.output_tool_description.clone(),
            input_schema: schema_value
                .get("definitions")
                .cloned()
                .unwrap_or(schema_value),
        };

        let mut system = self.system_prompt.clone();
        if attempt > 1 {
            system.push_str("\n\nIMPORTANT: Your previous response did not produce valid JSON. \
                             You MUST call the submit_output tool with valid JSON conforming \
                             to the schema. Do NOT output free-form text.");
        }

        Ok(LlmRequest {
            model: self.model.to_string(),
            messages: vec![Message::user(prompt)],
            system_prompt: Some(system),
            temperature: Some(self.temperature),
            max_tokens: Some(self.max_tokens),
            tools: vec![tool_definition],
            tool_choice: Some(ToolChoice::Required),  // Force tool call
            response_format: None,  // Using tool-calling, not response_format
            stop: vec![],
            top_p: None,
            seed: None,
            metadata: HashMap::new(),
        })
    }

    /// Extract the typed output from the LLM response.
    fn extract_output(&self, response: &LlmResponse) -> Result<T, StructuredOutputError> {
        // Find the output tool call
        let tool_call = response.tool_calls.iter()
            .find(|tc| tc.name == self.output_tool_name)
            .ok_or_else(|| StructuredOutputError::NoToolCall {
                content: response.content.clone(),
            })?;

        // Deserialize the tool call input
        let output: T = serde_json::from_value(tool_call.input.clone())
            .map_err(|e| StructuredOutputError::MalformedJson {
                raw: serde_json::to_string(&tool_call.input).unwrap_or_default(),
                error: e.to_string(),
            })?;

        Ok(output)
    }
}
```

### 2.4 Generate with Metadata

```rust
pub struct StructuredOutput<T> {
    /// The deserialized output.
    pub output: T,
    /// Raw JSON from the LLM.
    pub raw_json: serde_json::Value,
    /// The JSON schema used.
    pub schema: RootSchema,
    /// Token usage.
    pub usage: TokenUsage,
    /// Number of attempts (including retries).
    pub attempts: u32,
    /// Model used.
    pub model: String,
    /// Duration of generation.
    pub duration: Duration,
}

impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + 'static,
{
    /// Generate structured output with full metadata.
    pub async fn generate_with_metadata(
        &self,
        prompt: &str,
    ) -> Result<StructuredOutput<T>, StructuredOutputError> {
        let start = Instant::now();
        let mut attempt = 0;
        let mut last_usage = TokenUsage::default();

        loop {
            attempt += 1;
            let request = self.build_request(prompt, attempt)?;
            let response = self.provider.complete(request).await
                .map_err(StructuredOutputError::ProviderError)?;

            last_usage = response.usage.clone();

            match self.extract_output(&response) {
                Ok(output) => {
                    let raw_json = response.tool_calls.iter()
                        .find(|tc| tc.name == self.output_tool_name)
                        .map(|tc| tc.input.clone())
                        .unwrap_or(serde_json::Value::Null);

                    return Ok(StructuredOutput {
                        output,
                        raw_json,
                        schema: self.schema.clone(),
                        usage: last_usage,
                        attempts: attempt,
                        model: self.model.to_string(),
                        duration: start.elapsed(),
                    });
                }
                Err(StructuredOutputError::MalformedJson { .. }) if attempt <= self.max_retries => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
```

---

## 3. JsonSchema Generation via schemars

### 3.1 How It Works

xauft uses the `schemars` crate to derive JSON Schema from Rust types. The
schema is then presented to the LLM as a tool input schema.

```rust
use schemars::{schema_for, JsonSchema};

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
pub struct PlanOutput {
    /// The planned steps.
    pub steps: Vec<PlannedStepOutput>,
    /// Reasoning for the plan.
    pub reasoning: String,
    /// Confidence in the plan (0.0–1.0).
    pub confidence: f64,
    /// Assumptions made.
    pub assumptions: Vec<String>,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
pub struct PlannedStepOutput {
    /// Step description.
    pub description: String,
    /// Agent role to assign.
    pub assigned_role: String,
    /// Step dependencies (indices of prior steps).
    pub depends_on: Vec<usize>,
    /// Risk level.
    pub risk: String,
    /// Tools needed.
    pub available_tools: Vec<String>,
    /// Estimated complexity.
    pub complexity: String,
}
```

The generated JSON Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "PlanOutput",
  "type": "object",
  "required": ["steps", "reasoning", "confidence", "assumptions"],
  "properties": {
    "steps": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlannedStepOutput"
      }
    },
    "reasoning": {
      "type": "string"
    },
    "confidence": {
      "type": "number",
      "format": "double",
      "minimum": 0.0,
      "maximum": 1.0
    },
    "assumptions": {
      "type": "array",
      "items": {
        "type": "string"
      }
    }
  },
  "definitions": {
    "PlannedStepOutput": {
      "type": "object",
      "required": ["description", "assigned_role", "depends_on", "risk", "available_tools", "complexity"],
      "properties": {
        "description": { "type": "string" },
        "assigned_role": { "type": "string" },
        "depends_on": {
          "type": "array",
          "items": { "type": "integer", "minimum": 0 }
        },
        "risk": { "type": "string" },
        "available_tools": {
          "type": "array",
          "items": { "type": "string" }
        },
        "complexity": { "type": "string" }
      }
    }
  }
}
```

### 3.2 Schema Customization

```rust
/// Custom schemars settings for xauft schemas.
pub fn xaft_schema_settings() -> schemars::gen::SchemaSettings {
    let mut settings = schemars::gen::SchemaSettings::draft07();
    settings.option_add_null_type = false;      // Don't add null as option type
    settings.option_nullable = true;             // Use nullable keyword
    settings.inline_subschemas = false;          // Use $ref for sub-schemas
    settings.meta_schema = None;                 // Don't include $schema
    settings
}

/// Generate a schema optimized for LLM consumption.
pub fn schema_for_llm<T: JsonSchema>() -> RootSchema {
    let mut generator = schemars::gen::SchemaGenerator::new(xaft_schema_settings());
    let schema = generator.root_schema_for::<T>();

    // Add descriptions from doc comments (schemars picks these up)
    // Add examples if available
    schema
}
```

### 3.3 Schema Enhancements

xauft enhances schemas with LLM-friendly hints:

```rust
pub trait SchemaEnhancer: Send + Sync {
    fn enhance(&self, schema: &mut RootSchema);
}

/// Add enum constraints for string fields that should be one of a set of values.
pub struct EnumConstraintEnhancer;

impl SchemaEnhancer for EnumConstraintEnhancer {
    fn enhance(&self, schema: &mut RootSchema) {
        // Walk schema and add enum constraints where appropriate
        // e.g., "assigned_role" → enum: ["coder", "qa", "fixer", "planner"]
        // e.g., "risk" → enum: ["low", "medium", "high", "critical"]
    }
}

/// Add descriptions to fields that lack them.
pub struct DescriptionEnhancer {
    field_descriptions: HashMap<String, String>,
}

impl SchemaEnhancer for DescriptionEnhancer {
    fn enhance(&self, schema: &mut RootSchema) {
        for (field, description) in &self.field_descriptions {
            if let Some(properties) = schema.schema.object.as_mut()
                .and_then(|o| o.properties.get_mut(field))
            {
                // Add description
            }
        }
    }
}
```

---

## 4. ReturnMode::DirectJson vs StructuredLlm

### 4.1 DirectJson Mode

In `DirectJson` mode, `StructuredLlm<T>` returns the deserialized `T`
directly. This is the simplest and most common mode:

```rust
let planner = StructuredLlm::<PlanOutput, _>::new(
    &provider, "gpt-4o",
    "You are a task planner. Decompose tasks into executable steps.",
);

let plan: PlanOutput = planner.generate(&task.description).await?;
// Use plan.steps, plan.reasoning, etc.
```

### 4.2 StructuredLlm Mode

In `StructuredLlm` mode, the full metadata is returned alongside the output:

```rust
let planner = StructuredLlm::<PlanOutput, _>::new(
    &provider, "gpt-4o",
    "You are a task planner.",
).with_return_mode(ReturnMode::StructuredLlm);

let result: StructuredOutput<PlanOutput> = planner.generate_with_metadata(&task.description).await?;

// Access the output
println!("Plan: {:?}", result.output);
// Access metadata
println!("Raw JSON: {}", serde_json::to_string_pretty(&result.raw_json)?);
println!("Schema: {}", serde_json::to_string_pretty(&result.schema)?);
println!("Token usage: {:?}", result.usage);
println!("Attempts: {}", result.attempts);
```

### 4.3 When to Use Each Mode

| Mode            | Use Case                                         |
|-----------------|--------------------------------------------------|
| DirectJson      | Internal pipeline (plan generation, sub-agent)   |
| StructuredLlm   | Debugging, auditing, schema evolution            |
| DirectJson      | Production (lower overhead)                      |
| StructuredLlm   | Development (need to inspect raw output)         |

---

## 5. SubagentTool\<T\> and Structured Output

### 5.1 Integration

`SubagentTool<T>` uses `StructuredLlm<T>` internally to force the sub-agent
to produce typed output:

```rust
impl<TInput, TOutput, P: LlmProvider> SubagentTool<TInput, TOutput, P>
where
    TInput: JsonSchema + DeserializeOwned + Send + Sync + 'static,
    TOutput: JsonSchema + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub async fn execute(&self, input: TInput) -> Result<TOutput, SubagentError> {
        let input_json = serde_json::to_value(&input)?;

        // Build system prompt with schema instructions
        let output_schema = schemars::schema_for!(TOutput);
        let system_prompt = format!(
            "You are a specialised sub-agent. You will receive JSON input and \
             must produce structured output by calling the submit_output tool.\n\n\
             Output Schema:\n```json\n{}\n```",
            serde_json::to_string_pretty(&output_schema)?
        );

        // Create StructuredLlm for typed output
        let structured = StructuredLlm::<TOutput, P>::new(
            &*self.provider,
            "gpt-4o",  // or configured model
            &system_prompt,
        )
        .with_output_tool_name("submit_output")
        .with_max_retries(2)
        .with_temperature(0.0);

        // Generate structured output
        let prompt = format!(
            "Input:\n```json\n{}\n```\n\nProcess this input and submit your output.",
            serde_json::to_string_pretty(&input_json)?
        );

        let output = structured.generate(&prompt).await
            .map_err(SubagentError::StructuredOutputError)?;

        Ok(output)
    }
}
```

### 5.2 Example: Code Review Sub-Agent

```rust
#[derive(Debug, JsonSchema, Deserialize)]
struct CodeReviewInput {
    file_path: String,
    diff: String,
    language: String,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
struct CodeReviewOutput {
    issues: Vec<CodeIssue>,
    summary: String,
    approved: bool,
    security_concerns: Vec<String>,
    performance_notes: Vec<String>,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
struct CodeIssue {
    line: u32,
    severity: Severity,
    message: String,
    suggestion: Option<String>,
    category: IssueCategory,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
enum IssueCategory {
    Style,
    Correctness,
    Security,
    Performance,
    Maintainability,
}

// Usage
let review_tool = SubagentTool::<CodeReviewInput, CodeReviewOutput>::new(
    "code_review",
    "Delegate code review to a specialised QA sub-agent",
    qa_agent_config,
    provider.clone(),
    bus.clone(),
);

// When the agent calls this tool:
let input = CodeReviewInput {
    file_path: "src/auth/middleware.rs".into(),
    diff: "@@ -10,3 +10,5 @@\n+unsafe { ... }".into(),
    language: "rust".into(),
};

let output: CodeReviewOutput = review_tool.execute(input).await?;
// output.issues: Vec<CodeIssue>
// output.approved: bool
// output.security_concerns: Vec<String>
```

---

## 6. Planner Structured Output

### 6.1 PlanOutput Structure

The planner generates a `PlanOutput` using `StructuredLlm<PlanOutput>`:

```rust
#[derive(Debug, JsonSchema, Serialize, Deserialize)]
pub struct PlanOutput {
    /// Planned execution steps.
    pub steps: Vec<PlannedStepOutput>,
    /// Reasoning behind the plan.
    pub reasoning: String,
    /// Confidence level (0.0–1.0).
    pub confidence: f64,
    /// Assumptions made during planning.
    pub assumptions: Vec<String>,
    /// Potential risks.
    pub risks: Vec<String>,
    /// Alternative approaches considered.
    pub alternatives: Vec<String>,
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
pub struct PlannedStepOutput {
    /// Human-readable step description.
    pub description: String,
    /// Agent role assignment.
    pub assigned_role: String,
    /// Indices of steps this depends on.
    pub depends_on: Vec<usize>,
    /// Risk level: "low", "medium", "high", "critical".
    pub risk: String,
    /// Tools this step needs.
    pub available_tools: Vec<String>,
    /// Estimated complexity: "simple", "moderate", "complex".
    pub complexity: String,
    /// Estimated duration in seconds.
    pub estimated_duration_secs: Option<u64>,
    /// Whether this step can be parallelized.
    pub parallelizable: bool,
}
```

### 6.2 Planner Usage

```rust
impl<P: LlmProvider> OneShotPlanner<P> {
    pub async fn plan(&self, task: &Task) -> Result<Plan, PlannerError> {
        let structured = StructuredLlm::<PlanOutput, P>::new(
            &self.provider,
            &self.model,
            PLANNER_SYSTEM_PROMPT,
        )
        .with_output_tool_name("submit_plan")
        .with_max_retries(3)
        .with_temperature(0.1);  // Low temperature for consistency

        let prompt = format!(
            "Task: {}\n\nDecompose this task into concrete, executable steps. \
             Each step should be independently executable by a specialised agent.",
            task.description
        );

        let plan_output: PlanOutput = structured.generate(&prompt).await
            .map_err(PlannerError::StructuredOutputError)?;

        // Convert PlanOutput to Plan
        let mut steps = Vec::new();
        for (i, step_out) in plan_output.steps.iter().enumerate() {
            steps.push(PlannedStep {
                id: StepId::from_index(i),
                description: step_out.description.clone(),
                assigned_role: AgentRole::from_str(&step_out.assigned_role)?,
                available_tools: step_out.available_tools.clone(),
                depends_on: step_out.depends_on.iter()
                    .map(|&idx| StepId::from_index(idx))
                    .collect(),
                state: StepState::Planned,
                checkpoint: None,
                retry_count: 0,
                max_retries: 3,
                risk: RiskLevel::from_str(&step_out.risk)?,
                result: None,
                estimated_cost: None,
            });
        }

        Ok(Plan {
            id: PlanId::new(),
            task_id: task.id,
            steps,
            planner: PlannerType::OneShot,
            created_at: Utc::now(),
            revision: 0,
        })
    }
}
```

---

## 7. Error Handling for Malformed JSON

### 7.1 Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum StructuredOutputError {
    #[error("Provider error: {0}")]
    ProviderError(#[from] ProviderError),

    #[error("No tool call found in response. Content: {content}")]
    NoToolCall { content: String },

    #[error("Malformed JSON in tool call input: {error}. Raw: {raw}")]
    MalformedJson { raw: String, error: String },

    #[error("Schema generation error: {0}")]
    SchemaError(String),

    #[error("Max retries ({max}) exceeded for structured output")]
    MaxRetriesExceeded { max: u32, last_error: String },

    #[error("Validation error: {0}")]
    ValidationError(String),
}
```

### 7.2 Retry with Error Feedback

When JSON deserialization fails, xauft retries with the error message
included in the prompt:

```
  Attempt 1:
  LLM produces: { "steps": [...], "confidence": "high" }
  Error: "invalid type: string \"high\", expected f64"

  Attempt 2:
  Prompt includes: "Your previous output was invalid: invalid type: string
  \"high\", expected f64. The 'confidence' field must be a number between
  0.0 and 1.0."

  LLM produces: { "steps": [...], "confidence": 0.8 }
  Success!
```

```rust
impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + 'static,
{
    fn build_request(
        &self,
        prompt: &str,
        attempt: usize,
        last_error: Option<&str>,
    ) -> Result<LlmRequest, StructuredOutputError> {
        let mut messages = vec![Message::user(prompt)];

        if let Some(error) = last_error {
            messages.push(Message::system(format!(
                "⚠️ Your previous output was INVALID. Error: {}\n\n\
                 You MUST call the '{}' tool with valid JSON that conforms \
                 to the schema. Common mistakes:\n\
                 - Strings where numbers are expected\n\
                 - Missing required fields\n\
                 - Invalid enum values\n\
                 - Arrays where objects are expected\n\n\
                 Please try again with correct output.",
                error, self.output_tool_name
            )));
        }

        // ... rest of request building
        Ok(LlmRequest {
            messages,
            // ...
            ..Default::default()
        })
    }
}
```

---

## 8. Lenient Deserializers

### 8.1 Motivation

LLMs frequently produce JSON that is *almost* correct but has minor issues:

- String values where numbers are expected (`"3"` instead of `3`)
- Missing optional fields (should use defaults)
- Extra fields not in the schema (should be ignored)
- Enum values with different casing (`"High"` instead of `"high"`)

xauft uses **lenient deserializers** that tolerate these issues.

### 8.2 Lenient Deserialization Strategy

```rust
pub struct LenientDeserializer;

impl LenientDeserializer {
    /// Attempt to deserialize with increasing leniency levels.
    pub fn deserialize<T: DeserializeOwned>(
        value: &serde_json::Value,
    ) -> Result<T, StructuredOutputError> {
        // Level 0: Strict deserialization
        match serde_json::from_value::<T>(value.clone()) {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::debug!("Strict deserialization failed: {}", e);
                // Continue to lenient levels
            }
        }

        // Level 1: Coerce types (strings → numbers, etc.)
        let coerced = Self::coerce_types(value);
        match serde_json::from_value::<T>(coerced) {
            Ok(v) => {
                tracing::info!("Succeeded with type coercion");
                return Ok(v);
            }
            Err(e) => {
                tracing::debug!("Type-coerced deserialization failed: {}", e);
            }
        }

        // Level 2: Fill defaults for missing fields
        let with_defaults = Self::fill_defaults(value);
        match serde_json::from_value::<T>(with_defaults) {
            Ok(v) => {
                tracing::info!("Succeeded with default filling");
                return Ok(v);
            }
            Err(e) => {
                tracing::debug!("Default-filled deserialization failed: {}", e);
            }
        }

        // Level 3: Strip unknown fields
        let stripped = Self::strip_unknown_fields(value);
        match serde_json::from_value::<T>(stripped) {
            Ok(v) => {
                tracing::info!("Succeeded after stripping unknown fields");
                return Ok(v);
            }
            Err(e) => {
                // All levels failed
                return Err(StructuredOutputError::MalformedJson {
                    raw: serde_json::to_string(value).unwrap_or_default(),
                    error: e.to_string(),
                });
            }
        }
    }
}
```

### 8.3 Type Coercion

```rust
impl LenientDeserializer {
    /// Coerce common type mismatches.
    fn coerce_types(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut new_map = serde_json::Map::new();
                for (key, val) in map {
                    new_map.insert(key.clone(), Self::coerce_types(val));
                }
                serde_json::Value::Object(new_map)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(Self::coerce_types).collect())
            }
            serde_json::Value::String(s) => {
                // Try to parse string as number
                if let Ok(n) = s.parse::<i64>() {
                    serde_json::Value::Number(n.into())
                } else if let Ok(n) = s.parse::<f64>() {
                    if let Some(num) = Number::from_f64(n) {
                        serde_json::Value::Number(num)
                    } else {
                        serde_json::Value::String(s.clone())
                    }
                } else if s.eq_ignore_ascii_case("true") {
                    serde_json::Value::Bool(true)
                } else if s.eq_ignore_ascii_case("false") {
                    serde_json::Value::Bool(false)
                } else if s.eq_ignore_ascii_case("null") {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(s.clone())
                }
            }
            other => other.clone(),
        }
    }

    /// Fill default values for missing fields based on the schema.
    fn fill_defaults(value: &serde_json::Value) -> serde_json::Value {
        // If value is an object, check for missing fields and add defaults
        match value {
            serde_json::Value::Object(map) => {
                let mut new_map = map.clone();
                // Add empty arrays for missing array fields
                // Add 0.0 for missing number fields
                // Add empty strings for missing string fields
                // This requires schema awareness
                serde_json::Value::Object(new_map)
            }
            other => other.clone(),
        }
    }

    /// Remove fields that aren't in the target schema.
    fn strip_unknown_fields(value: &serde_json::Value) -> serde_json::Value {
        // This would need schema awareness to know which fields are valid
        // For now, just return as-is (serde already ignores unknown fields
        // with #[serde(deny_unknown_fields)] not set)
        value.clone()
    }
}
```

### 8.4 Lenient Enum Handling

```rust
/// Custom deserialize wrapper that handles case-insensitive enum values.
pub fn deserialize_lenient_enum<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: FromStr + std::fmt::Debug,
    <T as FromStr>::Err: std::fmt::Debug,
{
    let s = String::deserialize(deserializer)?;
    // Try exact match first
    T::from_str(&s).map_err(|_| {
        // Try case-insensitive match
        let lower = s.to_lowercase();
        // Try common variations
        for variant in T::variants() {
            if variant.to_lowercase() == lower {
                return T::from_str(variant)
                    .map_err(|_| serde::de::Error::custom(format!(
                        "Invalid enum value: {}", s
                    )));
            }
        }
        serde::de::Error::custom(format!("Invalid enum value: {}", s))
    })
}
```

### 8.5 Lenient Number Handling

```rust
/// Custom deserialize that accepts both numbers and numeric strings.
pub fn deserialize_lenient_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    struct F64Visitor;

    impl<'de> de::Visitor<'de> for F64Visitor {
        type Value = f64;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a number or a numeric string")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> { Ok(v as f64) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> { Ok(v as f64) }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> {
            v.parse::<f64>().map_err(|_| {
                E::custom(format!("cannot parse '{}' as f64", v))
            })
        }
    }

    deserializer.deserialize_any(F64Visitor)
}

// Usage:
#[derive(Debug, JsonSchema, Deserialize)]
struct PlanOutput {
    #[serde(deserialize_with = "deserialize_lenient_f64")]
    confidence: f64,
}
```

---

## 9. Provider-Specific Structured Output

### 9.1 OpenAI: response_format

OpenAI supports `response_format: { type: "json_object" }` and
`response_format: { type: "json_schema", json_schema: {...} }`. When the
provider supports it, xauft can use this instead of tool-calling:

```rust
impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + 'static,
{
    fn build_request(
        &self,
        prompt: &str,
        attempt: usize,
    ) -> Result<LlmRequest, StructuredOutputError> {
        let caps = self.provider.model_capabilities(self.model);

        // Use native structured output if supported
        if caps.structured_output {
            return Ok(LlmRequest {
                model: self.model.to_string(),
                messages: vec![Message::user(prompt)],
                system_prompt: Some(self.system_prompt.clone()),
                response_format: Some(ResponseFormat::JsonSchema {
                    schema: serde_json::to_value(&self.schema)?,
                }),
                tools: vec![],  // No tools needed
                tool_choice: None,
                temperature: Some(self.temperature),
                max_tokens: Some(self.max_tokens),
                ..Default::default()
            });
        }

        // Fallback: use tool-calling approach
        // ... (as shown earlier)
    }

    fn extract_output(&self, response: &LlmResponse) -> Result<T, StructuredOutputError> {
        let caps = self.provider.model_capabilities(self.model);

        if caps.structured_output && !response.content.is_empty() {
            // Parse from response content (native structured output)
            let value: serde_json::Value = serde_json::from_str(&response.content)
                .map_err(|e| StructuredOutputError::MalformedJson {
                    raw: response.content.clone(),
                    error: e.to_string(),
                })?;
            let output: T = LenientDeserializer::deserialize(&value)?;
            return Ok(output);
        }

        // Fallback: extract from tool call
        let tool_call = response.tool_calls.iter()
            .find(|tc| tc.name == self.output_tool_name)
            .ok_or_else(|| StructuredOutputError::NoToolCall {
                content: response.content.clone(),
            })?;

        let output: T = LenientDeserializer::deserialize(&tool_call.input)?;
        Ok(output)
    }
}
```

### 9.2 Anthropic: Tool-Calling Workaround

Anthropic doesn't support native structured output. xauft uses the
tool-calling approach exclusively for Anthropic:

```rust
// Anthropic-specific: force tool_choice to the output tool
// Anthropic requires tool_choice to be either "auto" or a specific tool
let tool_choice = ToolChoice::Specific(self.output_tool_name.clone());
```

### 9.3 Ollama: Format Parameter

Ollama supports a `format` parameter that enforces JSON output:

```rust
// Ollama-specific: use format: "json" with schema instructions in prompt
if self.provider.provider_type() == ProviderType::Ollama {
    let schema_prompt = format!(
        "\n\nYou MUST output valid JSON conforming to this schema:\n```json\n{}\n```",
        serde_json::to_string_pretty(&self.schema)?
    );
    // Append to system prompt
}
```

---

## 10. Validation and Post-Processing

### 10.1 Output Validation

After deserialization, xauft validates the output against business rules:

```rust
pub trait OutputValidator<T>: Send + Sync {
    fn validate(&self, output: &T) -> Result<Vec<ValidationIssue>, ValidationError>;
}

#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub field: String,
    pub message: String,
    pub severity: ValidationSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    Warning,
    Error,
}

/// Validator for PlanOutput.
pub struct PlanOutputValidator;

impl OutputValidator<PlanOutput> for PlanOutputValidator {
    fn validate(&self, output: &PlanOutput) -> Result<Vec<ValidationIssue>, ValidationError> {
        let mut issues = Vec::new();

        // Check that steps have valid dependency references
        for (i, step) in output.steps.iter().enumerate() {
            for dep in &step.depends_on {
                if *dep >= i {
                    issues.push(ValidationIssue {
                        field: format!("steps[{}].depends_on", i),
                        message: format!(
                            "Step {} depends on step {}, which doesn't exist yet",
                            i, dep
                        ),
                        severity: ValidationSeverity::Error,
                    });
                }
            }
        }

        // Check for cycles
        if self.has_cycles(&output.steps) {
            issues.push(ValidationIssue {
                field: "steps".into(),
                message: "Step dependencies contain a cycle".into(),
                severity: ValidationSeverity::Error,
            });
        }

        // Check confidence range
        if output.confidence < 0.0 || output.confidence > 1.0 {
            issues.push(ValidationIssue {
                field: "confidence".into(),
                message: "Confidence must be between 0.0 and 1.0".into(),
                severity: ValidationSeverity::Warning,
            });
        }

        Ok(issues)
    }

    fn has_cycles(&self, steps: &[PlannedStepOutput]) -> bool {
        // Topological sort cycle detection
        let mut visited = vec![false; steps.len()];
        let mut in_stack = vec![false; steps.len()];

        fn dfs(
            steps: &[PlannedStepOutput],
            node: usize,
            visited: &mut [bool],
            in_stack: &mut [bool],
        ) -> bool {
            if in_stack[node] { return true; }
            if visited[node] { return false; }
            visited[node] = true;
            in_stack[node] = true;
            for &dep in &steps[node].depends_on {
                if dfs(steps, dep, visited, in_stack) {
                    return true;
                }
            }
            in_stack[node] = false;
            false
        }

        for i in 0..steps.len() {
            if dfs(steps, i, &mut visited, &mut in_stack) {
                return true;
            }
        }
        false
    }
}
```

### 10.2 Validation Pipeline

```rust
impl<'a, T, P: LlmProvider> StructuredLlm<'a, T, P>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + 'static,
{
    /// Generate with validation.
    pub async fn generate_validated<V: OutputValidator<T>>(
        &self,
        prompt: &str,
        validator: &V,
    ) -> Result<ValidatedOutput<T>, StructuredOutputError> {
        let mut attempt = 0;
        let max_attempts = self.max_retries + 1;

        loop {
            attempt += 1;
            let output = self.generate(prompt).await?;
            let issues = validator.validate(&output).map_err(|e| {
                StructuredOutputError::ValidationError(e.to_string())
            })?;

            let errors: Vec<_> = issues.iter()
                .filter(|i| i.severity == ValidationSeverity::Error)
                .collect();

            if errors.is_empty() {
                return Ok(ValidatedOutput {
                    output,
                    warnings: issues.into_iter()
                        .filter(|i| i.severity == ValidationSeverity::Warning)
                        .collect(),
                });
            }

            if attempt >= max_attempts {
                return Err(StructuredOutputError::ValidationError(
                    errors.iter()
                        .map(|e| e.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }

            // Retry with validation feedback
            tracing::warn!(
                attempt = attempt,
                errors = ?errors,
                "Validation failed, retrying"
            );
        }
    }
}

pub struct ValidatedOutput<T> {
    pub output: T,
    pub warnings: Vec<ValidationIssue>,
}
```

---

## 11. Configuration Reference

```toml
[xaft.structured_output]
# Default settings for structured output generation
default_max_retries = 2
default_temperature = 0.0
default_max_tokens = 4096
lenient_deserialization = true     # enable lenient deserializers
validation_enabled = true          # validate output after deserialization

[xaft.structured_output.schema]
# Schema generation settings
draft_version = "draft-07"         # JSON Schema draft version
include_descriptions = true        # include doc comments as descriptions
inline_subschemas = false          # use $ref for sub-schemas
add_examples = true                # add example values to schema

[xaft.structured_output.providers]
# Provider-specific structured output preferences
openai = "native"                  # use response_format (json_schema)
anthropic = "tool_calling"         # use tool-calling workaround
ollama = "format_param"            # use format: "json"
gemini = "native"                  # use responseMimeType

[xaft.structured_output.retries]
# Retry behavior on deserialization failure
max_retries = 3
include_error_feedback = true      # include error message in retry prompt
backoff_ms = 100                   # delay between retries
escalate_on_max_retries = true     # escalate to parent agent on failure
```
