//! The `reconstruct_site` effect, the `infra_artifacts_present` guard and the
//! `reconstruct` tool group — the deterministic half of agenthicc's
//! `reconstruct_site` runner (`runner.py`, `phase_impl.py`, `evidence_plan.py`,
//! `evidence.py`).
//!
//! # What drives control flow
//!
//! * **The authoritative plan** ([`PLAN`]) — the 41 canonical phases, their
//!   turn budgets, artifact kinds and profile membership. A run's profile
//!   (`static`, `application`, `production` or `custom`) is fixed when init
//!   completes: the `profile` param, else `static` when the reference is
//!   static and `application` otherwise. Every forward transition routes to
//!   the next phase **of the active profile**, so a static run never enters
//!   the data layer or the infrastructure phases. `custom_phases` is
//!   validated exactly like agenthicc's `ReconstructPhasePlan.active`.
//! * **The page loop** — recon fixes the routes to implement (the desired
//!   routes, else `/`); `page` runs once per route, its cursor mirrored into
//!   `{state.page_route}` / `{state.page_number}` / `{state.page_count}`.
//! * **The research gate** — entering `research_gate` publishes a fidelity
//!   baseline (a content-addressed artifact whose sha256 is its id); approval
//!   must cite the current baseline id, a rejection names the earliest phase
//!   to re-enter.
//! * **Controlled re-entry** — validation rejections and the gate's rejection
//!   may send the run back to any phase of the active profile, within
//!   `max_reentries` (default 3). Re-entry stales the target's and every
//!   downstream phase's artifacts and, for a research target, reopens the
//!   gate. An empty target retries the current phase.
//! * **Terminal states** — `final_approved` completes the run;
//!   `final_blocked` ends it early with the blocker (agenthicc's `BLOCKED`).
//!
//! # Evidence
//!
//! `.xaft/reconstruct_site/<run-id>/manifest.json` plus
//! `phases/<phase>/<attempt>/<kind>-<sha256-prefix>.json`: one primary
//! artifact and one receipt per completed phase, the gate's baseline,
//! coverage report and decision, screenshot links and re-entry history,
//! published with atomic manifest revisions. The prompts' `[EVIDENCE DIGEST]`
//! (`{state.evidence_digest}`) names the manifest and the current baseline.
//!
//! **Simplified against agenthicc:** the PRD-178 coverage matrix (typed
//! route × viewport × state observation cells) is not ported. agenthicc's own
//! research tools submit only summaries, which puts its gate in the
//! "summary-only" mode this port implements for every profile; resume does
//! not re-verify artifact hashes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::groups::RunData;
use super::hooks::{
    EffectOutcome, HookContext, Route, arg_bool, arg_list, arg_objects, arg_str, field_list,
    field_str, field_u64, put_list, record, save_record, set_state,
};

const RECORD: &str = "reconstruct_site";

/// agenthicc's `PHASE_PLAN_VERSION`.
pub const PLAN_VERSION: &str = "reconstruct-site.v3";

/// One-line description for `list_tool_groups`.
pub const GROUP_SUMMARY: &str = "reconstruct_site evidence: record_reconstruct_screenshot links a \
     playwright/cloakbrowser screenshot artifact to the run's evidence manifest; guard \
     infra_artifacts_present; effect reconstruct_site";

/// Which profiles include a phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// static, application and production.
    All,
    /// application and production.
    Application,
    /// production only.
    Production,
}

/// One canonical phase of the authoritative plan.
#[derive(Debug, Clone, Copy)]
pub struct PlanPhase {
    /// Phase name.
    pub name: &'static str,
    /// Turn budget.
    pub max_turns: u32,
    /// Profile membership.
    pub scope: Scope,
    /// Artifact kinds the phase owns (re-entry stales them).
    pub artifacts: &'static [&'static str],
}

const fn p(
    name: &'static str,
    max_turns: u32,
    scope: Scope,
    artifacts: &'static [&'static str],
) -> PlanPhase {
    PlanPhase {
        name,
        max_turns,
        scope,
        artifacts,
    }
}

/// agenthicc's `RECONSTRUCT_PHASE_PLAN`, in canonical order.
pub const PLAN: &[PlanPhase] = &[
    p("init", 10, Scope::All, &["initial_state", "research_scope"]),
    p(
        "recon",
        35,
        Scope::All,
        &[
            "route_inventory",
            "route_surface_inventory",
            "viewport_environment_matrix",
            "visual_state_inventory",
        ],
    ),
    p(
        "visual_research",
        30,
        Scope::All,
        &[
            "visual_spec",
            "visual_measurements",
            "visual_state_inventory",
            "screenshot",
        ],
    ),
    p(
        "interaction_analysis",
        30,
        Scope::All,
        &[
            "interaction_inventory",
            "interaction_trace_catalog",
            "interaction_state_graph",
        ],
    ),
    p(
        "content_assets",
        25,
        Scope::All,
        &[
            "asset_inventory",
            "content_asset_inventory",
            "font_icon_inventory",
        ],
    ),
    p(
        "responsive_research",
        30,
        Scope::All,
        &[
            "responsive_research",
            "responsive_behavior_matrix",
            "screenshot",
        ],
    ),
    p("architecture", 25, Scope::All, &["architecture"]),
    p("design_system", 25, Scope::All, &["design_system"]),
    p(
        "research_gate",
        15,
        Scope::All,
        &[
            "fidelity_baseline",
            "research_coverage_report",
            "research_gate_receipt",
        ],
    ),
    p("bootstrap", 25, Scope::All, &["bootstrap"]),
    p("global_shell", 30, Scope::All, &["global_shell"]),
    p("component_system", 30, Scope::All, &["component_system"]),
    p("page", 40, Scope::All, &["page"]),
    p("data_layer", 30, Scope::Application, &["data_layer"]),
    p("responsive_pass", 30, Scope::All, &["responsive"]),
    p(
        "visual_validation",
        35,
        Scope::All,
        &["visual_validation", "screenshot"],
    ),
    p(
        "interaction_validation",
        35,
        Scope::All,
        &["interaction_validation"],
    ),
    p("accessibility", 30, Scope::All, &["accessibility"]),
    p("performance", 30, Scope::All, &["performance"]),
    p("fidelity_pass", 30, Scope::All, &["fidelity_pass"]),
    p("sqlite_db", 35, Scope::Production, &["sqlite_db"]),
    p("verify_sqlite", 30, Scope::Production, &["verify_sqlite"]),
    p("prisma", 35, Scope::Production, &["prisma"]),
    p("verify_prisma", 30, Scope::Production, &["verify_prisma"]),
    p("tanstack_query", 35, Scope::Production, &["tanstack_query"]),
    p(
        "verify_tanstack",
        30,
        Scope::Production,
        &["verify_tanstack"],
    ),
    p("env_config", 30, Scope::Production, &["env_config"]),
    p("verify_env", 30, Scope::Production, &["verify_env"]),
    p("docker", 35, Scope::Production, &["docker"]),
    p("verify_docker", 30, Scope::Production, &["verify_docker"]),
    p("netlify", 30, Scope::Production, &["netlify"]),
    p("verify_netlify", 30, Scope::Production, &["verify_netlify"]),
    p("caddy", 30, Scope::Production, &["caddy"]),
    p("verify_caddy", 30, Scope::Production, &["verify_caddy"]),
    p(
        "package_commands",
        30,
        Scope::Production,
        &["package_commands"],
    ),
    p("verify_package", 30, Scope::Production, &["verify_package"]),
    p("scripts", 35, Scope::Production, &["scripts"]),
    p("verify_scripts", 30, Scope::Production, &["verify_scripts"]),
    p("docs", 40, Scope::Production, &["docs"]),
    p("verify_docs", 30, Scope::Production, &["verify_docs"]),
    p("final_validation", 35, Scope::All, &["final_validation"]),
];

/// Phases whose re-entry reopens the research gate.
const RESEARCH_PHASES: &[&str] = &[
    "recon",
    "visual_research",
    "interaction_analysis",
    "content_assets",
    "responsive_research",
    "architecture",
    "design_system",
    "research_gate",
];

/// agenthicc's `_SUMMARY_RESEARCH_PHASES`.
const SUMMARY_RESEARCH_PHASES: &[&str] = &[
    "recon",
    "visual_research",
    "interaction_analysis",
    "content_assets",
    "responsive_research",
    "architecture",
    "design_system",
];

fn plan_index(name: &str) -> Option<usize> {
    PLAN.iter().position(|p| p.name == name)
}

fn normalise(name: &str) -> String {
    name.trim().to_lowercase().replace('-', "_")
}

