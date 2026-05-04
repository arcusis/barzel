mod cache;
mod cli;
mod compare;
mod config;
mod detect;
mod diff;
mod error;
mod history;
mod http;
mod init;
mod orchestrator;
mod plugin;
mod process;
mod report;
mod run;
mod runners;
mod tool_registry;

use chrono::Utc;
use clap::Parser;
use cli::{Cli, Commands};
use owo_colors::OwoColorize;
use report::{BarzelReport, LayerStatus, ReportStatus, Severity};
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;
use uuid::Uuid;

// ── Stdio protocol ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct StdioRequest {
    command: String,
    #[serde(default)]
    project_path: Option<String>,
    #[serde(default)]
    layers: Option<Vec<String>>,
    #[serde(default)]
    no_cache: bool,
    #[serde(default)]
    fail_fast: bool,
    #[serde(default)]
    request_id: Option<String>,
    /// Git revision for diff mode: only verify packages changed since this rev.
    #[serde(default)]
    since: Option<String>,
    /// Report id/prefix or "latest" for the `report` command.
    #[serde(default)]
    id: Option<String>,
    /// Two-element array `[baseline_ref, head_ref]` for the `report` compare command.
    #[serde(default)]
    compare: Option<Vec<String>>,
    /// Maximum number of history entries to return. Default 20, cap 200. 0 returns empty.
    #[serde(default)]
    limit: Option<usize>,
    /// Filter history entries by exact package_path (workspace member path).
    #[serde(default)]
    package_path: Option<String>,
    /// Filter history entries by exact language string.
    #[serde(default)]
    language: Option<String>,
}

#[derive(serde::Serialize)]
struct StdioResponse {
    status: String,
    request_id: String,
    timestamp: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn create_response(
    status: &str,
    request_id: Option<String>,
    data: Option<serde_json::Value>,
    error: Option<String>,
) -> StdioResponse {
    StdioResponse {
        status: status.to_string(),
        request_id: request_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        timestamp: Utc::now().to_rfc3339(),
        version: "1".to_string(),
        data,
        error,
    }
}

fn handle_stdio() -> ExitCode {
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        emit_error(None, format!("failed to read stdin: {}", e));
        return ExitCode::from(1);
    }

