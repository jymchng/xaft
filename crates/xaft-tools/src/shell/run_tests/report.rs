//! Turning a test runner's output into one structured report.
//!
//! agenthicc's `RunTestsTool` knows one runner: it reads pytest's
//! `--json-report` file and falls back to two regexes over stdout
//! (`(\d+) passed`, `(\d+) failed`). xaft has to answer for whatever the
//! project uses, so each ecosystem gets the same treatment — a structured
//! source where the runner offers one (pytest's JSON report, `go test -json`)
//! and a line parser over the runner's own summary format otherwise.
//!
//! Every parser returns a [`TestReport`]. A count the output does not state is
//! `None` rather than `0`: agenthicc reports `null` when its regex finds
//! nothing, and an agent must be able to tell "no tests failed" from "the run
//! never got far enough to say".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

/// Failures listed in one report; the rest are counted in `failures_omitted`.
pub const MAX_FAILURES: usize = 50;
/// Characters kept from one failure message.
pub const MAX_MESSAGE_CHARS: usize = 2_000;

/// Where a report's numbers came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReportSource {
    /// A machine-readable report the runner wrote (pytest's JSON report,
    /// `go test -json`).
    Structured,
    /// The runner's human-readable output.
    Output,
    /// Nothing recognisable was printed.
    #[default]
    None,
}

impl ReportSource {
    /// Wire name used in the tool result.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structured => "structured",
            Self::Output => "output",
            Self::None => "none",
        }
    }
}

/// One failing (or erroring) test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    /// The runner's name for the test (`tests::adds`, a pytest node id, …).
    pub name: String,
    /// The assertion or error message, clipped to [`MAX_MESSAGE_CHARS`].
    pub message: String,
}

/// Counts and failures parsed from one run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TestReport {
    /// Tests that passed.
    pub passed: Option<u64>,
    /// Tests that failed.
    pub failed: Option<u64>,
    /// Tests skipped, ignored, pending or expected to fail.
    pub skipped: Option<u64>,
    /// Errors that stopped tests from running: a compile failure, a pytest
    /// collection error, a Go package that did not build.
    pub errors: Option<u64>,
    /// Failing tests, in output order, at most [`MAX_FAILURES`].
    pub failures: Vec<TestFailure>,
    /// Failures beyond [`MAX_FAILURES`].
    pub failures_omitted: usize,
    /// Where the numbers came from.
    pub source: ReportSource,
}

impl TestReport {
    /// Sum of every stated count, or `None` when nothing was stated.
    pub fn total(&self) -> Option<u64> {
        let counts = [self.passed, self.failed, self.skipped];
        if counts.iter().all(Option::is_none) {
            return None;
        }
        Some(counts.iter().flatten().sum())
    }

    /// Record a failure, keeping the list bounded.
    pub fn push_failure(&mut self, name: impl Into<String>, message: impl AsRef<str>) {
        let name = name.into();
        if self.failures.iter().any(|f| f.name == name) {
            return;
        }
        if self.failures.len() >= MAX_FAILURES {
            self.failures_omitted += 1;
            return;
        }
        self.failures.push(TestFailure {
            name,
            message: clip(message.as_ref().trim(), MAX_MESSAGE_CHARS),
        });
    }

    /// Attach `message` to an already-recorded failure that has none.
    fn set_message(&mut self, name: &str, message: &str) {
        if let Some(f) = self.failures.iter_mut().find(|f| f.name == name) {
            if f.message.is_empty() {
                f.message = clip(message.trim(), MAX_MESSAGE_CHARS);
            }
        }
    }

    /// The report's fields as JSON, merged into a tool result.
    pub fn to_json(&self) -> Value {
        json!({
            "passed": self.passed,
            "failed": self.failed,
            "skipped": self.skipped,
            "errors": self.errors,
            "total": self.total(),
            "failures": self
                .failures
                .iter()
                .map(|f| json!({"name": f.name, "message": f.message}))
                .collect::<Vec<_>>(),
            "failures_omitted": self.failures_omitted,
            "report": self.source.as_str(),
        })
    }
}

/// `text` clipped to `max` characters, marking the cut.
pub fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => format!("{}…", &text[..cut]),
    }
}

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("ansi regex"));

/// `text` without terminal colour codes. Runners are asked not to emit them
/// (`NO_COLOR`, `CARGO_TERM_COLOR=never`), but not every runner listens.
pub fn strip_ansi(text: &str) -> String {
    ANSI.replace_all(text, "").into_owned()
}

fn count(caps: &regex::Captures<'_>, i: usize) -> u64 {
    caps.get(i)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0)
}

// ── cargo / libtest ──────────────────────────────────────────────────────────