/// agenthicc's `ReconstructPhasePlan.active`: the ordered phases of a
/// profile.
pub fn active_phases(profile: &str, custom: &[String]) -> Result<Vec<&'static str>, String> {
    let profile = normalise(profile);
    let selected: Vec<&'static str> = match profile.as_str() {
        "static" => PLAN
            .iter()
            .filter(|p| p.scope == Scope::All)
            .map(|p| p.name)
            .collect(),
        "application" => PLAN
            .iter()
            .filter(|p| p.scope != Scope::Production)
            .map(|p| p.name)
            .collect(),
        "production" => PLAN.iter().map(|p| p.name).collect(),
        "custom" => {
            let chosen: Vec<String> = custom
                .iter()
                .map(|n| normalise(n))
                .filter(|n| !n.is_empty())
                .collect();
            if chosen.is_empty() {
                return Err("custom profile requires at least one phase".into());
            }
            let mut unknown: Vec<&str> = chosen
                .iter()
                .filter(|n| plan_index(n).is_none())
                .map(String::as_str)
                .collect();
            unknown.sort_unstable();
            unknown.dedup();
            if !unknown.is_empty() {
                return Err(format!(
                    "custom profile contains unknown phases: {}",
                    unknown.join(", ")
                ));
            }
            if chosen.first().map(String::as_str) != Some("init")
                || chosen.last().map(String::as_str) != Some("final_validation")
            {
                return Err(
                    "custom profile must include init and final_validation at its boundaries"
                        .into(),
                );
            }
            if !chosen.iter().any(|n| n == "research_gate") {
                return Err(
                    "custom profile must include research_gate before implementation".into(),
                );
            }
            let mut seen = std::collections::HashSet::new();
            if !chosen.iter().all(|n| seen.insert(n.clone())) {
                return Err("custom profile contains duplicate phases".into());
            }
            let positions: Vec<usize> = chosen.iter().filter_map(|n| plan_index(n)).collect();
            if !positions.windows(2).all(|w| w[0] < w[1]) {
                return Err(
                    "custom profile phases must follow the authoritative plan order".into(),
                );
            }
            positions.into_iter().map(|i| PLAN[i].name).collect()
        }
        _ => return Err(format!("unknown reconstruct profile '{profile}'")),
    };
    Ok(selected)
}

/// agenthicc's `_skip_reason`.
fn skip_reason(name: &str, profile: &str) -> String {
    match profile {
        "static" => format!("{name} is outside the static reconstruction scope"),
        "application" => format!("{name} requires production infrastructure scope"),
        _ => format!("{name} was not selected by the custom phase graph"),
    }
}

