# XAFT Cost Dashboard

## Token and Cost Tracking Display

The cost dashboard provides real-time visibility into LLM token consumption and spending.
This is critical for autonomous coding agents, which can easily burn $10-50 per session
on frontier models. xaft consumes `UserUsageRecorded` and `ModelCallComplete` signals
from the agtrs runtime to maintain a live cost picture.

### Why Cost Visibility Matters

| Scenario | Without Dashboard | With Dashboard |
|---|---|---|
| Agent loops (retries) | $50 surprise bill | Catch at $5, intervene |
| Model routing mistake | Flagship used for trivial task | See model indicator, fix route |
| Budget overrun | No warning until API 429 | Budget bar, projection line |
| Subagent explosion | Unknown cost per agent | Per-agent breakdown |
| Long session | No running total | Cumulative cost always visible |

## Token Dashboard Pane

### Visual Design

```
┌─ Token Dashboard ────────────────────────────────────────────┐
│                                                              │
│  Tokens  ▸ 124,582                                          │
│  Cost    ▸ $1.87                                            │
│  Budget  ▸ ████████████░░░░░░░░ $5.00 ($3.13 remaining)     │
│                                                              │
│  ┌─ Per-Model Breakdown ──────────────────────────────────┐  │
│  │                                                         │  │
│  │  claude-sonnet-4-20250514        89,240t  $1.34  71.7% │  │
│  │  ████████████████████████████████████░░░░░░░░░░░░░░░░  │  │
│  │                                                         │  │
│  │  claude-haiku-3-20250301          35,342t  $0.05  2.7% │  │
│  │  █░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │  │
│  │                                                         │  │
│  └─────────────────────────────────────────────────────────┘  │
│                                                              │
│  ┌─ Per-Agent Breakdown ──────────────────────────────────┐  │
│  │                                                         │  │
│  │  ● coordinator        45,120t  $0.78   41.7%          │  │
│  │  ● file-editor-01     52,300t  $0.72   38.5%          │  │
│  │  ○ bash-runner-01      8,450t  $0.12    6.8%          │  │
│  │  ✓ researcher-01      18,712t  $0.25   13.0%          │  │
│  │                                                         │  │
│  └─────────────────────────────────────────────────────────┘  │
│                                                              │
│  Speed: 842 tok/s │ Rate: $0.12/min │ Projection: $3.20     │
│  Model: ● sonnet (flagship) │ ○ haiku (cheap)               │
└──────────────────────────────────────────────────────────────┘
```

## Cost State Management

### Data Structures

