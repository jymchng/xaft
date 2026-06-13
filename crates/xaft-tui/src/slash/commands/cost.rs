//! /cost command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::table::CliTable;
use crate::slash::{CommandContext, CommandResult};

pub struct CostHandler;

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{} {}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}

impl SlashHandler for CostHandler {
    fn description(&self) -> &'static str {
        "Show per-agent token and cost table"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let stats = match ctx.llm_stats.read() {
            Ok(s) => s,
            Err(_) => return CommandResult::Error("Failed to read agent stats.".into()),
        };

        if stats.is_empty() {
            return CommandResult::Lines(vec!["No LLM calls recorded yet.".into()]);
        }

        let mut table = CliTable::new(["AGENT", "CALLS", "IN", "OUT", "CACHE_R", "COST"]);
        let (mut ti, mut to, mut tc, mut cost, mut calls) = (0u64, 0u64, 0u64, 0f64, 0u32);

        for (agent, s) in stats.iter() {
            table.push_row([
                agent,
                &s.calls.to_string(),
                &fmt_tokens(s.input_tokens),
                &fmt_tokens(s.output_tokens),
                &fmt_tokens(s.cache_read),
                &format!("${:.4}", s.cost_usd),
            ]);
            ti += s.input_tokens;
            to += s.output_tokens;
            tc += s.cache_read;
            cost += s.cost_usd;
            calls += s.calls;
        }

        table.push_separator();
        table.push_row([
            "TOTAL",
            &calls.to_string(),
            &fmt_tokens(ti),
            &fmt_tokens(to),
            &fmt_tokens(tc),
            &format!("${:.4}", cost),
        ]);

        CommandResult::StyledLines(table.render(ctx.terminal_cols))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStats, AgentStatsMap, CommandContext, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn test_ctx_with_stats(args: &str, stats: AgentStatsMap) -> CommandContext {
        CommandContext {
            args: args.into(),
            command: SlashCommand::Cost,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(stats)),
            conversation_store: None,
            session_store: None,
        }
    }

    fn result_to_string(result: &CommandResult) -> String {
        match result {
            CommandResult::Lines(ls) => ls.join("\n"),
            CommandResult::StyledLines(ls) => ls
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            CommandResult::Error(e) => e.clone(),
            CommandResult::Handled => String::new(),
            CommandResult::ConfigDisplay(_) => "[config display]".to_string(),
            CommandResult::OpenMenu(_) => "[menu]".to_string(),
        }
    }

    #[test]
    fn cost_handler_empty_stats() {
        let handler = CostHandler;
        let ctx = test_ctx_with_stats("", AgentStatsMap::new());
        let result = handler.execute(ctx);
        let text = result_to_string(&result);
        assert!(
            text.contains("No LLM calls recorded yet"),
            "text was: {text}"
        );
    }

    #[test]
    fn cost_handler_renders_table() {
        let mut stats = AgentStatsMap::new();
        stats.insert(
            "planner".into(),
            AgentStats {
                input_tokens: 1000,
                output_tokens: 200,
                cache_read: 0,
                cache_write: 0,
                cost_usd: 0.005,
                calls: 1,
            },
        );
        let handler = CostHandler;
        let ctx = test_ctx_with_stats("", stats);
        let result = handler.execute(ctx);
        let text = result_to_string(&result);
        assert!(text.contains("planner"), "text was:\n{text}");
        assert!(
            text.contains("0.0050") || text.contains("$0.005"),
            "text was:\n{text}"
        );
        assert!(text.contains("TOTAL"), "text was:\n{text}");
    }
}