    let req: StdioRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            emit_error(None, format!("invalid JSON request: {}", e));
            return ExitCode::from(1);
        }
    };

    let request_id = req.request_id.clone();

    match req.command.as_str() {
        "init" => {
            let path = req.project_path.as_deref().map(Path::new);
            let target = path.unwrap_or_else(|| Path::new("."));
            match init::run_init(Some(target), true) {
                Ok(()) => {
                    let project = detect::detect_project(target).ok();
                    let resp = create_response(
                        "success",
                        request_id,
                        Some(serde_json::json!({
                            "message": "project initialized",
                            "config_file": ".barzel.toml",
                            "language": project.as_ref().map(|p| p.language.to_string()),
                            "frameworks": project.as_ref().map(|p| serde_json::json!({
                                "is_nextjs": p.frameworks.is_nextjs,
                                "has_ai_deps": p.frameworks.has_ai_deps,
                                "ai_frameworks": p.frameworks.ai_frameworks,
                            })),
                            "config_schema": {
                                "layers.enabled": "list of layers to run: logic, structural, hostile, operational",
                                "layers.structural.mutation_testing": "bool — enable mutation testing",
                                "layers.structural.mutation_threshold": "float 0-100 — minimum mutation score to pass",
                                "fail_on": "minimum severity to fail: critical, high, medium, low",
                            },
                            "next_steps": [
                                "Run `barzel run` to verify your project",
                                "Use `{\"command\":\"run\"}` via --stdio for agent-mode output",
                            ]
                        })),
                        None,
                    );
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "run" => {
            let path = req.project_path.as_deref().map(Path::new);
            let since = req.since.as_deref();
            match run::run_verification(path, req.layers, req.no_cache, req.fail_fast, true, false, since) {
                Ok(report) => {
                    let exit_code = report_exit_code(&report);
                    let data = build_run_data(&report);
                    let resp = create_response("success", request_id, Some(data), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    exit_code
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "check" => {
            let path = req.project_path.as_deref().map(Path::new);
            let target = path.unwrap_or_else(|| std::path::Path::new("."));
            match detect::detect_workspace(target) {
                Ok(workspace) => {
                    use crate::process::OsProcessRunner;
                    let cfg = config::BarzelConfig::load_for_project(target);
                    if let Err(e) = cfg.validate() {
                        emit_error(request_id, e.to_string());
                        return ExitCode::from(1);
                    }
                    let payload = build_check_payload(workspace, &cfg, &OsProcessRunner);
                    let resp = create_response("success", request_id, Some(payload), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "report" => {
            let target = req.project_path.as_deref().unwrap_or(".");
            match build_stdio_report_payload(target, req.id.as_deref(), req.compare.as_deref()) {
                Ok(payload) => {
                    let resp = create_response("success", request_id, Some(payload), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "history" => {
            let target = req.project_path.as_deref().unwrap_or(".");
            let payload = build_stdio_history_payload(
                target,
                req.limit,
                req.package_path.as_deref(),
                req.language.as_deref(),
            );
            let resp = create_response("success", request_id, Some(payload), None);
            println!("{}", serde_json::to_string(&resp).unwrap());
            ExitCode::SUCCESS
        }

        other => {
            emit_error(request_id, format!("unknown command: '{}'. Valid commands: init, run, check, report, history", other));
            ExitCode::from(1)
        }
    }
}

/// Build the `data` payload for a stdio `history` command.
///
/// Returns entries newest-first, filtered by optional package_path and language.
/// Default limit: 20. Cap: 200. limit=0 returns an empty entries array.
fn build_stdio_history_payload(
    target: &str,
    limit: Option<usize>,
    package_path: Option<&str>,
    language: Option<&str>,
) -> serde_json::Value {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 200;

    let effective_limit = match limit {
        None => DEFAULT_LIMIT,
        Some(n) => n.min(MAX_LIMIT),
    };

    // load_history_entries returns ascending; reverse for newest-first.
    let mut all: Vec<crate::history::HistoryEntry> =
        crate::history::load_history_entries(std::path::Path::new(target));
    all.reverse();

    // Apply optional exact filters.
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|e| {
            package_path
                .map(|p| e.package_path.as_deref() == Some(p))
                .unwrap_or(true)
                && language.map(|l| e.language == l).unwrap_or(true)
        })
        .collect();

    let total = filtered.len();
    let entries: Vec<_> = filtered.into_iter().take(effective_limit).collect();
    let returned = entries.len();

    serde_json::json!({
        "entries":       entries,
        "returned":      returned,
        "total":         total,
        "limit":         effective_limit,
    })
}

/// Build the `data` payload for a stdio `report` command.
///
/// - `id = None` or `id = Some("latest")` → loads the most recent report.
/// - `id = Some(prefix)` → loads by exact full ID or unambiguous prefix; ambiguous prefixes return Err.
/// - `compare = Some([baseline, head])` → runs compare_reports and returns comparison data.
///
/// Returns a structured error on not-found or malformed requests.
fn build_stdio_report_payload(
    target: &str,
    id: Option<&str>,
    compare: Option<&[String]>,
) -> crate::error::Result<serde_json::Value> {
    let base = std::path::Path::new(target);

    if let Some(refs) = compare {
        if refs.len() != 2 {
            return Err(crate::error::BarzelError::Detection(
                format!("compare requires exactly 2 report refs, got {}", refs.len())
            ));
        }
        let baseline = load_report_by_ref(base, &refs[0])?;
        let head     = load_report_by_ref(base, &refs[1])?;
        let cmp = compare::compare_reports(&baseline, &head);
        return Ok(serde_json::json!({ "comparison": cmp }));
    }

    let id_ref = id.unwrap_or("latest");
    let report = load_report_by_ref(base, id_ref)?;
    let report_json = serde_json::to_value(&report)
        .map_err(|e| crate::error::BarzelError::Detection(e.to_string()))?;
    Ok(serde_json::json!({ "report": report_json }))
}

/// Build the structured data payload an AI agent receives after `run`.
fn build_run_data(report: &BarzelReport) -> serde_json::Value {
    let passed = matches!(report.status, ReportStatus::Pass);
    let is_workspace = !report.workspace_members.is_empty();

    // For workspaces: flatten action_items from per-package member reports with package context.
    // For single projects: flatten from report.layers directly (unchanged behavior).
    let mut action_items: Vec<serde_json::Value> = if is_workspace {
        report.workspace_members.iter().flat_map(|member| {
            member.layers.iter().flat_map(move |layer| {
                layer.findings.iter().filter_map(move |f| {
                    if matches!(f.severity, Severity::Info) { return None; }
                    Some(serde_json::json!({
                        "priority":     severity_priority(&f.severity),
                        "package_path": member.package_path,
                        "layer":        layer.name,
                        "runner":       layer.runner,
                        "severity":     f.severity,
                        "code":         f.code,
                        "message":      f.message,
                        "location":     f.location,
                        "reproduce_cmd": f.reproduce_cmd,
                        "suggestion":   f.suggestion,
                    }))
                })
            })
        }).collect()
    } else {
        report.layers.iter().flat_map(|layer| {
            layer.findings.iter().filter_map(move |f| {
                if matches!(f.severity, Severity::Info) { return None; }
                Some(serde_json::json!({
                    "priority": severity_priority(&f.severity),
                    "layer":    layer.name,
                    "runner":   layer.runner,
                    "severity": f.severity,
                    "code":     f.code,
                    "message":  f.message,
                    "location": f.location,
                    "reproduce_cmd": f.reproduce_cmd,
                    "suggestion":    f.suggestion,
                }))
            })
        }).collect()
    };
    action_items.sort_by_key(|v| v["priority"].as_u64().unwrap_or(99));

    // Per-layer summary for quick agent parsing
    let layers_json = |layers: &[crate::report::LayerResult]| -> Vec<serde_json::Value> {
        layers.iter().map(|l| {
            let findings: Vec<serde_json::Value> = l.findings.iter().map(|f| serde_json::json!({
                "severity":     f.severity,
                "code":         f.code,
                "message":      f.message,
                "location":     f.location,
                "reproduce_cmd": f.reproduce_cmd,
                "suggestion":   f.suggestion,
            })).collect();
            serde_json::json!({
                "name":     l.name,
                "runner":   l.runner,
                "status":   l.status,
                "tests_run": l.metrics.tests_run,
                "passed":    l.metrics.passed,
                "failed":    l.metrics.failed,
                "mutation_score": l.metrics.mutation_score,
                "duration_ms": l.duration_ms,
                "findings": findings,
            })
        }).collect()
    };

    // For workspaces, expose per-package structure; for single projects, flat layers list
    let workspace_json: Vec<serde_json::Value> = report.workspace_members.iter().map(|m| {
        serde_json::json!({
            "package_path":    m.package_path,
            "language":        m.language,
            "status":          m.status,
            "summary": {
                "total_findings": m.summary.total_findings,
                "critical":       m.summary.critical,
                "high":           m.summary.high,
                "medium":         m.summary.medium,
                "low":            m.summary.low,
                "overall_status": m.summary.overall_status,
            },
            "layers": layers_json(&m.layers),
        })
    }).collect();

    // Always include top-level layers (aggregate) — agents always expect data.layers
    let mut payload = serde_json::json!({
        "passed":          passed,
        "overall_status":  report.status,
        "total_findings":  report.summary.total_findings,
        "critical":        report.summary.critical,
        "high":            report.summary.high,
        "medium":          report.summary.medium,
        "low":             report.summary.low,
        "layers":          layers_json(&report.layers),
        "action_items":    action_items,
        "report_id":       report.id,
        "timestamp":       report.timestamp,
    });

    if is_workspace {
        payload["is_workspace"] = serde_json::Value::Bool(true);
        payload["packages"] = serde_json::Value::Array(workspace_json);
    }

    // Expose diff mode metadata so agents know whether the run was scoped
    if report.diff_since.is_some() {
        let active = report.diff_changed_files.is_some();
        payload["diff_mode"] = serde_json::json!({
            "active":        active,
            "since":         report.diff_since,
            "changed_files": report.diff_changed_files,
            "fallback_reason": report.diff_fallback_reason,
        });
    }

    payload
}

fn severity_priority(s: &Severity) -> u8 {
    match s {
        Severity::Critical => 1,
        Severity::High => 2,
        Severity::Medium => 3,
        Severity::Low => 4,
        Severity::Info => 5,
    }
}

fn report_exit_code(report: &BarzelReport) -> ExitCode {
    // Check if any finding meets or exceeds the fail_on threshold
    let fail = match report.fail_on.as_str() {
        "critical" => report.summary.critical > 0,
        "medium"   => report.summary.critical > 0 || report.summary.high > 0 || report.summary.medium > 0,
        "any" | "low" => report.summary.total_findings > 0,
        _ => report.summary.critical > 0 || report.summary.high > 0, // "high" (default)
    };

    if fail {
        ExitCode::from(1)
    } else if !matches!(report.status, ReportStatus::Pass) {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn emit_error(request_id: Option<String>, message: String) {
    let resp = create_response("error", request_id, None, Some(message));
    println!("{}", serde_json::to_string(&resp).unwrap());
}

// ── Report command ────────────────────────────────────────────────────────────

fn cmd_report(id: Option<String>) -> error::Result<()> {
    let base = Path::new(".");

    let report = match &id {
        Some(id_str) if id_str != "latest" => {
            report::BarzelReport::load_by_id(base, id_str)?
        }
        _ => report::BarzelReport::load_latest(base)?,
    };

    let r = match report {
        Some(r) => r,
        None => {
            println!(
                "{} No reports found in .barzel/reports/",
                "→".bright_blue()
            );
            println!("Run `barzel run` first to generate a report.");
            return Ok(());
        }
    };

    let overall = match r.status {
        ReportStatus::Pass => "PASS".bright_green().to_string(),
        ReportStatus::Partial => "PARTIAL".yellow().to_string(),
        ReportStatus::Fail => "FAIL".bright_red().to_string(),
    };

    println!(
        "{} Report {} — {} — {}",
        "→".bright_blue(),
        (&r.id[..8]).dimmed(),
        r.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
        overall
    );
    println!(
        "   Project: {} ({})",
        r.project.package_name.as_deref().unwrap_or("unknown"),
        r.project.language
    );
    println!();

    for layer in &r.layers {
        let status_str = match layer.status {
            LayerStatus::Pass => "PASS   ".bright_green().to_string(),
            LayerStatus::Fail => "FAIL   ".bright_red().to_string(),
            LayerStatus::Partial => "PARTIAL".yellow().to_string(),
            LayerStatus::Skipped => "SKIPPED".dimmed().to_string(),
        };

        println!(
            "  {:<12} [{:<14}]  {}",
            layer.name.bright_white(),
            layer.runner.dimmed(),
            status_str
        );

        for finding in &layer.findings {
            let sev = match finding.severity {
                Severity::Critical => "[CRITICAL]".bright_red().to_string(),
                Severity::High => "[HIGH]    ".bright_yellow().to_string(),
                Severity::Medium => "[MEDIUM]  ".yellow().to_string(),
                Severity::Low => "[LOW]     ".bright_blue().to_string(),
                Severity::Info => "[INFO]    ".dimmed().to_string(),
            };
            println!("    {} {}", sev, finding.message);
            if let Some(cmd) = &finding.reproduce_cmd {
                println!("      run: {}", cmd.dimmed());
            }
        }
    }

    println!();
    println!(
        "   {} finding(s): {} critical, {} high, {} medium, {} low",
        r.summary.total_findings,
        r.summary.critical,
        r.summary.high,
        r.summary.medium,
        r.summary.low
    );

    Ok(())
}

// ── Compare command ───────────────────────────────────────────────────────────

fn load_report_by_ref(base: &Path, id_ref: &str) -> error::Result<BarzelReport> {
    let report = if id_ref == "latest" {
        BarzelReport::load_latest(base)?
    } else {
        BarzelReport::load_by_id(base, id_ref)?
    };
    report.ok_or_else(|| crate::error::BarzelError::Detection(format!("report not found: {}", id_ref)))
}

fn cmd_compare(baseline_ref: &str, head_ref: &str, json_out: bool) -> error::Result<()> {
    let base = Path::new(".");
    let baseline = load_report_by_ref(base, baseline_ref)?;
    let head = load_report_by_ref(base, head_ref)?;

    let cmp = compare::compare_reports(&baseline, &head);

    if json_out {
        println!("{}", serde_json::to_string_pretty(&cmp).unwrap_or_default());
        return Ok(());
    }

    // Human output
    let verdict_str = match cmp.verdict {
        compare::Verdict::Regressed => "REGRESSED".bright_red().to_string(),
        compare::Verdict::Improved  => "IMPROVED".bright_green().to_string(),
        compare::Verdict::Unchanged => "UNCHANGED".dimmed().to_string(),
    };

    println!(
        "{} Compare {} → {}  {}",
        "→".bright_blue(),
        (&cmp.baseline_id[..8.min(cmp.baseline_id.len())]).dimmed(),
        (&cmp.head_id[..8.min(cmp.head_id.len())]).dimmed(),
        verdict_str
    );
    println!(
        "   baseline: {}  head: {}",
        cmp.baseline_timestamp.format("%Y-%m-%d %H:%M UTC"),
        cmp.head_timestamp.format("%Y-%m-%d %H:%M UTC")
    );

    let d = &cmp.summary_delta;
    println!(
        "   findings: {:+} total  ({:+} critical  {:+} high  {:+} medium  {:+} low)",
        d.total, d.critical, d.high, d.medium, d.low
    );
    println!();

    if !cmp.regressions.is_empty() {
        println!("  {} Regressions ({})", "✗".bright_red(), cmp.regressions.len());
        for r in &cmp.regressions {
            match r {
                compare::Regression::StatusWorsened { layer, runner, from, to } =>
                    println!("    {} [{}/{}] {:?} → {:?}", "↓".bright_red(), layer, runner, from, to),
                compare::Regression::NewFinding { layer, runner, code, severity, message, location } => {
                    let sev = format!("[{:?}]", severity).bright_red().to_string();
                    println!("    {} {} [{}/{}] {} — {}", "↓".bright_red(), sev, layer, runner, code, message);
                    if let Some(loc) = location {
                        println!("        at {}", loc.dimmed());
                    }
                }
                compare::Regression::CoverageDrop { layer, runner, from, to, .. } =>
                    println!("    {} [coverage/{}/{}] {:.1}% → {:.1}%", "↓".bright_red(), layer, runner, from, to),
                compare::Regression::MutationScoreDrop { layer, runner, from, to, .. } =>
                    println!("    {} [mutation/{}/{}] {:.1}% → {:.1}%", "↓".bright_red(), layer, runner, from, to),
            }
        }
        println!();
    }

    if !cmp.improvements.is_empty() {
        println!("  {} Improvements ({})", "✓".bright_green(), cmp.improvements.len());
        for i in &cmp.improvements {
            match i {
                compare::Improvement::StatusImproved { layer, runner, from, to } =>
                    println!("    {} [{}/{}] {:?} → {:?}", "↑".bright_green(), layer, runner, from, to),
                compare::Improvement::FindingResolved { layer, runner, code, severity, .. } =>
                    println!("    {} [{:?}] [{}/{}] {} resolved", "↑".bright_green(), severity, layer, runner, code),
                compare::Improvement::CoverageImproved { layer, runner, from, to, .. } =>
                    println!("    {} [coverage/{}/{}] {:.1}% → {:.1}%", "↑".bright_green(), layer, runner, from, to),
                compare::Improvement::MutationScoreImproved { layer, runner, from, to, .. } =>
                    println!("    {} [mutation/{}/{}] {:.1}% → {:.1}%", "↑".bright_green(), layer, runner, from, to),
            }
        }
        println!();
    }

    Ok(())
}

// ── Check command ─────────────────────────────────────────────────────────────

/// Build the JSON payload for a `check` command. Extracted for testability.
fn build_check_payload(
    workspace: detect::WorkspaceInfo,
    cfg: &config::BarzelConfig,
    proc: &dyn process::SubprocessRunner,
) -> serde_json::Value {
    use detect::WorkspaceInfo;
    match workspace {
        WorkspaceInfo::Single(project) => {
            let statuses = tool_registry::probe_all_with_context(proc, &project, cfg);
            let tool_json = tool_registry::tool_statuses_to_json(&statuses);
            let missing_required = statuses.iter().filter(|s| s.required && !s.available).count();
            serde_json::json!({
                "language": project.language.to_string(),
                "frameworks": {
                    "is_nextjs": project.frameworks.is_nextjs,
                    "has_ai_deps": project.frameworks.has_ai_deps,
                    "ai_frameworks": project.frameworks.ai_frameworks,
                },
                "missing_required_tools": missing_required,
                "tools": tool_json,
            })
        }
        WorkspaceInfo::Multi { kind, members } => {
            let statuses = tool_registry::probe_all_with_workspace_context(proc, &members, cfg);
            let tool_json = tool_registry::tool_statuses_to_json(&statuses);
            let missing_required = statuses.iter().filter(|s| s.required && !s.available).count();
            let packages: Vec<_> = members.iter().map(|(rel_path, p)| serde_json::json!({
                "path":     rel_path,
                "name":     p.package_name,
                "language": p.language.to_string(),
            })).collect();
            serde_json::json!({
                "is_workspace":    true,
                "workspace_kind":  kind,
                "packages":        packages,
                "missing_required_tools": missing_required,
                "tools":           tool_json,
            })
        }
    }
}

fn cmd_check(path: Option<&std::path::Path>) -> error::Result<()> {
    use crate::detect::{detect_workspace, WorkspaceInfo};
    use crate::process::OsProcessRunner;

    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    let cfg = config::BarzelConfig::load_for_project(target);
    cfg.validate()?;

    let (statuses, header, frameworks) = match detect_workspace(target)? {
        WorkspaceInfo::Single(project) => {
            let statuses = tool_registry::probe_all_with_context(&OsProcessRunner, &project, &cfg);
            let header = format!("Barzel tool check — {} project", project.language.to_string().bright_green());
            let frameworks = Some(project.frameworks);
            (statuses, header, frameworks)
        }
        WorkspaceInfo::Multi { kind, members } => {
            const MEMBER_DISPLAY_LIMIT: usize = 7;
            let kind_str = format!("{:?}", kind).to_lowercase();
            let member_list = if members.len() <= MEMBER_DISPLAY_LIMIT {
                members.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ")
            } else {
                let shown = members[..MEMBER_DISPLAY_LIMIT]
                    .iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ");
                format!("{}, +{} more", shown, members.len() - MEMBER_DISPLAY_LIMIT)
            };
            let header = format!(
                "Barzel workspace check — {} ({} members: {})",
                kind_str.bright_green(),
                members.len(),
                member_list
            );
            let statuses = tool_registry::probe_all_with_workspace_context(&OsProcessRunner, &members, &cfg);
            (statuses, header, None)
        }
    };

    println!("{} {}", "→".bright_blue(), header);
    println!();

    print_check_sections(&statuses, frameworks.as_ref());

    Ok(())
}

fn print_check_sections(
    statuses: &[tool_registry::ToolStatus],
    frameworks: Option<&crate::detect::ProjectFrameworks>,
) {
    let required: Vec<_> = statuses.iter().filter(|s| s.required).collect();
    let applicable_disabled: Vec<_> = statuses.iter().filter(|s| s.applicable && !s.required).collect();
    let not_applicable: Vec<_> = statuses.iter().filter(|s| !s.applicable).collect();

    let section_label = if frameworks.is_some() { "this project" } else { "this workspace" };

    // Section 1: required tools
    if !required.is_empty() {
        println!("  {} Required for {}:", "→".bright_blue(), section_label);
        let missing_required = required.iter().filter(|s| !s.available).count();
        for s in &required {
            let icon = if s.available { "✓".bright_green().to_string() } else { "✗".bright_red().to_string() };
            println!(
                "    {} {:<20} [{:<12}]{}",
                icon, s.name, s.layer,
                if s.available { String::new() } else { format!("  install: {}", s.install.dimmed()) }
            );
        }
        println!();
        if missing_required == 0 {
            println!("  {} All required tools available — run `barzel run` to start verification.", "✓".bright_green());
        } else {
            println!(
                "  {} {} required tool(s) missing. Install them to enable the corresponding layers.",
                "!".yellow(), missing_required
            );
        }
    }

    // Section 2: applicable but layer disabled
    if !applicable_disabled.is_empty() {
        println!();
        println!("  {} Applicable but layer disabled in config:", "→".dimmed());
        for s in &applicable_disabled {
            let mark = if s.available { "✓" } else { "–" };
            println!("    {} {:<20} [{:<12}]  (layer disabled)", mark.dimmed(), s.name.dimmed(), s.layer.dimmed());
        }
    }

    // Section 3: not applicable
    if !not_applicable.is_empty() {
        println!();
        println!("  {} Other ecosystem tools (not applicable to this project):", "→".dimmed());
        for s in &not_applicable {
            let mark = if s.available { "✓" } else { "–" };
            println!("    {} {:<20} [{:<12}]", mark.dimmed(), s.name.dimmed(), s.layer.dimmed());
        }
    }

    // Framework notes (single-project only)
    if let Some(fw) = frameworks {
        if fw.has_ai_deps {
            println!();
            println!(
                "  {} AI frameworks detected: {}",
                "→".bright_blue(),
                fw.ai_frameworks.join(", ").bright_yellow()
            );
            println!("    aisec runner will activate automatically.");
        }
        if fw.is_nextjs {
            println!();
            println!("  {} Next.js project detected — playwright E2E runner available.", "→".bright_blue());
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--stdio") {
        return handle_stdio();
    }

    let cli = Cli::parse();

    let command = match cli.command {
        Some(c) => c,
        None => {
            eprintln!(
                "{} No subcommand provided. Use --help for usage.",
                "Error:".bright_red()
            );
            return ExitCode::from(2);
        }
    };

    match command {
        Commands::Init { path } => match init::run_init(path.as_deref(), false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red(), e);
                ExitCode::from(1)
            }
        },

        Commands::Run { path, layer, no_cache, fail_fast, json, since } => {
            match run::run_verification(path.as_deref(), layer, no_cache, fail_fast, false, json, since.as_deref()) {
                Ok(report) => report_exit_code(&report),
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red(), e);
                    ExitCode::from(1)
                }
            }
        }

        Commands::Report { id, compare, json } => {
            let result = if let Some(ids) = compare {
                cmd_compare(&ids[0], &ids[1], json)
            } else {
                cmd_report(id)
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red(), e);
                    ExitCode::from(1)
                }
            }
        }

        Commands::Check { path } => match cmd_check(path.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red(), e);
                ExitCode::from(1)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::report::{
        BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, ReportStatus, Severity,
        Summary, WorkspaceMemberReport,
    };

    fn make_finding(severity: Severity, code: &str) -> Finding {
        Finding {
            severity,
            code: code.to_string(),
            message: format!("{} issue", code),
            location: Some("src/app.py:10".to_string()),
            reproduce_cmd: Some(format!("grep -n {} src/app.py", code)),
            suggestion: None,
        }
    }

    fn make_layer(name: &str, runner: &str, findings: Vec<Finding>) -> LayerResult {
        let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical)) {
            LayerStatus::Fail
        } else {
            LayerStatus::Pass
        };
        LayerResult {
            name: name.to_string(),
            runner: runner.to_string(),
            status,
            findings,
            metrics: LayerMetrics::default(),
            duration_ms: 0,
        }
    }

    fn workspace_report() -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Unknown,
            root: "/tmp/ws".to_string(),
            has_tests: true,
            package_name: Some("cargo-workspace".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.status = ReportStatus::Fail;

        // pkg-a has a Critical finding
        let pkg_a_layer = make_layer("hostile", "ai-sec", vec![
            make_finding(Severity::Critical, "HARDCODED_API_KEY"),
        ]);
        let pkg_b_layer = make_layer("logic", "pytest", vec![
            make_finding(Severity::High, "TEST_FAILURE"),
        ]);

        report.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "crates/a".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Fail,
                layers: vec![pkg_a_layer.clone()],
                summary: Summary {
                    total_findings: 1, critical: 1, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Fail,
                },
            },
            WorkspaceMemberReport {
                package_path: "crates/b".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Partial,
                layers: vec![pkg_b_layer.clone()],
                summary: Summary {
                    total_findings: 1, critical: 0, high: 1, medium: 0, low: 0,
                    overall_status: ReportStatus::Partial,
                },
            },
        ];

        // Aggregate layers
        report.layers = vec![pkg_a_layer, pkg_b_layer];

        report
    }

    #[test]
    fn workspace_action_items_include_package_path() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let items = payload["action_items"].as_array().unwrap();
        assert!(!items.is_empty(), "action_items must not be empty for workspace with findings");
        for item in items {
            assert!(item.get("package_path").is_some(), "each action_item must have package_path");
            assert!(!item["package_path"].as_str().unwrap_or("").is_empty());
        }
    }

    #[test]
    fn workspace_action_items_are_priority_sorted() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let items = payload["action_items"].as_array().unwrap();
        let priorities: Vec<u64> = items.iter()
            .map(|v| v["priority"].as_u64().unwrap_or(99))
            .collect();
        let mut sorted = priorities.clone();
        sorted.sort();
        assert_eq!(priorities, sorted, "action_items must be sorted by priority (critical first)");
    }

    #[test]
    fn workspace_payload_has_top_level_layers() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let layers = payload["layers"].as_array().unwrap();
        assert!(!layers.is_empty(), "data.layers must be present even for workspaces");
    }

    #[test]
    fn workspace_payload_has_is_workspace_flag() {
        let report = workspace_report();
        let payload = build_run_data(&report);
        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert!(payload.get("packages").is_some());
    }

    #[test]
    fn single_project_action_items_have_no_package_path() {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/single".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("hostile", "bandit", vec![
            make_finding(Severity::High, "SQL_INJECTION"),
        ])];
        report.status = ReportStatus::Partial;

        let payload = build_run_data(&report);
        assert_eq!(payload.get("is_workspace"), None, "single project must not have is_workspace");
        let items = payload["action_items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            assert!(item.get("package_path").is_none(), "single project action_items must not have package_path");
        }
    }

    fn base_report() -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/proj".to_string(),
            has_tests: true,
            package_name: Some("proj".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        BarzelReport::new(project)
    }

    #[test]
    fn diff_mode_active_when_since_and_changed_files_set() {
        let mut report = base_report();
        report.diff_since = Some("HEAD~1".to_string());
        report.diff_changed_files = Some(2);

        let payload = build_run_data(&report);
        let dm = &payload["diff_mode"];
        assert_eq!(dm["active"].as_bool(), Some(true));
        assert_eq!(dm["since"].as_str(), Some("HEAD~1"));
        assert_eq!(dm["changed_files"].as_u64(), Some(2));
        assert!(dm["fallback_reason"].is_null());
    }

    #[test]
    fn diff_mode_inactive_when_fallback() {
        let mut report = base_report();
        report.diff_since = Some("bad-rev".to_string());
        report.diff_changed_files = None;
        report.diff_fallback_reason = Some("not a git repository or invalid revision".to_string());

        let payload = build_run_data(&report);
        let dm = &payload["diff_mode"];
        assert_eq!(dm["active"].as_bool(), Some(false));
        assert_eq!(dm["since"].as_str(), Some("bad-rev"));
        assert!(dm["changed_files"].is_null());
        assert_eq!(
            dm["fallback_reason"].as_str(),
            Some("not a git repository or invalid revision")
        );
    }

    #[test]
    fn diff_mode_absent_when_since_not_set() {
        let report = base_report();
        let payload = build_run_data(&report);
        assert!(payload.get("diff_mode").is_none(), "diff_mode must be absent when --since not used");
    }

    // ── tool registry / check payload ─────────────────────────────────────────

    #[test]
    fn stdio_check_tool_json_includes_install_guidance() {
        use crate::process::MockProcessRunner;
        // All tools spawn-fail — every JSON entry must still carry install guidance.
        let statuses = tool_registry::probe_all(&MockProcessRunner::unavailable());
        let tool_json = tool_registry::tool_statuses_to_json(&statuses);

        for entry in &tool_json {
            let install = entry["install"].as_str().unwrap_or("");
            assert!(!install.is_empty(),
                "tool '{}' must have install guidance in stdio payload",
                entry["name"].as_str().unwrap_or("?"));
        }
    }

    #[test]
    fn tool_statuses_to_json_produces_correct_shape() {
        // Verify the shared helper produces all four required fields.
        use crate::tool_registry::{tool_statuses_to_json, ToolStatus};
        let statuses = vec![
            ToolStatus { name: "cargo", layer: "core", available: true, install: "https://rustup.rs", applicable: true, required: true, reason: "Rust project".to_string() },
            ToolStatus { name: "semgrep", layer: "hostile", available: false, install: "pip install semgrep", applicable: true, required: true, reason: "all projects".to_string() },
        ];
        let json = tool_statuses_to_json(&statuses);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["name"].as_str(), Some("cargo"));
        assert_eq!(json[0]["layer"].as_str(), Some("core"));
        assert_eq!(json[0]["available"].as_bool(), Some(true));
        assert_eq!(json[0]["install"].as_str(), Some("https://rustup.rs"));
        assert_eq!(json[0]["applicable"].as_bool(), Some(true));
        assert_eq!(json[0]["required"].as_bool(), Some(true));
        assert!(json[0].get("reason").is_some(), "reason field must be present");
        assert_eq!(json[1]["available"].as_bool(), Some(false));
    }

    #[test]
    fn stdio_check_tool_json_contains_expected_tools() {
        use crate::process::MockProcessRunner;
        let statuses = tool_registry::probe_all(&MockProcessRunner::passing("ok"));
        let tool_json = tool_registry::tool_statuses_to_json(&statuses);
        let names: Vec<&str> = tool_json.iter()
            .filter_map(|v| v["name"].as_str())
            .collect();
        assert!(names.contains(&"go-mutesting"), "go-mutesting must be in stdio payload");
        assert!(names.contains(&"cargo audit"),  "cargo audit must be in stdio payload");
        assert!(names.contains(&"pip-audit"),    "pip-audit must be in stdio payload");
        assert!(names.contains(&"semgrep"),      "semgrep must be in stdio payload");
        assert!(names.contains(&"npm"),  "npm must be in stdio payload (backs npm audit)");
        assert!(names.contains(&"pnpm"), "pnpm must be in stdio payload (backs pnpm audit)");
        assert!(names.contains(&"yarn"), "yarn must be in stdio payload (backs yarn audit)");
    }

    // ── workspace check payload contract ──────────────────────────────────────

    fn make_project(language: Language, root: &str) -> ProjectInfo {
        ProjectInfo {
            language,
            root: root.to_string(),
            has_tests: true,
            package_name: Some(root.split('/').last().unwrap_or("pkg").to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    #[test]
    fn check_payload_single_project_preserves_pr24_shape() {
        use crate::detect::WorkspaceInfo;
        use crate::process::MockProcessRunner;
        let dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Single(make_project(Language::Rust, dir.path().to_str().unwrap()));
        let cfg = config::BarzelConfig::default();
        let payload = build_check_payload(ws, &cfg, &MockProcessRunner::passing("ok"));

        assert_eq!(payload["language"].as_str(), Some("rust"));
        assert!(payload.get("frameworks").is_some(), "frameworks key required");
        assert!(payload["missing_required_tools"].is_number(), "missing_required_tools must be a number");
        assert!(payload["tools"].is_array(), "tools must be an array");
        assert!(payload.get("is_workspace").is_none(), "Single must not emit is_workspace");
        assert!(payload.get("packages").is_none(), "Single must not emit packages");

        // All tool entries must have the required shape fields
        for tool in payload["tools"].as_array().unwrap() {
            for key in &["name", "layer", "available", "install", "applicable", "required", "reason"] {
                assert!(tool.get(key).is_some(), "tool entry missing field '{key}'");
            }
        }
    }

    #[test]
    fn check_payload_multi_workspace_contract() {
        use crate::detect::{WorkspaceInfo, WorkspaceKind};
        use crate::process::MockProcessRunner;
        let rust_dir = tempfile::tempdir().unwrap();
        let ts_dir   = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Multi {
            kind: WorkspaceKind::Cargo,
            members: vec![
                ("crates/api".to_string(), make_project(Language::Rust, rust_dir.path().to_str().unwrap())),
                ("apps/web".to_string(),   make_project(Language::TypeScript, ts_dir.path().to_str().unwrap())),
            ],
        };
        let cfg = config::BarzelConfig::default();
        let payload = build_check_payload(ws, &cfg, &MockProcessRunner::passing("ok"));

        // Workspace envelope
        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert_eq!(payload["workspace_kind"].as_str(), Some("cargo"));

        // packages list
        let pkgs = payload["packages"].as_array().expect("packages must be array");
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0]["path"].as_str(), Some("crates/api"));
        assert_eq!(pkgs[0]["language"].as_str(), Some("rust"));
        assert_eq!(pkgs[1]["path"].as_str(), Some("apps/web"));
        assert_eq!(pkgs[1]["language"].as_str(), Some("typescript"));

        // top-level aggregated count
        assert!(payload["missing_required_tools"].is_number());

        // tools list is aggregated and has correct shape
        let tools = payload["tools"].as_array().expect("tools must be array");
        assert!(!tools.is_empty());
        for tool in tools {
            for key in &["name", "layer", "available", "install", "applicable", "required", "reason"] {
                assert!(tool.get(key).is_some(), "tool entry missing field '{key}'");
            }
        }

        // Rust tools applicable, TypeScript tools applicable
        let cargo = tools.iter().find(|t| t["name"] == "cargo").expect("cargo must be present");
        assert_eq!(cargo["applicable"].as_bool(), Some(true), "cargo applicable for rust member");
        assert_eq!(cargo["required"].as_bool(), Some(true), "cargo required for rust member");

        let node = tools.iter().find(|t| t["name"] == "node").expect("node must be present");
        assert_eq!(node["applicable"].as_bool(), Some(true), "node applicable for ts member");

        // reason must name contributing members
        let cargo_reason = cargo["reason"].as_str().unwrap_or("");
        assert!(cargo_reason.contains("crates/api"), "cargo reason must name rust member");

        // Single contract fields must be absent
        assert!(payload.get("language").is_none(), "Multi must not emit top-level language");
        assert!(payload.get("frameworks").is_none(), "Multi must not emit top-level frameworks");
    }

    // ── run-data invariant helpers ────────────────────────────────────────────

    /// Required keys every action_item must carry, checked for the agent-facing contract.
    const ACTION_ITEM_REQUIRED_KEYS: &[&str] = &[
        "priority", "layer", "runner", "severity", "code", "message", "reproduce_cmd",
    ];

    /// Assert all action_items satisfy the agent-facing contract:
    /// - every required key is present
    /// - reproduce_cmd is non-null (agents must be able to run it)
    /// - items are sorted by priority (critical=1 first)
    fn assert_action_items_contract(payload: &serde_json::Value) {
        let items = payload["action_items"].as_array()
            .expect("action_items must be an array");

        for (i, item) in items.iter().enumerate() {
            for key in ACTION_ITEM_REQUIRED_KEYS {
                assert!(
                    item.get(*key).is_some(),
                    "action_item[{i}] missing required key '{key}'"
                );
            }
            let reproduce = item["reproduce_cmd"].as_str().unwrap_or("");
            assert!(
                !reproduce.trim().is_empty(),
                "action_item[{i}] has empty or null reproduce_cmd (code={:?})",
                item["code"].as_str().unwrap_or("?")
            );
        }

        let priorities: Vec<u64> = items.iter()
            .map(|v| v["priority"].as_u64().unwrap_or(99))
            .collect();
        let mut sorted = priorities.clone();
        sorted.sort();
        assert_eq!(priorities, sorted, "action_items must be sorted by priority (critical first)");
    }

    /// Assert the top-level keys all run payloads must carry.
    fn assert_run_payload_required_keys(payload: &serde_json::Value) {
        for key in &["passed", "overall_status", "action_items", "layers",
                     "total_findings", "critical", "high", "medium", "low",
                     "report_id", "timestamp"] {
            assert!(payload.get(*key).is_some(), "run payload missing required key '{key}'");
        }
    }

    // ── reproduce_cmd invariant ────────────────────────────────────────────────

    #[test]
    fn single_project_action_items_all_satisfy_contract() {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/single".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("hostile", "bandit", vec![
            make_finding(Severity::Critical, "SQL_INJECTION"),
            make_finding(Severity::High,     "OPEN_REDIRECT"),
            make_finding(Severity::Medium,   "WEAK_CIPHER"),
        ])];
        report.status = ReportStatus::Fail;

        let payload = build_run_data(&report);
        assert_action_items_contract(&payload);
        assert_eq!(payload["action_items"].as_array().unwrap().len(), 3,
            "all three non-info findings must appear as action_items");
    }

    #[test]
    fn workspace_action_items_all_satisfy_contract() {
        let payload = build_run_data(&workspace_report());
        assert_action_items_contract(&payload);
    }

    #[test]
    fn info_findings_are_excluded_from_action_items() {
        let project = ProjectInfo {
            language: Language::Rust,
            root: "/tmp/p".to_string(),
            has_tests: true,
            package_name: Some("p".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("logic", "cargo-test", vec![
            make_finding(Severity::Info, "TESTS_PASSED"),
            make_finding(Severity::High, "MUTATION_SURVIVED"),
        ])];
        report.status = ReportStatus::Partial;

        let payload = build_run_data(&report);
        let items = payload["action_items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "only the High finding must appear; Info must be excluded");
        assert_eq!(items[0]["code"].as_str(), Some("MUTATION_SURVIVED"));
    }

    // ── run payload required-key contract ─────────────────────────────────────

    #[test]
    fn run_payload_single_has_required_keys() {
        let project = ProjectInfo {
            language: Language::Go,
            root: "/tmp/go".to_string(),
            has_tests: true,
            package_name: Some("svc".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let report = BarzelReport::new(project);
        let payload = build_run_data(&report);

        assert_run_payload_required_keys(&payload);
        assert!(payload.get("is_workspace").is_none(), "Single must not emit is_workspace");
        assert!(payload.get("packages").is_none(),     "Single must not emit packages");
    }

    #[test]
    fn run_payload_workspace_has_required_keys_plus_workspace_fields() {
        let payload = build_run_data(&workspace_report());

        assert_run_payload_required_keys(&payload);
        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert!(payload["packages"].is_array(), "workspace payload must include packages array");
    }

    // ── global cross-package priority order ────────────────────────────────────

    #[test]
    fn workspace_mixed_findings_global_priority_order() {
        let project = ProjectInfo {
            language: Language::Unknown,
            root: "/tmp/ws".to_string(),
            has_tests: true,
            package_name: Some("ws".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.status = ReportStatus::Fail;

        // pkg-a: Medium only; pkg-b: Critical only; pkg-c: High only.
        // After flattening, expected order: Critical(pkg-b), High(pkg-c), Medium(pkg-a).
        report.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "pkg-a".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Partial,
                layers: vec![make_layer("structural", "mutants", vec![
                    make_finding(Severity::Medium, "MUTANT_SURVIVED"),
                ])],
                summary: Summary { total_findings: 1, critical: 0, high: 0, medium: 1, low: 0,
                    overall_status: ReportStatus::Partial },
            },
            WorkspaceMemberReport {
                package_path: "pkg-b".to_string(),
                language: "typescript".to_string(),
                status: ReportStatus::Fail,
                layers: vec![make_layer("hostile", "semgrep", vec![
                    make_finding(Severity::Critical, "HARDCODED_SECRET"),
                ])],
                summary: Summary { total_findings: 1, critical: 1, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Fail },
            },
            WorkspaceMemberReport {
                package_path: "pkg-c".to_string(),
                language: "python".to_string(),
                status: ReportStatus::Partial,
                layers: vec![make_layer("logic", "pytest", vec![
                    make_finding(Severity::High, "TEST_FAILURE"),
                ])],
                summary: Summary { total_findings: 1, critical: 0, high: 1, medium: 0, low: 0,
                    overall_status: ReportStatus::Partial },
            },
        ];
        report.layers = report.workspace_members.iter()
            .flat_map(|m| m.layers.clone()).collect();

        let payload = build_run_data(&report);
        assert_action_items_contract(&payload);

        let items = payload["action_items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["code"].as_str(), Some("HARDCODED_SECRET"), "Critical must be first");
        assert_eq!(items[1]["code"].as_str(), Some("TEST_FAILURE"),      "High must be second");
        assert_eq!(items[2]["code"].as_str(), Some("MUTANT_SURVIVED"),   "Medium must be last");
        // Verify package_path is preserved after reordering
        assert_eq!(items[0]["package_path"].as_str(), Some("pkg-b"));
        assert_eq!(items[1]["package_path"].as_str(), Some("pkg-c"));
        assert_eq!(items[2]["package_path"].as_str(), Some("pkg-a"));
    }

    // ── stdio report command ──────────────────────────────────────────────────

    fn saved_report(dir: &tempfile::TempDir) -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.add_layer(make_layer("hostile", "semgrep", vec![
            make_finding(Severity::High, "SQL_INJECT"),
        ]));
        report.save(dir.path()).unwrap();
        report
    }

    #[test]
    fn stdio_report_latest_returns_full_report_with_id() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), None, None).unwrap();
        let report_json = &payload["report"];
        assert_eq!(report_json["id"].as_str(), Some(saved.id.as_str()),
            "data.report.id must match the saved report's id");
        assert_eq!(report_json["project"]["package_name"].as_str(), Some("myapp"));
    }

    #[test]
    fn stdio_report_by_id_prefix_returns_matching_report() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);
        let prefix = &saved.id[..8];

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), Some(prefix), None).unwrap();
        assert_eq!(payload["report"]["id"].as_str(), Some(saved.id.as_str()));
    }

    #[test]
    fn stdio_report_latest_keyword_resolves_to_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), Some("latest"), None).unwrap();
        assert_eq!(payload["report"]["id"].as_str(), Some(saved.id.as_str()));
    }

    #[test]
    fn stdio_report_not_found_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        // No reports saved — should return Err
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), None, None);
        assert!(result.is_err(), "missing report must return Err");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found") || msg.contains("latest"),
            "error message must reference what was not found: {msg}");
    }

    #[test]
    fn stdio_report_unknown_id_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let _ = saved_report(&dir);
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), Some("nonexistent"), None);
        assert!(result.is_err(), "unknown report id must return Err");
    }

    #[test]
    fn stdio_compare_returns_verdict_and_summary_delta() {
        let dir = tempfile::tempdir().unwrap();

        let project = || ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };

        // baseline: clean (no findings)
        let baseline_report = BarzelReport::new(project());
        baseline_report.save(dir.path()).unwrap();
        let baseline_id = baseline_report.id.clone();

        // Small delay so head has a strictly later timestamp
        std::thread::sleep(std::time::Duration::from_millis(5));

        // head: new High finding → regression
        let mut head_report = BarzelReport::new(project());
        head_report.add_layer(make_layer("hostile", "semgrep", vec![
            make_finding(Severity::High, "VULN_NEW"),
        ]));
        head_report.save(dir.path()).unwrap();
        let head_id = head_report.id.clone();

        let refs = vec![baseline_id[..8].to_string(), head_id[..8].to_string()];
        let payload = build_stdio_report_payload(
            dir.path().to_str().unwrap(), None, Some(&refs),
        ).unwrap();

        let cmp = &payload["comparison"];
        assert_eq!(cmp["verdict"].as_str(), Some("regressed"),
            "new High finding in head must produce verdict=regressed");
        assert_eq!(cmp["summary_delta"]["high"].as_i64(), Some(1),
            "high must increase by 1 from baseline to head");
        assert!(cmp["regressions"].as_array().map(|v| !v.is_empty()).unwrap_or(false),
            "regressions must be non-empty");
    }

    #[test]
    fn stdio_compare_wrong_length_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let refs = vec!["only-one".to_string()];
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), None, Some(&refs));
        assert!(result.is_err(), "single-element compare must return Err");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("2"), "error must mention expected count of 2: {msg}");
    }

    // ── stdio history command ─────────────────────────────────────────────────

    fn history_entry(
        package_path: Option<&str>,
        language: &str,
        hours_ago: i64,
    ) -> crate::history::HistoryEntry {
        use crate::report::{LayerStatus, ReportStatus};
        crate::history::HistoryEntry {
            report_id: format!("hist-{}-{}", language, hours_ago),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(hours_ago),
            project: "myapp".to_string(),
            package_path: package_path.map(str::to_string),
            language: language.to_string(),
            status: ReportStatus::Pass,
            layers: vec![crate::history::HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.90 - hours_ago as f64 * 0.01),
            }],
        }
    }

    fn save_history(dir: &tempfile::TempDir, entry: &crate::history::HistoryEntry) {
        crate::history::save_entry(entry, dir.path()).unwrap();
    }

    #[test]
    fn history_no_entries_returns_empty_payload() {
        let dir = tempfile::tempdir().unwrap();
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), None, None, None);
        assert_eq!(payload["returned"].as_u64(), Some(0));
        assert_eq!(payload["total"].as_u64(), Some(0));
        assert_eq!(payload["limit"].as_u64(), Some(20), "default limit must be 20");
        assert!(payload["entries"].as_array().map(|a| a.is_empty()).unwrap_or(false));
    }

    #[test]
    fn history_limit_above_cap_is_clamped_to_200() {
        let dir = tempfile::tempdir().unwrap();
        // Write 2 entries — far fewer than 200; assert limit in response is capped.
        save_history(&dir, &history_entry(None, "python", 2));
        save_history(&dir, &history_entry(None, "python", 1));

        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(999), None, None);
        assert_eq!(payload["limit"].as_u64(), Some(200), "limit must be capped at 200");
        assert_eq!(payload["returned"].as_u64(), Some(2), "returned must not exceed available entries");
        assert_eq!(payload["total"].as_u64(), Some(2));
    }

    #[test]
    fn history_entries_returned_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "python", 3));
        save_history(&dir, &history_entry(None, "python", 1));
        save_history(&dir, &history_entry(None, "python", 2));

        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), None, None, None);
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        // Newest-first: hours_ago 1, then 2, then 3
        let ids: Vec<&str> = entries.iter().map(|e| e["report_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["hist-python-1", "hist-python-2", "hist-python-3"],
            "entries must be newest-first");
    }

    #[test]
    fn history_limit_truncates_returned_entries() {
        let dir = tempfile::tempdir().unwrap();
        for h in 1..=5 {
            save_history(&dir, &history_entry(None, "python", h));
        }
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(2), None, None);
        assert_eq!(payload["returned"].as_u64(), Some(2), "limit must cap returned entries");
        assert_eq!(payload["total"].as_u64(), Some(5), "total must reflect all filtered entries");
        assert_eq!(payload["limit"].as_u64(), Some(2));
        assert_eq!(payload["entries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn history_limit_zero_returns_empty_entries() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "python", 1));
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(0), None, None);
        assert_eq!(payload["returned"].as_u64(), Some(0));
        assert_eq!(payload["total"].as_u64(), Some(1), "total must still reflect filtered count");
        assert!(payload["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn history_package_path_filter() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(Some("crates/api"), "rust", 2));
        save_history(&dir, &history_entry(Some("apps/web"), "typescript", 1));

        let payload = build_stdio_history_payload(
            dir.path().to_str().unwrap(), None, Some("crates/api"), None,
        );
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "only crates/api must be returned");
        assert_eq!(
            entries[0]["package_path"].as_str(),
            Some("crates/api")
        );
        assert_eq!(payload["total"].as_u64(), Some(1));
    }

    #[test]
    fn history_language_filter() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "rust", 2));
        save_history(&dir, &history_entry(None, "python", 1));

        let payload = build_stdio_history_payload(
            dir.path().to_str().unwrap(), None, None, Some("rust"),
        );
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "only rust entries must be returned");
        assert_eq!(entries[0]["language"].as_str(), Some("rust"));
    }
}