static CARGO_TEST_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^test (.+?) \.\.\. (ok|FAILED|ignored\b.*|bench:.*)$").expect("cargo test line")
});
static CARGO_RESULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured",
    )
    .expect("cargo result line")
});
static CARGO_BLOCK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---- (.+?) stdout ----$").expect("cargo block header"));
static CARGO_DIAGNOSTIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error(?:\[E\d{4}\])?: (.*)$").expect("cargo diagnostic"));
static CARGO_COULD_NOT_COMPILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^error: could not compile `([^`]+)`(?: \(([^)]*)\))?").expect("could not compile")
});
static CARGO_TARGETS_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error: \d+ targets? failed").expect("targets failed"));

/// Is this `error:` line cargo narrating a test failure rather than a build
/// or usage error?
fn cargo_error_is_narration(line: &str) -> bool {
    line.starts_with("error: test failed")
        || line.starts_with("error: could not compile")
        || line.starts_with("error: aborting due to")
        || CARGO_TARGETS_FAILED.is_match(line)
}

/// `---- name stdout ----` blocks: the captured output of each failing test.
fn cargo_failure_blocks(stdout: &str) -> BTreeMap<String, String> {
    let mut blocks = BTreeMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    let flush = |current: &mut Option<(String, Vec<&str>)>,
                 blocks: &mut BTreeMap<String, String>| {
        if let Some((name, lines)) = current.take() {
            blocks.insert(name, cargo_block_message(&lines));
        }
    };
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(caps) = CARGO_BLOCK_HEADER.captures(line) {
            flush(&mut current, &mut blocks);
            current = Some((caps[1].to_string(), Vec::new()));
            continue;
        }
        // The name list (`failures:` / `successes:`) and the summary end the
        // last block.
        if line == "failures:" || line == "successes:" || line.starts_with("test result:") {
            flush(&mut current, &mut blocks);
            continue;
        }
        if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

/// The useful part of a failing test's captured output: the panic and what
/// follows it, without the backtrace hint.
fn cargo_block_message(lines: &[&str]) -> String {
    let start = lines
        .iter()
        .position(|l| l.starts_with("thread '") && l.contains("panicked at"))
        .unwrap_or(0);
    lines[start..]
        .iter()
        .filter(|l| !l.starts_with("note: run with `RUST_BACKTRACE"))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Parse `cargo test` output: libtest's per-test and summary lines on stdout,
/// cargo's build diagnostics on stderr.
pub fn parse_cargo(stdout: &str, stderr: &str) -> TestReport {
    let stdout = strip_ansi(stdout);
    let stderr = strip_ansi(stderr);
    let mut report = TestReport::default();

    let (mut sum_passed, mut sum_failed, mut sum_ignored) = (0u64, 0u64, 0u64);
    let mut saw_summary = false;
    let (mut line_passed, mut line_failed, mut line_ignored) = (0u64, 0u64, 0u64);
    let mut saw_test_line = false;

    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(caps) = CARGO_RESULT.captures(line) {
            saw_summary = true;
            sum_passed += count(&caps, 1);
            sum_failed += count(&caps, 2);
            sum_ignored += count(&caps, 3);
        } else if let Some(caps) = CARGO_TEST_LINE.captures(line) {
            saw_test_line = true;
            let status = &caps[2];
            if status == "ok" || status.starts_with("bench:") {
                line_passed += 1;
            } else if status == "FAILED" {
                line_failed += 1;
                report.push_failure(&caps[1], "");
            } else {
                line_ignored += 1;
            }
        }
    }

    for (name, message) in cargo_failure_blocks(&stdout) {
        report.set_message(&name, &message);
    }

    // Build and usage errors live on stderr.
    let mut diagnostics: Vec<String> = Vec::new();
    let mut not_compiled: Vec<String> = Vec::new();
    let lines: Vec<&str> = stderr.lines().map(str::trim_end).collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = CARGO_COULD_NOT_COMPILE.captures(line) {
            let target = match caps.get(2) {
                Some(kind) => format!("{} ({})", &caps[1], kind.as_str()),
                None => caps[1].to_string(),
            };
            not_compiled.push(target);
        } else if CARGO_DIAGNOSTIC.is_match(line) && !cargo_error_is_narration(line) {
            // The diagnostic plus its source excerpt, up to the blank line
            // rustc ends every diagnostic with.
            let block: Vec<&str> = lines[i..]
                .iter()
                .take_while(|l| !l.is_empty())
                .take(12)
                .copied()
                .collect();
            diagnostics.push(block.join("\n"));
        }
    }
    report.errors = Some(diagnostics.len() as u64);
    if !not_compiled.is_empty() || (!diagnostics.is_empty() && !saw_summary && !saw_test_line) {
        let message = diagnostics
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        if not_compiled.is_empty() {
            report.push_failure("cargo", &message);
        }
        for target in not_compiled {
            report.push_failure(format!("build: {target}"), &message);
        }
    }

    if saw_summary {
        report.passed = Some(sum_passed);
        report.failed = Some(sum_failed);
        report.skipped = Some(sum_ignored);
        report.source = ReportSource::Output;
    } else if saw_test_line {
        report.passed = Some(line_passed);
        report.failed = Some(line_failed);
        report.skipped = Some(line_ignored);
        report.source = ReportSource::Output;
    }
    report
}

// ── pytest ───────────────────────────────────────────────────────────────────