/// agenthicc's `_safe_reference_url`: keep scheme, host, port and path; drop
/// credentials, query and fragment.
pub fn safe_reference_url(value: &str) -> String {
    let value = value.trim();
    let Some((scheme, rest)) = value.split_once("://") else {
        return String::new();
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    if host_port.is_empty() || host_port.starts_with(':') {
        return String::new();
    }
    let host_lower = host_port.to_ascii_lowercase();
    let path_part = &rest[end..];
    let path = path_part.split(['?', '#']).next().unwrap_or_default();
    format!("{scheme}://{host_lower}{path}")
}

/// One configured research viewport (agenthicc's `ViewportSpec`).
fn parse_viewports(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let defaults = || {
        vec![
            json!({"viewport_id": "mobile", "width": 390, "height": 844, "device_scale": 1.0, "orientation": "portrait", "reduced_motion": false, "color_scheme": "light", "touch": true}),
            json!({"viewport_id": "tablet", "width": 768, "height": 1024, "device_scale": 1.0, "orientation": "portrait", "reduced_motion": false, "color_scheme": "light", "touch": true}),
            json!({"viewport_id": "desktop", "width": 1440, "height": 900, "device_scale": 1.0, "orientation": "portrait", "reduced_motion": false, "color_scheme": "light", "touch": false}),
        ]
    };
    let raw = match value {
        None | Some(Value::Null) => return Ok(defaults()),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(defaults()),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .map_err(|_| "reconstruct_site.viewports must be a JSON array".to_string())?,
        Some(other) => other.clone(),
    };
    let items = match raw {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => return Err("reconstruct_site.viewports must be an array of objects".into()),
    };
    if items.is_empty() {
        return Err("reconstruct_site.viewports must not be empty".into());
    }
    let mut out = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in items {
        let Value::Object(m) = item else {
            return Err("each reconstruct_site viewport must be an object".into());
        };
        let id = m
            .get("viewport_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let width = m.get("width").and_then(Value::as_i64).unwrap_or(0);
        let height = m.get("height").and_then(Value::as_i64).unwrap_or(0);
        let scale = m.get("device_scale").and_then(Value::as_f64).unwrap_or(1.0);
        let orientation = m
            .get("orientation")
            .and_then(Value::as_str)
            .unwrap_or("portrait");
        let scheme = m
            .get("color_scheme")
            .and_then(Value::as_str)
            .unwrap_or("light");
        if id.is_empty() || width < 1 || height < 1 || scale <= 0.0 {
            return Err("malformed viewport specification".into());
        }
        if !["portrait", "landscape"].contains(&orientation) {
            return Err("orientation must be portrait or landscape".into());
        }
        if !["light", "dark", "no-preference"].contains(&scheme) {
            return Err("invalid color_scheme".into());
        }
        if !ids.insert(id.clone()) {
            return Err("reconstruct_site.viewports must have unique viewport_id values".into());
        }
        out.push(json!({
            "viewport_id": id,
            "width": width,
            "height": height,
            "device_scale": scale,
            "orientation": orientation,
            "reduced_motion": m.get("reduced_motion").and_then(Value::as_bool).unwrap_or(false),
            "color_scheme": scheme,
            "touch": m.get("touch").and_then(Value::as_bool).unwrap_or(false),
        }));
    }
    Ok(out)
}

// ── Evidence store ─────────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// agenthicc's `_redact`: secret-looking keys never reach evidence files.
fn redact(value: &Value) -> Value {
    fn secret(key: &str) -> bool {
        let k = key.to_lowercase();
        [
            "authorization",
            "apikey",
            "api_key",
            "api-key",
            "api key",
            "token",
            "password",
            "cookie",
            "secret",
        ]
        .iter()
        .any(|s| k.contains(s))
    }
    match value {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    if secret(k) {
                        (k.clone(), json!("[redacted]"))
                    } else {
                        (k.clone(), redact(v))
                    }
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        other => other.clone(),
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("evidence path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("evidence")
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot publish {}: {e}", path.display()))
}

/// The run's evidence package on disk.
struct Evidence {
    root: PathBuf,
    relative_root: String,
    manifest: Map<String, Value>,
}

impl Evidence {
    fn open(data: &RunData, profile: &str) -> Result<Self, String> {
        let wd = data
            .working_dir()
            .ok_or("the run has no working directory")?
            .to_path_buf();
        let run_id = data.run_id().unwrap_or("run").to_string();
        let safe = !run_id.is_empty()
            && run_id.len() <= 128
            && run_id
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && run_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if !safe {
            return Err(format!("invalid run id for evidence: {run_id:?}"));
        }
        let relative_root = format!(".xaft/reconstruct_site/{run_id}");
        let root = wd.join(&relative_root);
        let path = root.join("manifest.json");
        let manifest = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(m)) => m,
                _ => return Err("reconstruct evidence manifest is unreadable".into()),
            },
            Err(_) => {
                let mut m = Map::new();
                m.insert("schema_version".into(), json!(1));
                m.insert("run_id".into(), json!(run_id));
                m.insert("plan_version".into(), json!(PLAN_VERSION));
                m.insert("profile".into(), json!(profile));
                m.insert("status".into(), json!("running"));
                m.insert("revision".into(), json!(0));
                m.insert("created_at".into(), json!(now()));
                m.insert("artifacts".into(), json!([]));
                m.insert("screenshots".into(), json!([]));
                m.insert("skipped_phases".into(), json!([]));
                m.insert("reentry_history".into(), json!([]));
                m
            }
        };
        Ok(Self {
            root,
            relative_root,
            manifest,
        })
    }

    fn manifest_relative_path(&self) -> String {
        format!("{}/manifest.json", self.relative_root)
    }

    fn list_mut(&mut self, key: &str) -> &mut Vec<Value> {
        let entry = self
            .manifest
            .entry(key.to_string())
            .or_insert_with(|| json!([]));
        if !entry.is_array() {
            *entry = json!([]);
        }
        entry.as_array_mut().expect("array")
    }

    fn publish(&mut self) -> Result<(), String> {
        let revision = self
            .manifest
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        self.manifest.insert("revision".into(), json!(revision));
        self.manifest.insert("updated_at".into(), json!(now()));
        let bytes = serde_json::to_vec_pretty(&Value::Object(self.manifest.clone()))
            .map_err(|e| e.to_string())?;
        atomic_write(&self.root.join("manifest.json"), &bytes)
    }

    fn revision(&self) -> u64 {
        self.manifest
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    /// agenthicc's `put_json`: write one content-addressed artifact.
    fn put_json(
        &mut self,
        kind: &str,
        phase: &str,
        attempt: u64,
        value: &Value,
    ) -> Result<String, String> {
        let bytes = serde_json::to_vec_pretty(&redact(value)).map_err(|e| e.to_string())?;
        let digest = sha256_hex(&bytes);
        let exists = self.list_mut("artifacts").iter().any(|a| {
            a["sha256"] == json!(digest)
                && a["kind"] == json!(kind)
                && a["phase"] == json!(phase)
                && a["attempt"] == json!(attempt)
                && a["status"] == json!("complete")
        });
        if exists {
            return Ok(digest);
        }
        let relative = format!(
            "{}/phases/{phase}/{attempt}/{kind}-{}.json",
            self.relative_root,
            &digest[..16]
        );
        let path = self
            .root
            .join("phases")
            .join(phase)
            .join(attempt.to_string())
            .join(format!("{kind}-{}.json", &digest[..16]));
        atomic_write(&path, &bytes)?;
        self.list_mut("artifacts").push(json!({
            "artifact_id": digest,
            "kind": kind,
            "relative_path": relative,
            "media_type": "application/json",
            "sha256": digest,
            "byte_count": bytes.len(),
            "phase": phase,
            "attempt": attempt,
            "source": "workflow",
            "status": "complete",
            "created_at": now(),
        }));
        Ok(digest)
    }

    /// agenthicc's `invalidate`: stale every complete artifact of `kinds`.
    fn invalidate(
        &mut self,
        kinds: &[&str],
        source: &str,
        target: &str,
        reason: &str,
    ) -> Vec<String> {
        let mut affected = Vec::new();
        for a in self.list_mut("artifacts").iter_mut() {
            let kind = a["kind"].as_str().unwrap_or_default().to_string();
            if kinds.contains(&kind.as_str()) && a["status"] == json!("complete") {
                a["status"] = json!("stale");
                affected.push(a["artifact_id"].as_str().unwrap_or_default().to_string());
            }
        }
        self.list_mut("reentry_history").push(json!({
            "source_phase": source,
            "target_phase": target,
            "reason": reason.chars().take(1000).collect::<String>(),
            "artifact_ids": affected,
        }));
        affected
    }

    fn digest(&self) -> String {
        let text = serde_json::to_string(&Value::Object(self.manifest.clone())).unwrap_or_default();
        sha256_hex(text.as_bytes())[..32].to_string()
    }
}

// ── Effect helpers ─────────────────────────────────────────────────────────

fn map_field(rec: &Map<String, Value>, key: &str) -> Map<String, Value> {
    match rec.get(key) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

fn set_map_entry(rec: &mut Map<String, Value>, key: &str, entry: &str, value: Value) {
    let mut m = map_field(rec, key);
    m.insert(entry.to_string(), value);
    rec.insert(key.to_string(), Value::Object(m));
}

fn push_entry(rec: &mut Map<String, Value>, key: &str, value: Value) {
    let mut list = match rec.get(key) {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    list.push(value);
    rec.insert(key.to_string(), Value::Array(list));
}

fn known_issue(rec: &mut Map<String, Value>, phase: &str, issue: &str, severity: &str) {
    push_entry(
        rec,
        "known_issues",
        json!({"phase": phase, "issue": issue, "severity": severity}),
    );
}

fn active(rec: &Map<String, Value>) -> Vec<String> {
    let list = field_list(rec, "active_phases");
    if list.is_empty() {
        // Before init records a profile, the full production graph applies
        // (agenthicc's fallback for direct phase-method callers).
        PLAN.iter().map(|p| p.name.to_string()).collect()
    } else {
        list
    }
}

/// The first phase of the active profile after `phase`.
fn next_active(rec: &Map<String, Value>, phase: &str) -> Option<String> {
    let active = active(rec);
    let pos = plan_index(phase)?;
    PLAN[pos + 1..]
        .iter()
        .map(|p| p.name)
        .find(|n| active.iter().any(|a| a == n))
        .map(str::to_string)
}

fn notes_path(rec: &Map<String, Value>, note_type: &str, filename: &str) -> String {
    let root = field_str(rec, "target_directory");
    let root = root.trim().trim_end_matches(['/', '\\']);
    let root = if root.is_empty() {
        "<output_dir>"
    } else {
        root
    };
    format!("{root}/research/{note_type}/{filename}")
}

fn attempt_for(rec: &mut Map<String, Value>, phase: &str) -> u64 {
    let n = map_field(rec, "phase_attempts")
        .get(phase)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    set_map_entry(rec, "phase_attempts", phase, json!(n));
    n
}

/// Write the phase's primary artifact and receipt, and mark it completed.
fn persist_phase(
    ctx: &HookContext<'_>,
    rec: &mut Map<String, Value>,
    ev: &mut Evidence,
    summary: &str,
) -> Result<(), String> {
    let phase = ctx.phase;
    let attempt = attempt_for(rec, phase);
    // The gate publishes its own artifacts (baseline, coverage report, decision).
    let primary = (phase != "research_gate")
        .then(|| plan_index(phase).and_then(|i| PLAN[i].artifacts.first()))
        .flatten();
    if let Some(kind) = primary {
        if *kind != "screenshot" {
            let id = ev.put_json(
                kind,
                phase,
                attempt,
                &json!({
                    "phase": phase,
                    "summary": summary,
                    "transition": ctx.tool,
                    "arguments": ctx.args,
                }),
            )?;
            set_map_entry(rec, "artifact_ids", kind, json!(id));
        }
    }
    ev.put_json(
        "phase_receipt",
        phase,
        attempt,
        &json!({
            "run_id": ctx.data.run_id().unwrap_or("run"),
            "plan_version": PLAN_VERSION,
            "phase": phase,
            "attempt": attempt,
            "summary": summary.chars().take(4000).collect::<String>(),
            "transition": ctx.tool,
        }),
    )?;
    let mut completed = field_list(rec, "completed_phases");
    if !completed.iter().any(|c| c == phase) {
        completed.push(phase.to_string());
    }
    put_list(rec, "completed_phases", &completed);
    rec.insert("last_transition".into(), json!(ctx.tool));
    Ok(())
}

/// Publish the summary-only research baseline (agenthicc's
/// `_publish_baseline` in summary-only mode).
fn publish_baseline(
    data: &RunData,
    rec: &mut Map<String, Value>,
    ev: &mut Evidence,
) -> Result<(), String> {
    let summaries = map_field(rec, "phase_summaries");
    let attempt = map_field(rec, "phase_attempts")
        .get("research_gate")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let research: Map<String, Value> = SUMMARY_RESEARCH_PHASES
        .iter()
        .filter_map(|p| summaries.get(*p).map(|s| ((*p).to_string(), s.clone())))
        .collect();
    let handoff = json!({
        "mode": "summary_only",
        "scope": {
            "reference_url": field_str(rec, "target_url"),
            "target_directory": field_str(rec, "target_directory"),
            "intent": data.intent(),
        },
        "phase_summaries": research,
    });
    let coverage = ev.put_json(
        "research_coverage_report",
        "research_gate",
        attempt,
        &handoff,
    )?;
    let mut baseline = handoff.clone();
    baseline["coverage_artifact_id"] = json!(coverage);
    let baseline_id = ev.put_json("fidelity_baseline", "research_gate", attempt, &baseline)?;
    rec.insert("research_baseline_id".into(), json!(baseline_id));
    let report = json!({
        "mode": "summary_only",
        "phase_summaries": Value::Object(summaries),
        "note": "Detailed findings are in the agent-owned Markdown files named by each phase.",
    });
    rec.insert("research_report".into(), json!(report.to_string()));
    Ok(())
}

/// Route to `target`, applying the page loop and the gate boundary, and
/// publish what the target's prompt needs.
fn route_to(
    ctx: &HookContext<'_>,
    rec: &mut Map<String, Value>,
    ev: &mut Evidence,
    target: Option<String>,
) -> Result<Route, String> {
    let Some(mut target) = target else {
        return Ok(Route::End);
    };
    loop {
        if target == "bootstrap"
            && active(rec).iter().any(|a| a == "research_gate")
            && !["approved", "approved_degraded"]
                .contains(&field_str(rec, "research_gate_status").as_str())
        {
            // The research gate is a hard boundary before implementation.
            target = "research_gate".into();
        }
        if target == "page" {
            let pages = field_list(rec, "pages_to_implement");
            let index = field_u64(rec, "page_index") as usize;
            if index >= pages.len() {
                // Every route is implemented: the runner's `_page` falls
                // straight through to the next phase.
                let mut completed = field_list(rec, "completed_phases");
                if !completed.iter().any(|c| c == "page") {
                    completed.push("page".into());
                }
                put_list(rec, "completed_phases", &completed);
                match next_active(rec, "page") {
                    Some(next) => {
                        target = next;
                        continue;
                    }
                    None => return Ok(Route::End),
                }
            }
            set_state(
                ctx.data,
                &[
                    ("page_route", pages[index].clone()),
                    ("page_number", (index + 1).to_string()),
                    ("page_count", pages.len().to_string()),
                ],
            );
        }
        if target == "research_gate" {
            publish_baseline(ctx.data, rec, ev)?;
            set_state(
                ctx.data,
                &[(
                    "research_gate_failure",
                    "review the report again".to_string(),
                )],
            );
        }
        return Ok(Route::Phase(target));
    }
}

/// agenthicc's `_reentry_state` + the runner's pending re-entry handling.
fn reenter(
    ctx: &HookContext<'_>,
    rec: &mut Map<String, Value>,
    ev: &mut Evidence,
    target: &str,
) -> Result<String, String> {
    let target = normalise(target);
    let profile = field_str(rec, "profile");
    if plan_index(&target).is_none() || !active(rec).contains(&target) {
        return Err(format!(
            "unknown phase '{target}' for profile '{}'; use a valid phase from the active profile",
            if profile.is_empty() {
                "production"
            } else {
                &profile
            }
        ));
    }
    let max = match ctx.data.param("max_reentries") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(3).max(0) as u64,
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(3).max(0) as u64,
        _ => 3,
    };
    let count = field_u64(rec, "reentry_count");
    if count >= max {
        return Err(format!("re-entry budget exhausted ({max})"));
    }
    let pos = active(rec).iter().position(|a| *a == target).unwrap_or(0);
    let kinds: Vec<&str> = active(rec)[pos..]
        .iter()
        .filter_map(|n| plan_index(n))
        .flat_map(|i| PLAN[i].artifacts.iter().copied())
        .collect();
    let reason = format!("validation requested re-entry to {target}");
    let affected = ev.invalidate(&kinds, ctx.phase, &target, &reason);
    rec.insert("reentry_count".into(), json!(count + 1));
    let mut stale = field_list(rec, "stale_artifact_ids");
    for a in &affected {
        if !stale.contains(a) {
            stale.push(a.clone());
        }
    }
    put_list(rec, "stale_artifact_ids", &stale);
    push_entry(
        rec,
        "reentry_history",
        json!({"source_phase": ctx.phase, "target_phase": target, "reason": reason, "artifact_ids": affected}),
    );
    if RESEARCH_PHASES.contains(&target.as_str()) {
        rec.insert("research_gate_status".into(), json!("pending"));
    }
    Ok(target)
}

fn publish_digest(ctx: &HookContext<'_>, rec: &Map<String, Value>, ev: &Evidence) {
    let baseline = field_str(rec, "research_baseline_id");
    let exceptions = match rec.get("research_exceptions") {
        Some(Value::Array(a)) if !a.is_empty() => Value::Array(a.clone()).to_string(),
        _ => "none".to_string(),
    };
    let digest = format!(
        "[EVIDENCE DIGEST]\nManifest: {}; revision: {}; phase digest: {}; profile: {}; current \
         research cell: none; coverage: unavailable. Baseline: {}; exceptions: {exceptions}. \
         Reuse verified artifacts by reference and cite their cell IDs.",
        ev.manifest_relative_path(),
        ev.revision(),
        ev.digest(),
        field_str(rec, "profile"),
        if baseline.is_empty() {
            "not published"
        } else {
            &baseline
        },
    );
    set_state(
        ctx.data,
        &[
            ("evidence_digest", digest),
            ("research_report", field_str(rec, "research_report")),
            ("research_baseline_id", baseline),
            ("reference_url", field_str(rec, "target_url")),
            ("target_directory", field_str(rec, "target_directory")),
            ("profile", field_str(rec, "profile")),
        ],
    );
}

fn require(value: &str, message: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(message.to_string())
    } else {
        Ok(())
    }
}

/// `(tool, status key)` for the infrastructure build/verify pairs.
const INFRA: &[(&str, &str, &str, &str, &str)] = &[
    // (submit tool, verified tool, rejected tool, status key, verify phase)
    (
        "submit_sqlite",
        "sqlite_verified",
        "sqlite_rejected",
        "sqlite",
        "verify_sqlite",
    ),
    (
        "submit_prisma",
        "prisma_verified",
        "prisma_rejected",
        "prisma",
        "verify_prisma",
    ),
    (
        "submit_tanstack",
        "tanstack_verified",
        "tanstack_rejected",
        "tanstack",
        "verify_tanstack",
    ),
    (
        "submit_env",
        "env_verified",
        "env_rejected",
        "env",
        "verify_env",
    ),
    (
        "submit_docker",
        "docker_verified",
        "docker_rejected",
        "docker",
        "verify_docker",
    ),
    (
        "submit_netlify",
        "netlify_verified",
        "netlify_rejected",
        "netlify",
        "verify_netlify",
    ),
    (
        "submit_caddy",
        "caddy_verified",
        "caddy_rejected",
        "caddy",
        "verify_caddy",
    ),
    (
        "submit_package",
        "package_verified",
        "package_rejected",
        "package",
        "verify_package",
    ),
    (
        "submit_scripts",
        "scripts_verified",
        "scripts_rejected",
        "scripts",
        "verify_scripts",
    ),
    (
        "submit_docs",
        "docs_verified",
        "docs_rejected",
        "docs",
        "verify_docs",
    ),
];

/// `(approve tool, reject tool, status key, issue severity, discrepancy list, list arg)`.
const VALIDATIONS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "visual_approved",
        "visual_rejected",
        "visual",
        "high",
        "visual_discrepancies",
        "discrepancies",
    ),
    (
        "interaction_approved",
        "interaction_rejected",
        "interaction",
        "high",
        "interaction_discrepancies",
        "discrepancies",
    ),
    (
        "a11y_approved",
        "a11y_rejected",
        "accessibility",
        "medium",
        "",
        "issues",
    ),
    (
        "perf_approved",
        "perf_rejected",
        "performance",
        "medium",
        "",
        "issues",
    ),
    (
        "fidelity_approved",
        "fidelity_rejected",
        "fidelity",
        "low",
        "visual_discrepancies",
        "discrepancies",
    ),
];