```rust
/// Cost tracking state for the dashboard
#[derive(Debug, Clone)]
pub struct CostState {
    /// Total tokens consumed (input + output)
    total_tokens: u64,

    /// Total cost in USD
    total_cost: f64,

    /// User-configured budget limit
    budget_limit: Option<f64>,

    /// Per-model token and cost tracking
    model_breakdown: HashMap<String, ModelCost>,

    /// Per-agent token and cost tracking
    agent_breakdown: HashMap<AgentId, AgentCost>,

    /// Token rate tracking (tokens per second)
    token_rate: RateTracker,

    /// Cost rate tracking (dollars per minute)
    cost_rate: RateTracker,

    /// Cost projection for current run
    projection: CostProjection,

    /// Session start time
    session_start: Instant,

    /// Price table (model → per-million-token cost)
    price_table: PriceTable,
}

#[derive(Debug, Clone)]
pub struct ModelCost {
    pub model_id: String,
    pub display_name: String,
    pub tier: ModelTier,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub call_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelTier {
    /// Most capable, most expensive (e.g., claude-sonnet, gpt-4o)
    Flagship,
    /// Fast and cheap (e.g., claude-haiku, gpt-4o-mini)
    Cheap,
    /// Embedding or specialty model
    Other,
}

#[derive(Debug, Clone)]
pub struct AgentCost {
    pub agent_id: AgentId,
    pub agent_name: String,
    pub total_tokens: u64,
    pub cost: f64,
    pub call_count: u64,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct CostProjection {
    /// Estimated total cost at session end
    pub estimated_total: f64,

    /// Estimated time remaining
    pub estimated_time: Duration,

    /// Confidence (0.0-1.0) — low early in session, high later
    pub confidence: f64,
}

/// Model pricing table (per million tokens)
#[derive(Debug, Clone)]
pub struct PriceTable {
    prices: HashMap<String, ModelPrice>,
}

#[derive(Debug, Clone)]
pub struct ModelPrice {
    pub input_per_mtok: f64,   // USD per million input tokens
    pub output_per_mtok: f64,  // USD per million output tokens
}

impl PriceTable {
    pub fn default_prices() -> Self {
        let mut prices = HashMap::new();

        prices.insert("claude-sonnet-4-20250514".into(), ModelPrice {
            input_per_mtok: 3.00,
            output_per_mtok: 15.00,
        });
        prices.insert("claude-haiku-3-20250301".into(), ModelPrice {
            input_per_mtok: 0.80,
            output_per_mtok: 4.00,
        });
        prices.insert("gpt-4o".into(), ModelPrice {
            input_per_mtok: 2.50,
            output_per_mtok: 10.00,
        });
        prices.insert("gpt-4o-mini".into(), ModelPrice {
            input_per_mtok: 0.15,
            output_per_mtok: 0.60,
        });
        prices.insert("gemini-2.0-flash".into(), ModelPrice {
            input_per_mtok: 0.10,
            output_per_mtok: 0.40,
        });

        Self { prices }
    }

    /// Calculate cost for a model call
    pub fn calculate_cost(&self, model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
        let price = self.prices.get(model).unwrap_or(&ModelPrice {
            input_per_mtok: 3.00,  // Default: assume flagship pricing
            output_per_mtok: 15.00,
        });

        let input_cost = (input_tokens as f64 / 1_000_000.0) * price.input_per_mtok;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * price.output_per_mtok;
        input_cost + output_cost
    }
}
```

## Signal Consumption

### UserUsageRecorded Signal

The agtrs runtime emits `UserUsageRecorded` after each LLM API call:

```rust
/// Signal: user usage recorded after an LLM call
#[derive(Debug, Clone)]
pub struct UserUsageRecorded {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub agent_id: AgentId,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

### ModelCallComplete Signal

```rust
/// Signal: a model call completed
#[derive(Debug, Clone)]
pub struct ModelCallComplete {
    pub model: String,
    pub agent_id: AgentId,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency: Duration,
    pub cost: f64,
    pub success: bool,
}
```

### Signal Handler

```rust
impl CostState {
    /// Handle a UserUsageRecorded signal
    pub fn handle_usage_recorded(&mut self, record: &UserUsageRecorded) {
        let total = record.input_tokens + record.output_tokens;
        let cost = self.price_table.calculate_cost(
            &record.model,
            record.input_tokens,
            record.output_tokens,
        );

        // Update totals
        self.total_tokens += total;
        self.total_cost += cost;

        // Update model breakdown
        let model_entry = self.model_breakdown
            .entry(record.model.clone())
            .or_insert_with(|| ModelCost {
                model_id: record.model.clone(),
                display_name: Self::short_model_name(&record.model),
                tier: Self::classify_tier(&record.model),
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cost: 0.0,
                call_count: 0,
            });

        model_entry.input_tokens += record.input_tokens;
        model_entry.output_tokens += record.output_tokens;
        model_entry.total_tokens += total;
        model_entry.cost += cost;
        model_entry.call_count += 1;

        // Update agent breakdown
        let agent_entry = self.agent_breakdown
            .entry(record.agent_id)
            .or_insert_with(|| AgentCost {
                agent_id: record.agent_id,
                agent_name: String::new(), // Will be filled from TaskTree
                total_tokens: 0,
                cost: 0.0,
                call_count: 0,
                is_active: true,
            });

        agent_entry.total_tokens += total;
        agent_entry.cost += cost;
        agent_entry.call_count += 1;

        // Update rate trackers
        self.token_rate.record(total as f64);
        self.cost_rate.record(cost);

        // Update projection
        self.update_projection();

        // Check budget
        self.check_budget();
    }