/// Parse pytest-json-report's file (`--json-report-file`).
///
/// agenthicc reads `summary.passed`, `summary.failed` and `summary.error`;
/// this also reads `skipped` (plus `xfailed`, which pytest's own JUnit report
/// files as skipped) and lists each failing test with its crash message.
/// Returns `None` when the document has no `summary` — agenthicc's cue to fall
/// back to the regexes.
pub fn parse_pytest_json(doc: &Value) -> Option<TestReport> {
    let summary = doc.get("summary")?.as_object()?;
    let n = |key: &str| summary.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut report = TestReport {
        passed: Some(n("passed") + n("xpassed")),
        failed: Some(n("failed")),
        skipped: Some(n("skipped") + n("xfailed")),
        errors: Some(n("error")),
        source: ReportSource::Structured,
        ..TestReport::default()
    };

    for collector in doc
        .get("collectors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if collector.get("outcome").and_then(Value::as_str) == Some("failed") {
            let name = collector
                .get("nodeid")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("collection");
            let message = collector
                .get("longrepr")
                .and_then(Value::as_str)
                .unwrap_or("");
            report.push_failure(name, last_lines(message, 12));
        }
    }

    for test in doc
        .get("tests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let outcome = test.get("outcome").and_then(Value::as_str).unwrap_or("");
        if outcome != "failed" && outcome != "error" {
            continue;
        }
        let name = test.get("nodeid").and_then(Value::as_str).unwrap_or("?");
        let message = ["call", "setup", "teardown"]
            .iter()
            .filter_map(|stage| test.get(*stage))
            .find_map(|stage| {
                stage
                    .get("crash")
                    .and_then(|c| c.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        stage
                            .get("longrepr")
                            .and_then(Value::as_str)
                            .map(|s| last_lines(s, 12))
                    })
            })
            .unwrap_or_default();
        report.push_failure(name, message);
    }
    Some(report)
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

static PYTEST_COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (passed|failed|skipped|errors?|xfailed|xpassed|deselected|warnings?|rerun)")
        .expect("pytest count")
});
static PYTEST_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^=*\s*(?:(?:\d+ \w+,? )+|no tests ran )in [\d.]+s").expect("pytest summary")
});
static PYTEST_SHORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(FAILED|ERROR) (\S+)(?: - (.*))?$").expect("pytest short"));

/// agenthicc's fallback, made precise: the counts come from pytest's final
/// summary line (`2 failed, 10 passed in 0.52s`), not from the first
/// `(\d+) passed` anywhere in stdout — a test that prints "3 passed" would
/// otherwise set the count. Failures come from the short test summary
/// (`FAILED node::id - message`), which pytest prints by default.
pub fn parse_pytest_output(stdout: &str, stderr: &str) -> TestReport {
    let stdout = strip_ansi(stdout);
    let mut report = TestReport::default();

    if let Some(line) = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| PYTEST_SUMMARY.is_match(l))
    {
        let (mut passed, mut failed, mut skipped, mut errors) = (0, 0, 0, 0);
        for caps in PYTEST_COUNT.captures_iter(line) {
            let n = count(&caps, 1);
            match &caps[2] {
                "passed" | "xpassed" => passed += n,
                "failed" => failed += n,
                "skipped" | "xfailed" => skipped += n,
                "error" | "errors" => errors += n,
                _ => {}
            }
        }
        report.passed = Some(passed);
        report.failed = Some(failed);
        report.skipped = Some(skipped);
        report.errors = Some(errors);
        report.source = ReportSource::Output;
    }

    for line in stdout.lines() {
        if let Some(caps) = PYTEST_SHORT.captures(line.trim_end()) {
            report.push_failure(&caps[2], caps.get(3).map_or("", |m| m.as_str()));
        }
    }

    // A usage error (bad flag, missing path) never reaches a summary line.
    if report.source == ReportSource::None {
        let stderr = strip_ansi(stderr);
        if let Some(line) = stderr.lines().find(|l| l.contains("error:")) {
            report.errors = Some(1);
            report.push_failure("pytest", line.trim());
        }
    }
    report
}

// ── unittest ─────────────────────────────────────────────────────────────────

static UNITTEST_RAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Ran (\d+) tests? in").expect("unittest ran"));
static UNITTEST_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(OK|FAILED)(?: \((.*)\))?\s*$").expect("unittest result"));
static UNITTEST_DETAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(failures|errors|skipped|expected failures|unexpected successes)=(\d+)")
        .expect("unittest detail")
});
static UNITTEST_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(FAIL|ERROR): (.+)$").expect("unittest header"));