/// `(tool, research note type, note file)` for the research handoffs.
const RESEARCH_HANDOFFS: &[(&str, &str, &str, &str)] = &[
    (
        "submit_route_inventory",
        "reconnaissance",
        "route-inventory.md",
        "route_inventory",
    ),
    (
        "submit_visual_spec",
        "visual",
        "visual-observations.md",
        "visual_spec",
    ),
    (
        "submit_interaction_inventory",
        "interaction",
        "interaction-analysis.md",
        "interaction_inventory",
    ),
    (
        "submit_asset_inventory",
        "content_assets",
        "asset-inventory.md",
        "asset_inventory",
    ),
    (
        "submit_responsive_research",
        "responsive",
        "responsive-observations.md",
        "responsive_research",
    ),
    (
        "submit_architecture",
        "architecture",
        "architecture.md",
        "architecture",
    ),
    (
        "submit_design_system",
        "design_system",
        "design-system.md",
        "design_system",
    ),
];

const CONFIRMS: &[(&str, &str)] = &[
    ("confirm_bootstrap_healthy", "bootstrap"),
    ("confirm_global_shell", "global_shell"),
    ("confirm_component_system", "component_system"),
    ("confirm_data_layer", "data_layer"),
    ("confirm_responsive", "responsive"),
];

// ── The effect ─────────────────────────────────────────────────────────────