    fn short_model_name(model: &str) -> String {
        // "claude-sonnet-4-20250514" → "claude-sonnet"
        model.split('-').take(2).collect::<Vec<_>>().join("-")
    }

    fn classify_tier(model: &str) -> ModelTier {
        let cheap_patterns = ["haiku", "mini", "flash", "lite"];
        if cheap_patterns.iter().any(|p| model.contains(p)) {
            ModelTier::Cheap
        } else {
            ModelTier::Flagship
        }
    }
}
```

## Rate Tracking

### Sliding-Window Rate Calculator

```rust
/// Rate tracker using a sliding window
#[derive(Debug, Clone)]
pub struct RateTracker {
    /// Events in the last N seconds (timestamp, value)
    events: VecDeque<(Instant, f64)>,

    /// Window duration
    window: Duration,
}

impl RateTracker {
    pub fn new(window: Duration) -> Self {
        Self {
            events: VecDeque::new(),
            window,
        }
    }

    /// Record a new event
    pub fn record(&mut self, value: f64) {
        let now = Instant::now();
        self.events.push_back((now, value));
        self.prune(now);
    }

    /// Get the current rate (value per second)
    pub fn rate(&mut self) -> f64 {
        let now = Instant::now();
        self.prune(now);

        if self.events.is_empty() { return 0.0; }

        let total: f64 = self.events.iter().map(|(_, v)| v).sum();
        let duration_secs = self.window.as_secs_f64();

        total / duration_secs
    }