/// Parse `python -m unittest` output (it reports on stderr).
pub fn parse_unittest(stdout: &str, stderr: &str) -> TestReport {
    let text = strip_ansi(&format!("{stdout}\n{stderr}"));
    let mut report = TestReport::default();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();

    let ran = lines
        .iter()
        .find_map(|l| UNITTEST_RAN.captures(l).map(|c| count(&c, 1)));
    let result = lines.iter().rev().find_map(|l| UNITTEST_RESULT.captures(l));
    if let (Some(ran), Some(caps)) = (ran, result) {
        let (mut failures, mut errors, mut skipped, mut expected, mut unexpected) = (0, 0, 0, 0, 0);
        if let Some(detail) = caps.get(2) {
            for d in UNITTEST_DETAIL.captures_iter(detail.as_str()) {
                let n = count(&d, 2);
                match &d[1] {
                    "failures" => failures = n,
                    "errors" => errors = n,
                    "skipped" => skipped = n,
                    "expected failures" => expected = n,
                    _ => unexpected = n,
                }
            }
        }
        report.failed = Some(failures);
        report.errors = Some(errors);
        report.skipped = Some(skipped + expected);
        let not_passed = failures + errors + skipped + expected;
        report.passed = Some((ran.saturating_sub(not_passed) + unexpected).min(ran));
        report.source = ReportSource::Output;
    }

    // `FAIL: test_x (module.Class.test_x)` + traceback, separated by rules.
    let mut i = 0;
    while i < lines.len() {
        if let Some(caps) = UNITTEST_HEADER.captures(lines[i]) {
            let name = caps[2].to_string();
            let body: Vec<&str> = lines[i + 1..]
                .iter()
                .take_while(|l| !l.starts_with("=====") && !UNITTEST_RAN.is_match(l))
                .filter(|l| !l.starts_with("-----"))
                .copied()
                .collect();
            let message = body
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            report.push_failure(name, message);
            i += 1 + body.len();
        } else {
            i += 1;
        }
    }
    report
}

// ── JavaScript: jest, vitest, mocha, node:test ──────────────────────────────