/// Apply the effect for `ctx.tool`.
pub fn apply(ctx: &HookContext<'_>) -> Result<EffectOutcome, String> {
    let mut rec = record(ctx.data, RECORD);
    let tool = ctx.tool;
    let summary = arg_str(ctx.args, "summary");

    if plan_index(ctx.phase).is_none() {
        return Err(format!(
            "the reconstruct_site effect routes reconstruct_site's own phases; '{}' is not one of \
             them",
            ctx.phase
        ));
    }
    if tool == "submit_initial_state" {
        return init(ctx, rec);
    }
    let mut ev = Evidence::open(ctx.data, &field_str(&rec, "profile"))?;

    let (route, note) = if let Some((_, note_type, file, artifact)) =
        RESEARCH_HANDOFFS.iter().find(|(t, ..)| *t == tool)
    {
        require(&summary, "summary must not be empty.")?;
        set_map_entry(&mut rec, "phase_summaries", ctx.phase, json!(summary));
        let notes = notes_path(&rec, note_type, file);
        set_map_entry(&mut rec, "artifacts", artifact, json!(notes));
        if ctx.phase == "recon" && field_list(&rec, "pages_to_implement").is_empty() {
            let desired = map_field(&rec, "research_scope")
                .get("desired_routes")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut routes: Vec<String> = desired
                .split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_string)
                .collect();
            if routes.is_empty() {
                routes.push("/".into());
            }
            rec.insert(
                "route_inventory".into(),
                Value::Array(routes.iter().map(|r| json!({"route": r})).collect()),
            );
            put_list(&mut rec, "pages_to_implement", &routes);
            rec.insert("page_index".into(), json!(0));
        }
        persist_phase(ctx, &mut rec, &mut ev, &summary)?;
        let next = next_active(&rec, ctx.phase);
        (
            route_to(ctx, &mut rec, &mut ev, next)?,
            "Research handoff recorded.".to_string(),
        )
    } else if let Some((_, key)) = CONFIRMS.iter().find(|(t, _)| *t == tool) {
        require(&summary, "summary must not be empty.")?;
        set_map_entry(&mut rec, "artifacts", key, json!(summary));
        persist_phase(ctx, &mut rec, &mut ev, &summary)?;
        let next = next_active(&rec, ctx.phase);
        (
            route_to(ctx, &mut rec, &mut ev, next)?,
            format!("{} confirmed.", ctx.phase),
        )
    } else if tool == "complete_page" {
        let page_route = arg_str(ctx.args, "page_route");
        if page_route.is_empty() || summary.is_empty() {
            return Err(
                "page_route and summary must not be empty. Implement the page fully, \
                 then call complete_page(route, summary)."
                    .into(),
            );
        }
        set_map_entry(
            &mut rec,
            "implementation_status",
            &page_route,
            json!("implemented"),
        );
        let index = field_u64(&rec, "page_index") + 1;
        rec.insert("page_index".into(), json!(index));
        persist_phase(ctx, &mut rec, &mut ev, &summary)?;
        rec.insert(
            "last_transition".into(),
            json!(format!("complete_page({page_route})")),
        );
        let route = route_to(ctx, &mut rec, &mut ev, Some("page".into()))?;
        (
            route,
            format!("Page {page_route} complete. Next page or the next phase."),
        )
    } else if tool == "approve_research_baseline" || tool == "approve_degraded_research" {
        gate_approve(ctx, &mut rec, &mut ev)?
    } else if tool == "reject_research_baseline" {
        let findings = arg_list(ctx.args, "findings");
        if findings.is_empty() {
            return Err(
                "findings must not be empty. List the missing or contradictory observations."
                    .into(),
            );
        }
        let target = arg_str(ctx.args, "target_phase");
        if target.is_empty() {
            return Err("target_phase must not be empty. Choose the earliest phase that can resolve the findings.".into());
        }
        let target = match reenter(ctx, &mut rec, &mut ev, &target) {
            Ok(t) => t,
            Err(e) => {
                rec.insert("fail_reason".into(), json!(e));
                known_issue(&mut rec, "research_gate", &e, "high");
                save_record(ctx.data, RECORD, rec);
                set_state(ctx.data, &[("research_gate_failure", e.clone())]);
                return Err(e);
            }
        };
        for f in &findings {
            known_issue(&mut rec, "research_gate", f, "high");
        }
        if !RESEARCH_PHASES.contains(&target.as_str()) {
            rec.insert("research_gate_status".into(), json!("rejected"));
        }
        persist_phase(ctx, &mut rec, &mut ev, &findings.join("; "))?;
        rec.insert("last_transition".into(), json!("reject_research_baseline"));
        let route = route_to(ctx, &mut rec, &mut ev, Some(target.clone()))?;
        (
            route,
            format!("Research rejection recorded; re-entering {target}."),
        )
    } else if let Some(v) = VALIDATIONS.iter().find(|v| v.0 == tool || v.1 == tool) {
        let (approve, _, key, severity, list_key, arg) = *v;
        if tool == approve {
            set_map_entry(&mut rec, "validation_status", key, json!("approved"));
            persist_phase(ctx, &mut rec, &mut ev, &summary)?;
            let next = next_active(&rec, ctx.phase);
            (
                route_to(ctx, &mut rec, &mut ev, next)?,
                format!("{} approved.", ctx.phase),
            )
        } else {
            let target = arg_str(ctx.args, "target_phase");
            let resolved = if target.is_empty() {
                None
            } else {
                Some(reenter(ctx, &mut rec, &mut ev, &target)?)
            };
            set_map_entry(&mut rec, "validation_status", key, json!("rejected"));
            rec.insert("last_transition".into(), json!(tool));
            if !list_key.is_empty() {
                for d in arg_objects(ctx.args, arg) {
                    push_entry(&mut rec, list_key, Value::Object(d));
                }
            }
            match resolved {
                Some(target) => {
                    known_issue(
                        &mut rec,
                        ctx.phase,
                        &format!("re-entering {target}"),
                        severity,
                    );
                    let route = route_to(ctx, &mut rec, &mut ev, Some(target.clone()))?;
                    (route, format!("Rejection recorded; re-entering {target}."))
                }
                None => (
                    Route::Phase(ctx.phase.to_string()),
                    format!("Rejection recorded; {} runs again.", ctx.phase),
                ),
            }
        }
    } else if let Some(i) = INFRA
        .iter()
        .find(|i| i.0 == tool || i.1 == tool || i.2 == tool)
    {
        let (submit, verified, _, key, _) = *i;
        if tool == submit || tool == verified {
            require(&summary, "summary must not be empty.")?;
            set_map_entry(
                &mut rec,
                "infra_status",
                key,
                json!(if tool == submit {
                    "submitted"
                } else {
                    "verified"
                }),
            );
            persist_phase(ctx, &mut rec, &mut ev, &summary)?;
            let next = next_active(&rec, ctx.phase);
            (
                route_to(ctx, &mut rec, &mut ev, next)?,
                format!("{} recorded.", ctx.phase),
            )
        } else {
            let issues = arg_list(ctx.args, "issues");
            if issues.is_empty() {
                return Err(
                    "issues must not be empty. List the concrete verification failures.".into(),
                );
            }
            for issue in &issues {
                known_issue(&mut rec, ctx.phase, issue, "medium");
            }
            set_map_entry(&mut rec, "infra_status", key, json!("rejected"));
            rec.insert("last_transition".into(), json!(tool));
            (
                Route::Phase(ctx.phase.to_string()),
                format!("{key} rejected. Fix the issues and retry this phase."),
            )
        }
    } else if tool == "final_approved" {
        require(&summary, "summary must not be empty.")?;
        set_map_entry(&mut rec, "validation_status", "final", json!("approved"));
        persist_phase(ctx, &mut rec, &mut ev, &summary)?;
        ev.manifest.insert("status".into(), json!("complete"));
        (
            Route::End,
            "Final validation passed. Workflow COMPLETED.".to_string(),
        )
    } else if tool == "final_blocked" {
        let issue = arg_str(ctx.args, "issue");
        require(
            &issue,
            "issue must not be empty. Document the material blocker precisely.",
        )?;
        rec.insert("fail_reason".into(), json!(issue));
        push_entry(&mut rec, "blocked_phases", json!("final_validation"));
        set_map_entry(&mut rec, "validation_status", "final", json!("blocked"));
        persist_phase(ctx, &mut rec, &mut ev, &issue)?;
        ev.manifest.insert("status".into(), json!("failed"));
        (
            Route::Exit,
            format!(
                "Blocker recorded. Workflow BLOCKED: {issue}. Give the user a one-paragraph \
                 explanation and stop."
            ),
        )
    } else {
        return Err(format!(
            "the reconstruct_site effect does not handle '{tool}'; attach it only to \
             reconstruct_site's transition tools"
        ));
    };

    ev.publish()?;
    rec.insert("artifact_manifest_revision".into(), json!(ev.revision()));
    publish_digest(ctx, &rec, &ev);
    save_record(ctx.data, RECORD, rec);
    Ok(EffectOutcome {
        note,
        route: Some(route),
    })
}

/// `submit_initial_state`: validate, fix the profile, open the evidence.
fn init(ctx: &HookContext<'_>, mut rec: Map<String, Value>) -> Result<EffectOutcome, String> {
    let reference = arg_str(ctx.args, "reference_url");
    require(
        &reference,
        "reference_url must not be empty. Provide the reference website URL.",
    )?;
    let safe = safe_reference_url(&reference);
    if safe.is_empty() {
        return Err(
            "reference_url must be an HTTP or HTTPS URL without invalid syntax. Provide \
             a reachable reference URL such as https://example.test/."
                .into(),
        );
    }
    let target_dir = arg_str(ctx.args, "target_directory");
    require(
        &target_dir,
        "target_directory must not be empty. Provide the target application directory.",
    )?;
    let is_static = arg_bool(ctx.args, "reference_is_static", true);

    // A resumed run keeps the profile it recorded; config cannot change it.
    let configured = match ctx.data.param("profile") {
        Some(Value::String(s)) => normalise(s),
        _ => String::new(),
    };
    let profile = match field_str(&rec, "profile") {
        recorded if !recorded.is_empty() => recorded,
        _ if !configured.is_empty() => configured,
        _ if is_static => "static".to_string(),
        _ => "application".to_string(),
    };
    let custom: Vec<String> = match ctx.data.param("custom_phases") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let phases = active_phases(&profile, &custom)
        .map_err(|e| format!("{e} (check [workflows.reconstruct_site] profile / custom_phases)"))?;
    let viewports = parse_viewports(
        ctx.data
            .param("viewports")
            .or_else(|| ctx.data.param("research_viewports")),
    )?;
    let skipped: Map<String, Value> = PLAN
        .iter()
        .filter(|p| !phases.contains(&p.name))
        .map(|p| (p.name.to_string(), json!(skip_reason(p.name, &profile))))
        .collect();

    let desired = arg_str(ctx.args, "desired_routes");
    let scope = json!({
        "reference_url": safe,
        "target_directory": target_dir,
        "desired_routes": desired,
        "auth_required": arg_bool(ctx.args, "auth_required", false),
        "reference_is_static": is_static,
        "reproduce_api_or_mock": match arg_str(ctx.args, "reproduce_api_or_mock") { s if s.is_empty() => "mock".to_string(), s => s },
        "constraints_provided": !arg_str(ctx.args, "constraints").is_empty(),
        "allowed_domains": arg_list(ctx.args, "allowed_domains"),
        "route_inclusion": arg_list(ctx.args, "route_inclusion"),
        "route_exclusion": arg_list(ctx.args, "route_exclusion"),
        "dynamic_content_policy": match arg_str(ctx.args, "dynamic_content_policy") { s if s.is_empty() => "observe".to_string(), s => s },
        "unavailable_behavior": match arg_str(ctx.args, "unavailable_behavior") { s if s.is_empty() => "report".to_string(), s => s },
        "fidelity_exceptions": arg_str(ctx.args, "fidelity_exceptions"),
    });

    rec.insert("plan_version".into(), json!(PLAN_VERSION));
    rec.insert("profile".into(), json!(profile));
    rec.insert("active_phases".into(), json!(phases));
    rec.insert("skipped_reasons".into(), Value::Object(skipped.clone()));
    rec.insert("target_url".into(), json!(safe));
    rec.insert("target_directory".into(), json!(target_dir));
    rec.insert("research_scope".into(), scope.clone());
    rec.insert("research_viewports".into(), json!(viewports));
    rec.entry("research_gate_status")
        .or_insert(json!("pending"));
    set_map_entry(
        &mut rec,
        "phase_summaries",
        "init",
        json!(format!(
            "url={safe}; dir={target_dir}; static={}; api={}",
            if is_static { "True" } else { "False" },
            scope["reproduce_api_or_mock"].as_str().unwrap_or("mock")
        )),
    );
    let notes = notes_path(&rec, "initial_state", "initial-state.md");
    set_map_entry(&mut rec, "artifacts", "initial_state", json!(notes));

    let mut ev = Evidence::open(ctx.data, &profile)?;
    ev.manifest.insert("profile".into(), json!(profile));
    ev.manifest.insert(
        "skipped_phases".into(),
        Value::Array(
            skipped
                .iter()
                .map(|(k, v)| json!({"phase": k, "reason": v}))
                .collect(),
        ),
    );
    let browser = ["cloakbrowser_screenshot", "playwright_screenshot"]
        .iter()
        .find(|t| ctx.data.has_tool(t))
        .map(|t| t.split('_').next().unwrap_or_default().to_string());
    match &browser {
        Some(family) => {
            rec.insert("browser_capability_status".into(), json!("available"));
            rec.insert("browser_backend".into(), json!(family));
        }
        None => {
            rec.insert("browser_capability_status".into(), json!("unavailable"));
            rec.insert("browser_backend".into(), json!("unavailable"));
            if field_str(&rec, "screenshot_status").is_empty()
                || field_str(&rec, "screenshot_status") == "pending"
            {
                // agenthicc's `record_degraded_screenshot`: say which
                // capability is missing instead of inventing an image.
                ev.list_mut("screenshots").push(json!({
                    "role": "reference",
                    "route": "*",
                    "viewport": "all",
                    "backend": "unavailable",
                    "status": "degraded",
                    "reason": "No browser integration is configured for this session.",
                }));
                rec.insert("screenshot_status".into(), json!("degraded"));
            }
        }
    }
    rec.insert(
        "artifact_manifest_path".into(),
        json!(ev.manifest_relative_path()),
    );
    persist_phase(ctx, &mut rec, &mut ev, &format!("initial state for {safe}"))?;
    let next = next_active(&rec, "init");
    let route = route_to(ctx, &mut rec, &mut ev, next)?;
    ev.publish()?;
    rec.insert("artifact_manifest_revision".into(), json!(ev.revision()));
    publish_digest(ctx, &rec, &ev);
    save_record(ctx.data, RECORD, rec);
    Ok(EffectOutcome {
        note: format!(
            "Initial state recorded for {safe} → {target_dir} (profile {profile}, {} phases). \
             Reconnaissance starts next.",
            phases.len()
        ),
        route: Some(route),
    })
}