    /// Remove events outside the window
    fn prune(&mut self, now: Instant) {
        let cutoff = now - self.window;
        while self.events.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            self.events.pop_front();
        }
    }
}
```

## Cost Projection

### Linear Regression Projection

xaft projects the total cost at session end using a simple linear model:

```rust
impl CostState {
    fn update_projection(&mut self) {
        let elapsed = self.session_start.elapsed();
        if elapsed.as_secs() < 30 {
            // Not enough data for projection
            self.projection = CostProjection {
                estimated_total: self.total_cost,
                estimated_time: Duration::ZERO,
                confidence: 0.0,
            };
            return;
        }

        // Current burn rate (USD per minute)
        let cost_per_minute = self.cost_rate.rate() * 60.0;

        // Estimate remaining time based on task progress
        // (This is a heuristic — we don't know exactly when the agent will finish)
        let elapsed_minutes = elapsed.as_secs_f64() / 60.0;
        let tokens_per_minute = self.token_rate.rate() * 60.0;

        // Heuristic: assume 60% of total work is done when we've seen
        // a stable token rate for >2 minutes
        let progress_estimate = if elapsed_minutes > 2.0 { 0.6 } else { 0.3 };
        let remaining_fraction = 1.0 - progress_estimate;
        let estimated_remaining_minutes = (elapsed_minutes * remaining_fraction) / progress_estimate;

        let estimated_remaining_cost = cost_per_minute * estimated_remaining_minutes;
        let estimated_total = self.total_cost + estimated_remaining_cost;

        // Confidence increases with time
        let confidence = (elapsed_minutes / 5.0).min(1.0);

        self.projection = CostProjection {
            estimated_total,
            estimated_time: Duration::from_secs_f64(estimated_remaining_minutes * 60.0),
            confidence,
        };
    }
}
```

## Budget Management

### Budget Bar Widget

```rust
/// Render the budget bar
fn render_budget_bar(state: &CostState, area: Rect, buf: &mut Buffer) {
    match state.budget_limit {
        Some(limit) => {
            let fraction = (state.total_cost / limit).min(1.0);
            let remaining = limit - state.total_cost;

            let (bar_color, bar_style) = if fraction > 0.9 {
                (Color::Red, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            } else if fraction > 0.7 {
                (Color::Yellow, Style::default().fg(Color::Yellow))
            } else {
                (Color::Green, Style::default().fg(Color::Green))
            };

            let gauge = LineGauge::default()
                .gauge_style(Style::default().fg(bar_color).bg(Color::DarkGray))
                .ratio(fraction)
                .label(Line::from(vec![
                    Span::styled(
                        format!(" Budget: ${:.2} ", limit),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!("(${:.2} remaining) ", remaining),
                        bar_style,
                    ),
                ]));
            gauge.render(area, buf);
        }
        None => {
            // No budget set — show total cost only
            let line = Line::from(vec![
                Span::styled(" Cost: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("${:.2}", state.total_cost),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" (no budget set — press B to configure)", Style::default().fg(Color::DarkGray)),
            ]);
            line.render(area, buf);
        }
    }
}
```

### Budget Warnings

```rust
impl CostState {
    fn check_budget(&self) -> BudgetStatus {
        match self.budget_limit {
            Some(limit) => {
                let fraction = self.total_cost / limit;
                if fraction >= 1.0 {
                    BudgetStatus::Exceeded
                } else if fraction >= 0.9 {
                    BudgetStatus::Critical
                } else if fraction >= 0.75 {
                    BudgetStatus::Warning
                } else {
                    BudgetStatus::Ok
                }
            }
            None => BudgetStatus::NoBudget,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetStatus {
    Ok,       // < 75% of budget
    Warning,  // 75-90% of budget
    Critical, // 90-100% of budget
    Exceeded, // > 100% of budget
    NoBudget, // No budget configured
}
```

When budget status changes, the TUI shows a notification:

```
┌──────────────────────────────────────────────────────────────┐
│  ⚠ BUDGET WARNING: 78% of $5.00 budget used ($3.90 / $5.00)│
│                                                              │
│  Current rate: $0.12/min                                     │
│  Projection:  $3.20 additional (total: $7.10)               │
│                                                              │
│  [B] Increase budget  [S] Stop agent  [D] Dismiss           │
└──────────────────────────────────────────────────────────────┘
```

## Per-Agent Cost Breakdown

### Agent Cost Table

```
┌─ Per-Agent Cost Breakdown ───────────────────────────────────┐
│                                                              │
│  Agent              Tokens     Cost    Calls   % of Total    │
│  ─────────────────────────────────────────────────────────── │
│  ● coordinator      45,120   $0.78      12     41.7%        │
│  ● file-editor-01   52,300   $0.72      18     38.5%        │
│  ○ bash-runner-01    8,450   $0.12       6      6.4%        │
│  ✓ researcher-01    18,712   $0.25       8     13.4%        │
│  ─────────────────────────────────────────────────────────── │
│  TOTAL             124,582   $1.87      44    100.0%        │
│                                                              │
│  Sort: [1] by cost [2] by tokens [3] by calls [4] by %      │
└──────────────────────────────────────────────────────────────┘
```

```rust
/// Render per-agent cost breakdown table
fn render_agent_breakdown(state: &CostState, area: Rect, buf: &mut Buffer) {
    let mut agents: Vec<&AgentCost> = state.agent_breakdown.values().collect();
    agents.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));

    // Header
    let header = Line::from(vec![
        Span::styled(" Agent             ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled("Tokens    ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled("Cost   ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled("Calls  ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        Span::styled("% Total", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
    ]);
    header.render(area, buf);

    // Separator
    let sep = "─".repeat(area.width as usize);
    Span::styled(sep, Style::default().fg(Color::DarkGray))
        .render(Rect::new(area.x, area.y + 1, area.width, 1), buf);

    // Rows
    for (i, agent) in agents.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.bottom() - 2 { break; }

        let pct = if state.total_cost > 0.0 {
            agent.cost / state.total_cost * 100.0
        } else {
            0.0
        };

        let icon = if agent.is_active { "●" } else { "○" };
        let icon_color = if agent.is_active { Color::Yellow } else { Color::DarkGray };

        // Mini bar for percentage
        let bar_width = 10u16;
        let filled = (bar_width as f64 * pct / 100.0).round() as u16;

        let row = Line::from(vec![
            Span::styled(format!(" {} ", icon), Style::default().fg(icon_color)),
            Span::styled(format!("{:<18}", agent.agent_name), Style::default().fg(Color::White)),
            Span::styled(format!("{:>8} ", format_tokens(agent.total_tokens)), Style::default().fg(Color::Gray)),
            Span::styled(format!("${:>5.2} ", agent.cost), Style::default().fg(Color::White)),
            Span::styled(format!("{:>5} ", agent.call_count), Style::default().fg(Color::Gray)),
            Span::styled("█".repeat(filled as usize), Style::default().fg(Color::Cyan)),
            Span::styled("░".repeat((bar_width - filled) as usize), Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {:.1}%", pct), Style::default().fg(Color::Gray)),
        ]);
        row.render(Rect::new(area.x, y, area.width, 1), buf);
    }

    // Total row
    let total_y = area.bottom() - 1;
    let total_row = Line::from(vec![
        Span::styled(" TOTAL             ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>8} ", format_tokens(state.total_tokens)), Style::default().fg(Color::White)),
        Span::styled(format!("${:>5.2} ", state.total_cost), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>5} ", state.agent_breakdown.values().map(|a| a.call_count).sum::<u64>()), Style::default().fg(Color::Gray)),
        Span::styled("100.0%", Style::default().fg(Color::Gray)),
    ]);
    total_row.render(Rect::new(area.x, total_y, area.width, 1), buf);
}

/// Format large token counts with commas
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}
```

## Model Routing Indicator

### Flagship vs Cheap Display

xaft shows which model tier is currently being used, so users can spot when expensive
models are being used for trivial tasks:

```
Model: ● claude-sonnet (flagship) │ ○ claude-haiku (cheap)
       ████████████████████████████░░░░░░  82% flagship usage
```

```rust
/// Render model routing indicator
fn render_model_routing(state: &CostState, area: Rect, buf: &mut Buffer) {
    let flagship_tokens: u64 = state.model_breakdown.values()
        .filter(|m| m.tier == ModelTier::Flagship)
        .map(|m| m.total_tokens)
        .sum();

    let cheap_tokens: u64 = state.model_breakdown.values()
        .filter(|m| m.tier == ModelTier::Cheap)
        .map(|m| m.total_tokens)
        .sum();

    let total = flagship_tokens + cheap_tokens;
    let flagship_pct = if total > 0 { flagship_tokens as f64 / total as f64 * 100.0 } else { 0.0 };

    let line = Line::from(vec![
        Span::styled(" Model: ", Style::default().fg(Color::Gray)),
        Span::styled("● ", Style::default().fg(Color::Cyan)),
        Span::styled("sonnet (flagship)", Style::default().fg(Color::Cyan)),
        Span::raw(" │ "),
        Span::styled("○ ", Style::default().fg(Color::DarkGray)),
        Span::styled("haiku (cheap)", Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        // Mini bar
        Span::styled(
            "█".repeat((flagship_pct / 100.0 * 20.0).round() as usize),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            "░".repeat((20 - (flagship_pct / 100.0 * 20.0).round() as usize).max(0) as usize),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!(" {:.0}% flagship", flagship_pct), Style::default().fg(Color::Gray)),
    ]);
    line.render(area, buf);
}
```

## Speed Metrics

### Token Speed Display

```
Speed: 842 tok/s │ Rate: $0.12/min │ Projection: $3.20
```

```rust
/// Render speed metrics
fn render_speed_metrics(state: &CostState, area: Rect, buf: &mut Buffer) {
    let tok_per_sec = state.token_rate.rate();
    let cost_per_min = state.cost_rate.rate() * 60.0;

    let projection_text = if state.projection.confidence > 0.3 {
        format!("${:.2}", state.projection.estimated_total)
    } else {
        "calculating...".into()
    };

    let line = Line::from(vec![
        Span::styled(" Speed: ", Style::default().fg(Color::Gray)),
        Span::styled(format!("{:.0} tok/s", tok_per_sec), Style::default().fg(Color::White)),
        Span::raw(" │ "),
        Span::styled("Rate: ", Style::default().fg(Color::Gray)),
        Span::styled(format!("${:.2}/min", cost_per_min), Style::default().fg(Color::White)),
        Span::raw(" │ "),
        Span::styled("Projection: ", Style::default().fg(Color::Gray)),
        Span::styled(projection_text, Style::default().fg(
            if state.projection.estimated_total > state.budget_limit.unwrap_or(f64::MAX) {
                Color::Red
            } else {
                Color::White
            }
        )),
    ]);
    line.render(area, buf);
}
```

## Compact Mode

When the TokenDashboard has limited space (e.g., in the sidebar), it renders in
compact mode:

```
┌─ Costs ──────────────┐
│ 124.5Kt │ $1.87      │
│ ████████░░ $5.00     │
│ 842t/s │ $0.12/min  │
│ ● sonnet 82% │ ○ h   │
└───────────────────────┘
```

```rust
/// Compact cost display for narrow panes
fn render_compact(state: &CostState, area: Rect, buf: &mut Buffer) {
    // Line 1: Tokens and cost
    let line1 = Line::from(vec![
        Span::styled(format_tokens(state.total_tokens), Style::default().fg(Color::White)),
        Span::raw(" │ "),
        Span::styled(format!("${:.2}", state.total_cost), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]);
    line1.render(Rect::new(area.x, area.y, area.width, 1), buf);

    // Line 2: Budget bar
    if let Some(limit) = state.budget_limit {
        let fraction = (state.total_cost / limit).min(1.0);
        let color = if fraction > 0.9 { Color::Red } else if fraction > 0.7 { Color::Yellow } else { Color::Green };
        let filled = (area.width as f64 * fraction).round() as u16;
        for x in area.x..area.x + filled {
            buf.get_mut(x, area.y + 1).set_char('█').set_style(Style::default().fg(color));
        }
        for x in area.x + filled..area.x + area.width {
            buf.get_mut(x, area.y + 1).set_char('░').set_style(Style::default().fg(Color::DarkGray));
        }
    }

    // Line 3: Speed and rate
    let line3 = Line::from(vec![
        Span::styled(format!("{:.0}t/s", state.token_rate.rate()), Style::default().fg(Color::Gray)),
        Span::raw(" │ "),
        Span::styled(format!("${:.2}/m", state.cost_rate.rate() * 60.0), Style::default().fg(Color::Gray)),
    ]);
    line3.render(Rect::new(area.x, area.y + 2, area.width, 1), buf);
}
```

## Cost History Chart

When the TokenDashboard has extra vertical space, it renders a sparkline chart of
cumulative cost over time:

```
  Cost over time:
  $2.00 ┤                              ╭──
  $1.50 ┤                    ╭─────────╯
  $1.00 ┤           ╭────────╯
  $0.50 ┤     ╭─────╯
  $0.00 ┼─────╯
        0m    2m    4m    6m    8m   10m   12m
```

```rust
/// Render cost sparkline
fn render_cost_sparkline(history: &[(Instant, f64)], area: Rect, buf: &mut Buffer) {
    if history.len() < 2 { return; }

    let max_cost = history.iter().map(|(_, c)| *c).fold(0.0f64, f64::max);
    if max_cost == 0.0 { return; }

    // Bucket history into area.width data points
    let bucket_count = area.width as usize;
    let time_range = history.last().unwrap().0 - history.first().unwrap().0;
    let bucket_duration = time_range / bucket_count as u32;

    let mut buckets = vec![0.0f64; bucket_count];
    for (time, cost) in history {
        let bucket_idx = ((time - history.first().unwrap().0) / bucket_duration)
            .min((bucket_count - 1) as u32) as usize;
        buckets[bucket_idx] = buckets[bucket_idx].max(*cost);
    }

    // Render sparkline
    let sparkline = Sparkline::default()
        .data(&buckets.iter().map(|b| (b / max_cost * area.height as f64) as u64).collect::<Vec<_>>())
        .style(Style::default().fg(Color::Cyan));
    sparkline.render(area, buf);
}
```