static JEST_TESTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Tests:\s+(.*\d+ total.*)$").expect("jest tests"));
static JEST_COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (failed|passed|skipped|todo|pending)").expect("jest count")
});
static JEST_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*● (.+)$").expect("jest failure"));
static VITEST_TESTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*Tests\s+(\d+ .*)$").expect("vitest tests"));
static VITEST_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*FAIL\s+(.+ > .+)$").expect("vitest failure"));
static MOCHA_PASSING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+) passing\b").expect("mocha passing"));
static MOCHA_FAILING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+) failing\b").expect("mocha failing"));
static MOCHA_PENDING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+) pending\b").expect("mocha pending"));
static MOCHA_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+\d+\) (.+)$").expect("mocha item"));
static NODE_COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:#|ℹ) (tests|pass|fail|skipped|todo|cancelled) (\d+)\s*$")
        .expect("node count")
});
static TAP_NOT_OK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*not ok \d+ - (.+?)(?: # .*)?$").expect("tap not ok"));
static TAP_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*error: (.+)$").expect("tap error"));
static NODE_SPEC_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*✖ (.+?)(?: \([\d.]+m?s\))?$").expect("node spec failure"));
static CODE_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>?\s*\d+ \|").expect("code frame"));

/// The first few lines of a failure's explanation, stopping at a code frame
/// or stack trace.
fn message_after(lines: &[&str], start: usize, max_lines: usize) -> String {
    lines[start.min(lines.len())..]
        .iter()
        .map(|l| l.trim())
        .skip_while(|l| l.is_empty())
        .take_while(|l| {
            !l.starts_with("at ")
                && !l.starts_with('❯')
                && !l.starts_with('⎯')
                && !l.starts_with('●')
                && !CODE_FRAME.is_match(l)
        })
        .filter(|l| !l.is_empty())
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse `npm test` / `yarn test` / `pnpm test` output from whichever runner
/// the project's `test` script uses: jest, vitest, mocha or `node --test`.
pub fn parse_node(stdout: &str, stderr: &str) -> TestReport {
    let text = strip_ansi(&format!("{stdout}\n{stderr}"));
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut report = TestReport::default();

    if let Some(summary) = lines.iter().rev().find_map(|l| JEST_TESTS.captures(l)) {
        set_js_counts(&mut report, &summary[1]);
        for (i, line) in lines.iter().enumerate() {
            if let Some(caps) = JEST_FAILURE.captures(line) {
                let name = caps[1].trim();
                if name == "Test suite failed to run" {
                    report.errors = Some(report.errors.unwrap_or(0) + 1);
                }
                report.push_failure(name, message_after(&lines, i + 1, 6));
            }
        }
        return report;
    }

    if let Some(summary) = lines.iter().rev().find_map(|l| VITEST_TESTS.captures(l)) {
        set_js_counts(&mut report, &summary[1]);
        for (i, line) in lines.iter().enumerate() {
            if let Some(caps) = VITEST_FAILURE.captures(line) {
                report.push_failure(caps[1].trim(), message_after(&lines, i + 1, 4));
            }
        }
        return report;
    }

    if let Some(passing) = lines.iter().find_map(|l| MOCHA_PASSING.captures(l)) {
        let failing = lines.iter().find_map(|l| MOCHA_FAILING.captures(l));
        let pending = lines.iter().find_map(|l| MOCHA_PENDING.captures(l));
        report.passed = Some(count(&passing, 1));
        report.failed = Some(failing.as_ref().map_or(0, |c| count(c, 1)));
        report.skipped = Some(pending.as_ref().map_or(0, |c| count(c, 1)));
        report.errors = Some(0);
        report.source = ReportSource::Output;
        if let Some(start) = lines.iter().position(|l| MOCHA_FAILING.is_match(l)) {
            let mut i = start + 1;
            while i < lines.len() {
                if let Some(caps) = MOCHA_ITEM.captures(lines[i]) {
                    // The title wraps over indented lines until one ends in ':'.
                    let mut parts = vec![caps[1].trim().to_string()];
                    let mut j = i;
                    while !parts.last().is_some_and(|p| p.ends_with(':')) && j + 1 < lines.len() {
                        j += 1;
                        let next = lines[j].trim();
                        if next.is_empty() {
                            break;
                        }
                        parts.push(next.to_string());
                    }
                    let name = parts.join(" ").trim_end_matches(':').to_string();
                    report.push_failure(name, message_after(&lines, j + 1, 3));
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
        }
        return report;
    }

    let mut node_counts: BTreeMap<String, u64> = BTreeMap::new();
    for line in &lines {
        if let Some(caps) = NODE_COUNT.captures(line) {
            node_counts.insert(caps[1].to_string(), count(&caps, 2));
        }
    }
    if node_counts.contains_key("tests") || node_counts.contains_key("pass") {
        let n = |k: &str| node_counts.get(k).copied().unwrap_or(0);
        report.passed = Some(n("pass"));
        report.failed = Some(n("fail"));
        report.skipped = Some(n("skipped") + n("todo"));
        report.errors = Some(n("cancelled"));
        report.source = ReportSource::Output;
        let mut i = 0;
        while i < lines.len() {
            if let Some(caps) = TAP_NOT_OK.captures(lines[i]) {
                let name = caps[1].to_string();
                // The YAML diagnostic block carries `error: '...'`.
                let message = lines[i + 1..]
                    .iter()
                    .take_while(|l| !l.trim_start().starts_with("ok ") && !TAP_NOT_OK.is_match(l))
                    .find_map(|l| {
                        TAP_ERROR
                            .captures(l)
                            .map(|c| c[1].trim_matches('\'').to_string())
                    })
                    .unwrap_or_default();
                report.push_failure(name, message.replace("\\n", "\n"));
            } else if let Some(caps) = NODE_SPEC_FAIL.captures(lines[i]) {
                let name = caps[1].to_string();
                if !name.starts_with("failing tests") {
                    report.push_failure(name, message_after(&lines, i + 1, 4));
                }
            }
            i += 1;
        }
    }
    report
}

fn set_js_counts(report: &mut TestReport, summary: &str) {
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for caps in JEST_COUNT.captures_iter(summary) {
        let n = count(&caps, 1);
        match &caps[2] {
            "passed" => passed += n,
            "failed" => failed += n,
            _ => skipped += n,
        }
    }
    report.passed = Some(passed);
    report.failed = Some(failed);
    report.skipped = Some(skipped);
    report.errors = Some(report.errors.unwrap_or(0));
    report.source = ReportSource::Output;
}

// ── go test -json ────────────────────────────────────────────────────────────

static GO_TEXT_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*--- (PASS|FAIL|SKIP): (\S+)").expect("go text result"));
static GO_TEXT_PACKAGE_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^FAIL\s+(\S+)").expect("go package fail"));

fn go_output_is_noise(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("=== RUN")
        || t.starts_with("=== PAUSE")
        || t.starts_with("=== CONT")
        || t.starts_with("=== NAME")
        || t.starts_with("--- FAIL:")
        || t.starts_with("--- PASS:")
        || t.starts_with("--- SKIP:")
        || t == "FAIL"
        || t == "PASS"
}

/// Parse `go test -json`: one event per line, each naming its package and,
/// for test events, the test.
pub fn parse_go(stdout: &str, stderr: &str) -> TestReport {
    let mut report = TestReport::default();
    let (mut passed, mut failed, mut skipped) = (0u64, 0u64, 0u64);
    let mut saw_event = false;
    let mut test_output: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut package_output: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failed_tests: Vec<(String, String)> = Vec::new();
    let mut failed_packages: Vec<String> = Vec::new();
    let mut packages_with_failed_tests: BTreeSet<String> = BTreeSet::new();

    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(action) = event.get("Action").and_then(Value::as_str) else {
            continue;
        };
        saw_event = true;
        let package = event
            .get("Package")
            .or_else(|| event.get("ImportPath"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let test = event
            .get("Test")
            .and_then(Value::as_str)
            .map(str::to_string);
        let output = event.get("Output").and_then(Value::as_str).unwrap_or("");
        match (action, test) {
            ("output", Some(test)) => test_output
                .entry((package, test))
                .or_default()
                .push(output.to_string()),
            ("output" | "build-output", None) => package_output
                .entry(package)
                .or_default()
                .push(output.to_string()),
            ("pass", Some(_)) => passed += 1,
            ("skip", Some(_)) => skipped += 1,
            ("fail", Some(test)) => {
                failed += 1;
                packages_with_failed_tests.insert(package.clone());
                failed_tests.push((package, test));
            }
            ("fail" | "build-fail", None) if !failed_packages.contains(&package) => {
                failed_packages.push(package);
            }
            _ => {}
        }
    }

    if !saw_event {
        return parse_go_text(stdout, stderr);
    }

    let clean = |lines: &[String]| -> String {
        lines
            .concat()
            .lines()
            .filter(|l| !go_output_is_noise(l))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // A failed parent test is failed *because* of its subtests; report the
    // subtest that carries the message and skip the empty parent.
    for (package, test) in &failed_tests {
        let message = test_output
            .get(&(package.clone(), test.clone()))
            .map(|l| clean(l))
            .unwrap_or_default();
        let has_failed_child = failed_tests
            .iter()
            .any(|(p, t)| p == package && t.starts_with(&format!("{test}/")));
        if has_failed_child && message.trim().is_empty() {
            continue;
        }
        report.push_failure(test.clone(), message);
    }

    let mut errors = 0;
    let stderr = strip_ansi(stderr);
    for package in failed_packages {
        if packages_with_failed_tests.contains(&package) {
            continue;
        }
        errors += 1;
        let mut message = package_output
            .get(&package)
            .map(|l| clean(l))
            .unwrap_or_default();
        if message.trim().is_empty() {
            message = stderr.trim().to_string();
        }
        report.push_failure(package, message);
    }

    report.passed = Some(passed);
    report.failed = Some(failed);
    report.skipped = Some(skipped);
    report.errors = Some(errors);
    report.source = ReportSource::Structured;
    report
}

/// `go test` without `-json` (a user `args` override can produce this).
fn parse_go_text(stdout: &str, stderr: &str) -> TestReport {
    let text = strip_ansi(&format!("{stdout}\n{stderr}"));
    let mut report = TestReport::default();
    let (mut passed, mut failed, mut skipped) = (0u64, 0u64, 0u64);
    let mut saw = false;
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = GO_TEXT_RESULT.captures(line) {
            saw = true;
            match &caps[1] {
                "PASS" => passed += 1,
                "SKIP" => skipped += 1,
                _ => {
                    failed += 1;
                    let message = lines[i + 1..]
                        .iter()
                        .take_while(|l| l.starts_with("    ") && !GO_TEXT_RESULT.is_match(l))
                        .map(|l| l.trim())
                        .collect::<Vec<_>>()
                        .join("\n");
                    report.push_failure(&caps[2], message);
                }
            }
        } else if let Some(caps) = GO_TEXT_PACKAGE_FAIL.captures(line) {
            saw = true;
            if line.contains("[build failed]") || line.contains("[setup failed]") {
                report.errors = Some(report.errors.unwrap_or(0) + 1);
                report.push_failure(&caps[1], line.trim());
            }
        }
    }
    if saw {
        report.passed = Some(passed);
        report.failed = Some(failed);
        report.skipped = Some(skipped);
        report.errors = Some(report.errors.unwrap_or(0));
        report.source = ReportSource::Output;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_FAILING: &str = "\
running 3 tests
test tests::adds ... ok
test tests::subtracts ... FAILED
test tests::slow ... ignored, needs a network

failures:

---- tests::subtracts stdout ----
captured line

thread 'tests::subtracts' panicked at src/lib.rs:12:9:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::subtracts

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

running 2 tests
test it_works ... ok
test src/lib.rs - add (line 3) ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    #[test]
    fn cargo_sums_every_test_binary_and_keeps_the_panic() {
        let report = parse_cargo(
            CARGO_FAILING,
            "error: test failed, to rerun pass `--lib`\nerror: 1 target failed:\n    `--lib`\n",
        );
        assert_eq!(report.passed, Some(3));
        assert_eq!(report.failed, Some(1));
        assert_eq!(report.skipped, Some(1));
        assert_eq!(report.errors, Some(0), "narration is not a build error");
        assert_eq!(report.total(), Some(5));
        assert_eq!(report.failures.len(), 1);
        let failure = &report.failures[0];
        assert_eq!(failure.name, "tests::subtracts");
        assert!(
            failure
                .message
                .starts_with("thread 'tests::subtracts' panicked")
        );
        assert!(failure.message.contains("right: 2"));
        assert!(!failure.message.contains("RUST_BACKTRACE"));
        assert!(
            !failure.message.contains("captured line"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn cargo_reports_a_compile_failure_as_an_error_with_the_diagnostic() {
        let stderr = "\
   Compiling demo v0.1.0 (/tmp/demo)
error[E0425]: cannot find value `y` in this scope
 --> src/lib.rs:2:5
  |
2 |     y
  |     ^ not found in this scope

error: could not compile `demo` (lib test) due to 1 previous error
";
        let report = parse_cargo("", stderr);
        assert_eq!(report.passed, None, "no test ran, so no count is claimed");
        assert_eq!(report.errors, Some(1));
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].name, "build: demo (lib test)");
        assert!(report.failures[0].message.contains("cannot find value `y`"));
        assert!(report.failures[0].message.contains("src/lib.rs:2:5"));
    }

    #[test]
    fn cargo_without_summaries_falls_back_to_per_test_lines() {
        let report = parse_cargo("test a ... ok\ntest b ... FAILED\n", "");
        assert_eq!((report.passed, report.failed), (Some(1), Some(1)));
        assert_eq!(report.failures[0].name, "b");
    }

    #[test]
    fn pytest_json_report_is_preferred_and_lists_crashes() {
        let doc = json!({
            "summary": {"passed": 3, "failed": 1, "error": 1, "skipped": 2, "xfailed": 1, "total": 8},
            "collectors": [
                {"nodeid": "tests/test_broken.py", "outcome": "failed",
                 "longrepr": "ImportError while importing test module\nE   ModuleNotFoundError: No module named 'nope'"}
            ],
            "tests": [
                {"nodeid": "tests/test_a.py::test_ok", "outcome": "passed"},
                {"nodeid": "tests/test_a.py::test_bad", "outcome": "failed",
                 "call": {"outcome": "failed", "crash": {"message": "AssertionError: assert 1 == 2"}}},
                {"nodeid": "tests/test_a.py::test_fixture", "outcome": "error",
                 "setup": {"outcome": "failed", "longrepr": "fixture 'db' not found"}}
            ]
        });
        let report = parse_pytest_json(&doc).expect("summary present");
        assert_eq!(report.source, ReportSource::Structured);
        assert_eq!(
            (report.passed, report.failed, report.skipped, report.errors),
            (Some(3), Some(1), Some(3), Some(1))
        );
        let names: Vec<&str> = report.failures.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "tests/test_broken.py",
                "tests/test_a.py::test_bad",
                "tests/test_a.py::test_fixture"
            ]
        );
        assert_eq!(report.failures[1].message, "AssertionError: assert 1 == 2");
        assert_eq!(report.failures[2].message, "fixture 'db' not found");
        assert!(parse_pytest_json(&json!({"tests": []})).is_none());
    }

    #[test]
    fn pytest_output_reads_the_final_summary_not_stray_text() {
        let stdout = "\
.F.s
this test printed 99 passed on purpose
=========================== short test summary info ============================
FAILED tests/test_math.py::test_div - ZeroDivisionError: division by zero
ERROR tests/test_db.py::test_conn - fixture 'db' not found
=============== 1 failed, 2 passed, 1 skipped, 1 error in 0.12s ================
";
        let report = parse_pytest_output(stdout, "");
        assert_eq!(
            (report.passed, report.failed, report.skipped, report.errors),
            (Some(2), Some(1), Some(1), Some(1))
        );
        assert_eq!(report.failures.len(), 2);
        assert_eq!(report.failures[0].name, "tests/test_math.py::test_div");
        assert_eq!(
            report.failures[0].message,
            "ZeroDivisionError: division by zero"
        );
    }

    #[test]
    fn pytest_quiet_summary_and_usage_errors() {
        let report = parse_pytest_output("..\n2 passed in 0.01s\n", "");
        assert_eq!((report.passed, report.failed), (Some(2), Some(0)));
        let report = parse_pytest_output("\nno tests ran in 0.00s\n", "");
        assert_eq!(report.total(), Some(0));
        let report = parse_pytest_output(
            "",
            "ERROR: usage: pytest [options]\npytest: error: unrecognized arguments: --bogus\n",
        );
        assert_eq!(report.errors, Some(1));
        assert!(
            report.failures[0]
                .message
                .contains("unrecognized arguments")
        );
    }

    #[test]
    fn unittest_counts_and_failure_blocks() {
        let stderr = "\
.F.Es
======================================================================
ERROR: test_e (test_mod.T.test_e)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_mod.py\", line 9, in test_e
    raise RuntimeError('boom')
RuntimeError: boom

======================================================================
FAIL: test_f (test_mod.T.test_f)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_mod.py\", line 6, in test_f
    self.assertEqual(1, 2)
AssertionError: 1 != 2

----------------------------------------------------------------------
Ran 5 tests in 0.001s

FAILED (failures=1, errors=1, skipped=1)
";
        let report = parse_unittest("", stderr);
        assert_eq!(
            (report.passed, report.failed, report.skipped, report.errors),
            (Some(2), Some(1), Some(1), Some(1))
        );
        assert_eq!(report.failures[0].name, "test_e (test_mod.T.test_e)");
        assert_eq!(report.failures[0].message, "RuntimeError: boom");
        assert_eq!(report.failures[1].message, "AssertionError: 1 != 2");

        let ok = parse_unittest("", "...\n------\nRan 3 tests in 0.0s\n\nOK (skipped=1)\n");
        assert_eq!(
            (ok.passed, ok.failed, ok.skipped),
            (Some(2), Some(0), Some(1))
        );
    }

    #[test]
    fn jest_summary_and_failures() {
        let stderr = "\
FAIL src/math.test.js
  ● math › adds

    expect(received).toBe(expected) // Object.is equality

    Expected: 3
    Received: 2

      3 | test('adds', () => {
    > 4 |   expect(1 + 1).toBe(3);

Tests:       1 failed, 1 skipped, 1 todo, 3 passed, 6 total
Test Suites: 1 failed, 1 total
";
        let report = parse_node("", stderr);
        assert_eq!(
            (report.passed, report.failed, report.skipped),
            (Some(3), Some(1), Some(2))
        );
        assert_eq!(report.failures[0].name, "math › adds");
        assert!(report.failures[0].message.contains("Expected: 3"));
        assert!(!report.failures[0].message.contains("expect(1 + 1)"));
    }

    #[test]
    fn vitest_summary_and_failures() {
        let stdout = "\
 FAIL  src/math.test.ts > math > adds
AssertionError: expected 2 to be 3 // Object.is equality
 ❯ src/math.test.ts:4:19

 Test Files  1 failed (1)
      Tests  1 failed | 2 passed | 1 skipped (4)
";
        let report = parse_node(stdout, "");
        assert_eq!(
            (report.passed, report.failed, report.skipped),
            (Some(2), Some(1), Some(1))
        );
        assert_eq!(report.failures[0].name, "src/math.test.ts > math > adds");
        assert_eq!(
            report.failures[0].message,
            "AssertionError: expected 2 to be 3 // Object.is equality"
        );
    }

    #[test]
    fn mocha_summary_and_failures() {
        let stdout = "\
  2 passing (8ms)
  1 pending
  1 failing

  1) Array
       #indexOf()
         returns -1 when absent:
     AssertionError [ERR_ASSERTION]: 0 == -1
      at Context.<anonymous> (test/a.js:5:14)
";
        let report = parse_node(stdout, "");
        assert_eq!(
            (report.passed, report.failed, report.skipped),
            (Some(2), Some(1), Some(1))
        );
        assert_eq!(
            report.failures[0].name,
            "Array #indexOf() returns -1 when absent"
        );
        assert_eq!(
            report.failures[0].message,
            "AssertionError [ERR_ASSERTION]: 0 == -1"
        );
    }

    #[test]
    fn node_test_runner_tap() {
        let stdout = "\
TAP version 13
# Subtest: adds
ok 1 - adds
# Subtest: divides
not ok 2 - divides
  ---
  duration_ms: 0.4
  error: 'Expected values to be strictly equal:\\n\\n2 !== 3\\n'
  code: 'ERR_ASSERTION'
  ...
1..2
# tests 2
# pass 1
# fail 1
# cancelled 0
# skipped 0
# todo 0
";
        let report = parse_node(stdout, "");
        assert_eq!((report.passed, report.failed), (Some(1), Some(1)));
        assert_eq!(report.failures[0].name, "divides");
        assert!(report.failures[0].message.contains("2 !== 3"));
    }

    #[test]
    fn go_json_events() {
        let stdout = r##"{"Action":"run","Package":"ex/a","Test":"TestAdd"}
{"Action":"output","Package":"ex/a","Test":"TestAdd","Output":"=== RUN   TestAdd\n"}
{"Action":"pass","Package":"ex/a","Test":"TestAdd","Elapsed":0}
{"Action":"run","Package":"ex/a","Test":"TestDiv"}
{"Action":"output","Package":"ex/a","Test":"TestDiv","Output":"=== RUN   TestDiv\n"}
{"Action":"output","Package":"ex/a","Test":"TestDiv","Output":"    a_test.go:9: got 2, want 3\n"}
{"Action":"output","Package":"ex/a","Test":"TestDiv","Output":"--- FAIL: TestDiv (0.00s)\n"}
{"Action":"fail","Package":"ex/a","Test":"TestDiv","Elapsed":0}
{"Action":"skip","Package":"ex/a","Test":"TestSlow","Elapsed":0}
{"Action":"fail","Package":"ex/a","Elapsed":0.01}
{"Action":"output","Package":"ex/b","Output":"# ex/b\n"}
{"Action":"output","Package":"ex/b","Output":"./b.go:3:2: undefined: nope\n"}
{"Action":"fail","Package":"ex/b","Elapsed":0}
"##;
        let report = parse_go(stdout, "");
        assert_eq!(report.source, ReportSource::Structured);
        assert_eq!(
            (report.passed, report.failed, report.skipped, report.errors),
            (Some(1), Some(1), Some(1), Some(1))
        );
        assert_eq!(report.failures[0].name, "TestDiv");
        assert_eq!(report.failures[0].message, "a_test.go:9: got 2, want 3");
        assert_eq!(report.failures[1].name, "ex/b");
        assert!(report.failures[1].message.contains("undefined: nope"));
    }

    #[test]
    fn go_text_output_is_still_understood() {
        let stdout = "--- FAIL: TestX (0.00s)\n    x_test.go:5: boom\nFAIL\nFAIL\tex/x\t0.01s\n";
        let report = parse_go(stdout, "");
        assert_eq!((report.failed, report.passed), (Some(1), Some(0)));
        assert_eq!(report.failures[0].message, "x_test.go:5: boom");
    }

    #[test]
    fn failures_are_bounded_and_deduplicated() {
        let mut report = TestReport::default();
        for i in 0..(MAX_FAILURES + 5) {
            report.push_failure(format!("t{i}"), "x");
        }
        report.push_failure("t0", "again");
        assert_eq!(report.failures.len(), MAX_FAILURES);
        assert_eq!(report.failures_omitted, 5);
        let long = "y".repeat(MAX_MESSAGE_CHARS + 10);
        let mut r = TestReport::default();
        r.push_failure("long", &long);
        assert_eq!(r.failures[0].message.chars().count(), MAX_MESSAGE_CHARS + 1);
    }

    #[test]
    fn ansi_codes_do_not_hide_a_summary() {
        let stdout = "\x1b[32mtest a ... ok\x1b[0m\n\x1b[1mtest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\x1b[0m\n";
        assert_eq!(parse_cargo(stdout, "").passed, Some(1));
    }
}