/// `approve_research_baseline` / `approve_degraded_research`.
fn gate_approve(
    ctx: &HookContext<'_>,
    rec: &mut Map<String, Value>,
    ev: &mut Evidence,
) -> Result<(Route, String), String> {
    let degraded = ctx.tool == "approve_degraded_research";
    let baseline = arg_str(ctx.args, "baseline_artifact_id");
    let text = arg_str(ctx.args, if degraded { "rationale" } else { "summary" });
    if text.is_empty() || baseline.is_empty() {
        return Err(if degraded {
            "exception_ids, rationale, and baseline_artifact_id are required. List each accepted \
             unavailable cell and explain the limitation."
        } else {
            "summary and baseline_artifact_id are required. Resolve all blocking cells, then \
             approve the current baseline."
        }
        .into());
    }
    if baseline != field_str(rec, "research_baseline_id") {
        let reason = "research approval referenced a stale baseline";
        rec.insert("fail_reason".into(), json!(reason));
        known_issue(rec, "research_gate", reason, "high");
        save_record(ctx.data, RECORD, rec.clone());
        set_state(ctx.data, &[("research_gate_failure", reason.to_string())]);
        return Err(format!(
            "{reason}; cite the current baseline id from the [EVIDENCE DIGEST] ({}).",
            field_str(rec, "research_baseline_id")
        ));
    }
    let exceptions = arg_list(ctx.args, "exception_ids");
    let status = if degraded {
        "approved_degraded"
    } else {
        "approved"
    };
    let decision = json!({
        "status": status,
        "approved": true,
        "profile": field_str(rec, "profile"),
        "summary": text,
        "baseline_artifact_id": baseline,
        "blocking_cell_ids": [],
        "exception_ids": exceptions,
    });
    let attempt = map_field(rec, "phase_attempts")
        .get("research_gate")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    ev.put_json("research_gate_receipt", "research_gate", attempt, &decision)?;
    rec.insert("research_gate_status".into(), json!(status));
    rec.insert("research_gate_decision".into(), decision);
    rec.insert(
        "research_exceptions".into(),
        Value::Array(
            exceptions
                .iter()
                .map(|e| json!({"exception_id": e, "status": "accepted"}))
                .collect(),
        ),
    );
    persist_phase(ctx, rec, ev, &text)?;
    let next = next_active(rec, "research_gate");
    let route = route_to(ctx, rec, ev, next)?;
    Ok((
        route,
        if degraded {
            "Degraded research approval recorded.".into()
        } else {
            "Research baseline approval recorded.".into()
        },
    ))
}

// ── The infrastructure guard ───────────────────────────────────────────────

fn target_dir(data: &RunData) -> Option<PathBuf> {
    let wd = data.working_dir()?.to_path_buf();
    let rec = record(data, RECORD);
    let dir = field_str(&rec, "target_directory");
    let dir = dir.trim();
    Some(if dir.is_empty() {
        wd
    } else if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        wd.join(dir)
    })
}

fn package_json(root: &Path) -> Option<Value> {
    std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// The `infra_artifacts_present` guard: a `*_verified` call is accepted only
/// when the files its build phase was told to create exist in the target
/// directory. An xaft addition — agenthicc's verify phases are checklists the
/// model runs; this makes the artifacts' presence a deterministic
/// precondition. `verify_sqlite` has no fixed file name and is not checked.
pub fn check_infra_artifacts(ctx: &HookContext<'_>) -> Result<(), String> {
    let Some(root) = target_dir(ctx.data) else {
        return Ok(());
    };
    let mut missing: Vec<String> = Vec::new();
    let files = |names: &[&str], missing: &mut Vec<String>| {
        for n in names {
            if !root.join(n).exists() {
                missing.push((*n).to_string());
            }
        }
    };
    match ctx.phase {
        "verify_prisma" => files(&["prisma/schema.prisma"], &mut missing),
        "verify_env" => files(
            &[".env.local", ".env.example", ".env.prod", ".env.netlify"],
            &mut missing,
        ),
        "verify_docker" => files(
            &[
                "Dockerfile",
                "docker-compose.yaml",
                "docker-compose-dev.yaml",
                ".dockerignore",
            ],
            &mut missing,
        ),
        "verify_netlify" => files(&["netlify.toml"], &mut missing),
        "verify_caddy" => files(&["Caddyfile"], &mut missing),
        "verify_tanstack" => {
            let pkg = package_json(&root);
            let declared = pkg.as_ref().is_some_and(|p| {
                ["dependencies", "devDependencies"]
                    .iter()
                    .any(|k| p[k].get("@tanstack/react-query").is_some())
            });
            if !declared {
                missing.push("@tanstack/react-query in package.json dependencies".into());
            }
        }
        "verify_package" => match package_json(&root) {
            None => missing.push("package.json".into()),
            Some(pkg) => {
                for script in [
                    "lint",
                    "typecheck",
                    "test",
                    "check",
                    "db:generate",
                    "db:migrate",
                    "db:migrate:deploy",
                    "db:seed",
                    "db:reset",
                    "db:studio",
                ] {
                    if pkg["scripts"].get(script).is_none() {
                        missing.push(format!("package.json script \"{script}\""));
                    }
                }
            }
        },
        "verify_scripts" => {
            let has_sh = std::fs::read_dir(root.join("scripts"))
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("sh"))
                })
                .unwrap_or(false);
            if !has_sh {
                missing.push("scripts/*.sh".into());
            }
            files(&["docs/scripts.md"], &mut missing);
        }
        "verify_docs" => {
            let has_md = std::fs::read_dir(root.join("docs"))
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
                })
                .unwrap_or(false);
            if !has_md {
                missing.push("docs/*.md".into());
            }
        }
        _ => {}
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the target directory {} is missing: {}. Create them (or call the phase's *_rejected \
             tool with the issues) before verifying.",
            root.display(),
            missing.join(", ")
        ))
    }
}

// ── The screenshot-linking tool ────────────────────────────────────────────

/// The `reconstruct` tool group for one phase run.
pub fn reconstruct_tools(data: &RunData, phase: &str, attempt: u32) -> Vec<Arc<ErasedTool>> {
    vec![Arc::new(ScreenshotTool {
        data: data.clone(),
        phase: phase.to_string(),
        attempt: u64::from(attempt.max(1)),
    }) as Arc<ErasedTool>]
}

/// agenthicc's `record_reconstruct_screenshot`.
struct ScreenshotTool {
    data: RunData,
    phase: String,
    attempt: u64,
}

impl ScreenshotTool {
    fn record(&self, input: &Value) -> Result<Value, String> {
        let s = |k: &str| arg_str(input, k);
        let role = s("role");
        if !["reference", "implementation", "exploratory"].contains(&role.as_str()) {
            return Err(format!("invalid screenshot role '{role}'"));
        }
        let width = input.get("width").and_then(Value::as_i64).unwrap_or(-1);
        let height = input.get("height").and_then(Value::as_i64).unwrap_or(-1);
        let scale = input
            .get("device_scale")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        if width < 0 || height < 0 || scale <= 0.0 {
            return Err("invalid screenshot dimensions".into());
        }
        let artifact_id = s("browser_artifact_id");
        let rel = s("browser_artifact_path");
        if artifact_id.is_empty() || rel.is_empty() {
            return Err("browser artifact must include id and path".into());
        }
        let wd = self
            .data
            .working_dir()
            .ok_or("the run has no working directory")?;
        let path = if Path::new(&rel).is_absolute() {
            PathBuf::from(&rel)
        } else {
            wd.join(&rel)
        };
        let canonical = path
            .canonicalize()
            .map_err(|_| "browser screenshot is unreadable".to_string())?;
        let root = wd.canonicalize().map_err(|e| e.to_string())?;
        let relative = canonical
            .strip_prefix(&root)
            .map_err(|_| "browser screenshot is outside the workspace".to_string())?
            .display()
            .to_string();
        let bytes = std::fs::read(&canonical)
            .map_err(|_| "browser screenshot is unreadable".to_string())?;
        let digest = sha256_hex(&bytes);
        let url = safe_reference_url(&s("url"));
        let page_state = match s("page_state") {
            p if p.is_empty() => "default".to_string(),
            p => p,
        };
        let backend = match s("backend") {
            b if b.is_empty() => "unknown".to_string(),
            b => b,
        };
        let identity = json!({
            "role": role, "route": s("route"), "url": url, "viewport": s("viewport"),
            "width": width, "height": height, "device_scale": scale, "page_state": page_state,
            "backend": backend, "sha256": digest, "source_revision": s("source_revision"),
        });
        let screenshot_id = sha256_hex(identity.to_string().as_bytes());

        let mut rec = record(&self.data, RECORD);
        let mut ev = Evidence::open(&self.data, &field_str(&rec, "profile"))?;
        let duplicate = ev
            .list_mut("screenshots")
            .iter()
            .any(|x| x["screenshot_id"] == json!(screenshot_id));
        if !duplicate {
            ev.list_mut("artifacts").push(json!({
                "artifact_id": artifact_id,
                "kind": "screenshot",
                "relative_path": relative,
                "media_type": if relative.ends_with(".png") { "image/png" } else { "image/jpeg" },
                "sha256": digest,
                "byte_count": bytes.len(),
                "phase": self.phase,
                "attempt": self.attempt,
                "source": "browser",
                "status": "complete",
                "source_cells": arg_list(input, "source_cells"),
                "created_at": now(),
            }));
            let mut shot = identity.clone();
            shot["screenshot_id"] = json!(screenshot_id);
            shot["artifact_id"] = json!(artifact_id);
            shot["status"] = json!("complete");
            shot["redaction_status"] = json!(match s("redaction_status") {
                r if r.is_empty() => "not_reported".to_string(),
                r => r,
            });
            for flag in ["fonts_loaded", "images_loaded", "network_complete"] {
                shot[flag] = input
                    .get(flag)
                    .filter(|v| v.is_boolean())
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            ev.list_mut("screenshots").push(shot);
            ev.publish()?;
        }
        rec.insert("screenshot_status".into(), json!("complete"));
        let mut ids = field_list(&rec, "screenshot_ids");
        if !ids.contains(&screenshot_id) {
            ids.push(screenshot_id.clone());
        }
        put_list(&mut rec, "screenshot_ids", &ids);
        rec.insert("artifact_manifest_revision".into(), json!(ev.revision()));
        save_record(&self.data, RECORD, rec);
        Ok(json!({ "ok": true, "screenshot_id": screenshot_id, "artifact_id": artifact_id }))
    }
}

#[async_trait::async_trait]
impl Tool for ScreenshotTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "record_reconstruct_screenshot"
    }

    fn description(&self) -> &str {
        "Link an existing browser screenshot to reconstruct evidence. The screenshot must already \
         have been written by a playwright_screenshot or cloakbrowser_screenshot call (pass the \
         artifact id and path it returned). This tool never accepts raw bytes and never opens a \
         second browser client."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "browser_artifact_id": { "type": "string" },
                "browser_artifact_path": { "type": "string" },
                "role": { "type": "string", "description": "reference, implementation or exploratory" },
                "route": { "type": "string" },
                "url": { "type": "string" },
                "viewport": { "type": "string" },
                "width": { "type": "integer" },
                "height": { "type": "integer" },
                "device_scale": { "type": "number" },
                "page_state": { "type": "string" },
                "backend": { "type": "string" },
                "source_cells": { "type": "array", "items": { "type": "string" } },
                "source_revision": { "type": "string" },
                "fonts_loaded": { "type": ["boolean", "null"] },
                "images_loaded": { "type": ["boolean", "null"] },
                "network_complete": { "type": ["boolean", "null"] },
                "redaction_status": { "type": "string" }
            },
            "required": ["browser_artifact_id", "browser_artifact_path", "role", "route", "url", "viewport", "width", "height"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        Ok(match self.record(&input) {
            Ok(body) => ToolResult::ok(body.to_string(), &ctx.tool_use_id),
            Err(e) => ToolResult::error(
                json!({"ok": false, "error": e}).to_string(),
                &ctx.tool_use_id,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::groups::RunMeta;
    use crate::phased::tools::{PhaseControl, SharedControl};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn data(tmp: &Path, params: Value, tools: &[&str]) -> RunData {
        RunData::new(BTreeMap::new(), tmp).with_meta(RunMeta {
            run_id: "run1".into(),
            workflow: "reconstruct_site".into(),
            intent: "rebuild https://ref.test".into(),
            params: params.as_object().cloned().unwrap_or_default(),
            tool_names: tools.iter().map(|t| t.to_string()).collect(),
        })
    }

    fn call(data: &RunData, phase: &str, tool: &str, args: Value) -> Result<EffectOutcome, String> {
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        apply(&HookContext {
            data,
            control: &control,
            args: &args,
            phase,
            tool,
        })
    }

    fn next(out: &EffectOutcome) -> String {
        match &out.route {
            Some(Route::Phase(p)) => p.clone(),
            Some(Route::End) => "END".into(),
            Some(Route::Exit) => "EXIT".into(),
            None => "-".into(),
        }
    }

    fn state(data: &RunData, key: &str) -> String {
        data.get("state")
            .and_then(|s| s.get(key).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    fn init_args(is_static: bool) -> Value {
        json!({
            "reference_url": "https://user:pw@Ref.Test/home?token=1#x",
            "target_directory": "site",
            "desired_routes": "/, /about",
            "reference_is_static": is_static,
        })
    }

    #[test]
    fn profiles_select_the_documented_graph_sizes() {
        assert_eq!(active_phases("static", &[]).unwrap().len(), 20);
        assert_eq!(active_phases("application", &[]).unwrap().len(), 21);
        assert_eq!(active_phases("production", &[]).unwrap().len(), 41);
        assert_eq!(PLAN.len(), 41);
        assert!(
            active_phases("galactic", &[])
                .unwrap_err()
                .contains("unknown reconstruct profile")
        );
    }

    #[test]
    fn custom_profiles_are_validated_like_agenthicc() {
        let ok: Vec<String> = [
            "init",
            "recon",
            "research_gate",
            "bootstrap",
            "page",
            "final_validation",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(active_phases("custom", &ok).unwrap().len(), 6);
        for (phases, message) in [
            (vec![], "at least one phase"),
            (
                vec!["init", "warp", "final_validation"],
                "unknown phases: warp",
            ),
            (
                vec!["recon", "research_gate", "final_validation"],
                "boundaries",
            ),
            (vec!["init", "recon", "final_validation"], "research_gate"),
            (
                vec!["init", "research_gate", "research_gate", "final_validation"],
                "duplicate",
            ),
            (
                vec!["init", "page", "research_gate", "final_validation"],
                "plan order",
            ),
        ] {
            let phases: Vec<String> = phases.iter().map(|s| s.to_string()).collect();
            let err = active_phases("custom", &phases).unwrap_err();
            assert!(err.contains(message), "{err}");
        }
    }

    #[test]
    fn reference_urls_lose_credentials_and_queries() {
        assert_eq!(
            safe_reference_url("https://u:p@Ref.Test:8443/a/b?q=1#f"),
            "https://ref.test:8443/a/b"
        );
        assert_eq!(safe_reference_url("ftp://x.test"), "");
        assert_eq!(safe_reference_url("not a url"), "");
    }

    #[test]
    fn a_static_run_skips_the_data_layer_and_infrastructure() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &["cloakbrowser_screenshot"]);
        let out = call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        assert_eq!(next(&out), "recon");
        let rec = record(&d, RECORD);
        assert_eq!(rec["profile"], "static");
        assert_eq!(rec["target_url"], "https://ref.test/home");
        assert_eq!(rec["browser_backend"], "cloakbrowser");
        assert!(
            rec["skipped_reasons"]["docker"]
                .as_str()
                .unwrap()
                .contains("static reconstruction scope")
        );
        assert!(state(&d, "evidence_digest").contains(".xaft/reconstruct_site/run1/manifest.json"));
        assert!(
            tmp.path()
                .join(".xaft/reconstruct_site/run1/manifest.json")
                .is_file()
        );

        // The last page routes past data_layer (application only).
        let mut rec = record(&d, RECORD);
        rec.insert("pages_to_implement".into(), json!(["/"]));
        rec.insert("page_index".into(), json!(0));
        save_record(&d, RECORD, rec);
        let out = call(
            &d,
            "page",
            "complete_page",
            json!({"page_route": "/", "summary": "done"}),
        )
        .unwrap();
        assert_eq!(next(&out), "responsive_pass");
        // The fidelity pass routes straight to final validation.
        let out = call(
            &d,
            "fidelity_pass",
            "fidelity_approved",
            json!({"summary": "polished"}),
        )
        .unwrap();
        assert_eq!(next(&out), "final_validation");
    }

    #[test]
    fn a_dynamic_reference_or_configured_profile_widens_the_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &[]);
        call(&d, "init", "submit_initial_state", init_args(false)).unwrap();
        let rec = record(&d, RECORD);
        assert_eq!(rec["profile"], "application");
        assert_eq!(rec["browser_capability_status"], "unavailable");
        assert_eq!(rec["screenshot_status"], "degraded");

        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({"profile": "production"}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        let out = call(
            &d,
            "fidelity_pass",
            "fidelity_approved",
            json!({"summary": "ok"}),
        )
        .unwrap();
        assert_eq!(next(&out), "sqlite_db");

        let tmp = tempfile::tempdir().unwrap();
        let d = data(
            tmp.path(),
            json!({"profile": "custom", "custom_phases": "init, warp, final_validation"}),
            &[],
        );
        let err = call(&d, "init", "submit_initial_state", init_args(true)).unwrap_err();
        assert!(err.contains("unknown phases: warp"), "{err}");
    }

    #[test]
    fn recon_fixes_the_routes_and_the_page_loop_walks_them() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        call(
            &d,
            "recon",
            "submit_route_inventory",
            json!({"summary": "two routes"}),
        )
        .unwrap();
        let rec = record(&d, RECORD);
        assert_eq!(rec["pages_to_implement"], json!(["/", "/about"]));
        assert_eq!(
            rec["artifacts"]["route_inventory"],
            "site/research/reconnaissance/route-inventory.md"
        );

        let out = call(
            &d,
            "component_system",
            "confirm_component_system",
            json!({"summary": "lib"}),
        )
        .unwrap();
        assert_eq!(next(&out), "page");
        assert_eq!(
            (
                state(&d, "page_route").as_str(),
                state(&d, "page_number").as_str()
            ),
            ("/", "1")
        );
        let out = call(
            &d,
            "page",
            "complete_page",
            json!({"page_route": "/", "summary": "home"}),
        )
        .unwrap();
        assert_eq!(next(&out), "page");
        assert_eq!(state(&d, "page_route"), "/about");
        assert_eq!(state(&d, "page_count"), "2");
        let out = call(
            &d,
            "page",
            "complete_page",
            json!({"page_route": "/about", "summary": "about"}),
        )
        .unwrap();
        assert_eq!(next(&out), "responsive_pass");
        assert!(
            call(
                &d,
                "page",
                "complete_page",
                json!({"page_route": "", "summary": "x"})
            )
            .is_err()
        );
    }

    #[test]
    fn the_gate_requires_the_current_baseline_and_rejection_reenters() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({"max_reentries": 1}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        let out = call(
            &d,
            "design_system",
            "submit_design_system",
            json!({"summary": "tokens"}),
        )
        .unwrap();
        assert_eq!(next(&out), "research_gate");
        let baseline = state(&d, "research_baseline_id");
        assert_eq!(baseline.len(), 64, "a sha256 artifact id");
        assert!(state(&d, "research_report").contains("summary_only"));
        assert!(state(&d, "evidence_digest").contains(&baseline));

        let stale = call(
            &d,
            "research_gate",
            "approve_research_baseline",
            json!({"summary": "ok", "baseline_artifact_id": "old"}),
        );
        assert!(stale.unwrap_err().contains("stale baseline"));

        let out = call(
            &d,
            "research_gate",
            "reject_research_baseline",
            json!({"findings": ["no mobile"], "target_phase": "Visual-Research"}),
        )
        .unwrap();
        assert_eq!(next(&out), "visual_research");
        let rec = record(&d, RECORD);
        assert_eq!(rec["research_gate_status"], "pending");
        assert_eq!(rec["reentry_count"], 1);
        let err = call(
            &d,
            "research_gate",
            "reject_research_baseline",
            json!({"findings": ["x"], "target_phase": "recon"}),
        )
        .unwrap_err();
        assert!(err.contains("re-entry budget exhausted (1)"), "{err}");

        // A fresh baseline is published on the next entry; approving it opens bootstrap.
        call(
            &d,
            "design_system",
            "submit_design_system",
            json!({"summary": "tokens v2"}),
        )
        .unwrap();
        let baseline = state(&d, "research_baseline_id");
        let out = call(
            &d,
            "research_gate",
            "approve_research_baseline",
            json!({"summary": "complete", "baseline_artifact_id": baseline}),
        )
        .unwrap();
        assert_eq!(next(&out), "bootstrap");
        assert_eq!(record(&d, RECORD)["research_gate_status"], "approved");
    }

    #[test]
    fn validation_rejections_retry_or_reenter_within_the_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        let out = call(
            &d,
            "visual_validation",
            "visual_rejected",
            json!({"discrepancies": [{"page": "/", "issue": "gap"}], "target_phase": ""}),
        )
        .unwrap();
        assert_eq!(
            next(&out),
            "visual_validation",
            "no target retries the phase"
        );
        let err = call(
            &d,
            "visual_validation",
            "visual_rejected",
            json!({"target_phase": "docker"}),
        )
        .unwrap_err();
        assert!(
            err.contains("unknown phase 'docker' for profile 'static'"),
            "{err}"
        );
        let out = call(
            &d,
            "accessibility",
            "a11y_rejected",
            json!({"issues": [], "target_phase": "global_shell"}),
        )
        .unwrap();
        assert_eq!(next(&out), "global_shell");
        let rec = record(&d, RECORD);
        assert_eq!(
            rec["visual_discrepancies"],
            json!([{"page": "/", "issue": "gap"}])
        );
        assert_eq!(rec["validation_status"]["accessibility"], "rejected");
        assert_eq!(
            rec["research_gate_status"], "pending",
            "an implementation target keeps the gate as it was"
        );
    }

    #[test]
    fn infra_rejections_retry_and_final_states_end_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({"profile": "production"}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        let out = call(
            &d,
            "verify_docker",
            "docker_rejected",
            json!({"issues": ["no healthcheck"]}),
        )
        .unwrap();
        assert_eq!(next(&out), "verify_docker");
        assert!(
            call(
                &d,
                "verify_docker",
                "docker_rejected",
                json!({"issues": []})
            )
            .is_err()
        );
        let out = call(
            &d,
            "verify_docs",
            "docs_verified",
            json!({"summary": "audited"}),
        )
        .unwrap();
        assert_eq!(next(&out), "final_validation");
        let out = call(
            &d,
            "final_validation",
            "final_blocked",
            json!({"issue": "build fails"}),
        )
        .unwrap();
        assert_eq!(next(&out), "EXIT");
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".xaft/reconstruct_site/run1/manifest.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["status"], "failed");
        assert!(
            manifest["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["kind"] == "phase_receipt")
        );
    }

    #[test]
    fn the_infra_guard_checks_the_files_each_phase_must_create() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &[]);
        call(&d, "init", "submit_initial_state", init_args(true)).unwrap();
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        let args = json!({});
        let check = |phase: &str| {
            check_infra_artifacts(&HookContext {
                data: &d,
                control: &control,
                args: &args,
                phase,
                tool: "x",
            })
        };
        let err = check("verify_docker").unwrap_err();
        assert!(
            err.contains("Dockerfile") && err.contains(".dockerignore"),
            "{err}"
        );
        let site = tmp.path().join("site");
        std::fs::create_dir_all(&site).unwrap();
        for f in [
            "Dockerfile",
            "docker-compose.yaml",
            "docker-compose-dev.yaml",
            ".dockerignore",
        ] {
            std::fs::write(site.join(f), "x").unwrap();
        }
        assert!(check("verify_docker").is_ok());
        std::fs::write(
            site.join("package.json"),
            r#"{"dependencies": {"@tanstack/react-query": "5"}, "scripts": {"lint": "x"}}"#,
        )
        .unwrap();
        assert!(check("verify_tanstack").is_ok());
        assert!(check("verify_package").unwrap_err().contains("db:seed"));
        assert!(
            check("verify_sqlite").is_ok(),
            "no fixed file name to check"
        );
        assert!(check("bootstrap").is_ok(), "only verify phases are checked");
    }

    #[tokio::test]
    async fn screenshots_are_linked_once_and_must_stay_in_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(tmp.path(), json!({}), &[]);
        std::fs::create_dir_all(tmp.path().join(".xaft/browser")).unwrap();
        std::fs::write(tmp.path().join(".xaft/browser/a.png"), b"png").unwrap();
        let tools = reconstruct_tools(&d, "visual_research", 1);
        let ctx = ToolContext::new("t");
        let args = json!({
            "browser_artifact_id": "a1", "browser_artifact_path": ".xaft/browser/a.png",
            "role": "reference", "route": "/", "url": "https://ref.test/", "viewport": "mobile",
            "width": 390, "height": 844
        });
        let r = tools[0].call(args.clone(), &ctx).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        tools[0].call(args, &ctx).await.unwrap();
        let rec = record(&d, RECORD);
        assert_eq!(
            rec["screenshot_ids"].as_array().unwrap().len(),
            1,
            "idempotent"
        );
        assert_eq!(rec["screenshot_status"], "complete");

        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("b.png");
        std::fs::write(&outside, b"png").unwrap();
        let r = tools[0]
            .call(json!({
                "browser_artifact_id": "a2", "browser_artifact_path": outside.display().to_string(),
                "role": "reference", "route": "/", "url": "", "viewport": "mobile", "width": 1, "height": 1
            }), &ctx)
            .await
            .unwrap();
        assert!(
            r.is_error && r.content.contains("outside the workspace"),
            "{}",
            r.content
        );
        let r = tools[0]
            .call(json!({"browser_artifact_id": "a", "browser_artifact_path": "x", "role": "hero", "route": "/", "url": "", "viewport": "m", "width": 1, "height": 1}), &ctx)
            .await
            .unwrap();
        assert!(r.content.contains("invalid screenshot role"));
    }
}
