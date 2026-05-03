mod cache;
mod cli;
mod compare;
mod config;
mod detect;
mod diff;
mod error;
mod http;
mod init;
mod orchestrator;
mod plugin;
mod process;
mod report;
mod run;
mod runners;

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
            match detect::detect_project(path.unwrap_or_else(|| std::path::Path::new("."))) {
                Ok(project) => {
                    use crate::process::{OsProcessRunner, SubprocessRunner};
                    let proc = OsProcessRunner;
                    let tools = [
                        ("cargo", vec!["--version"], "core"),
                        ("cargo mutants", vec!["mutants", "--version"], "structural"),
                        ("semgrep", vec!["--version"], "hostile"),
                        ("pytest", vec!["--version"], "logic"),
                        ("mypy", vec!["--version"], "logic"),
                        ("mutmut", vec!["--version"], "structural"),
                        ("bandit", vec!["--version"], "hostile"),
                        ("node", vec!["--version"], "core"),
                        ("go", vec!["version"], "core"),
                    ];
                    let tool_status: Vec<serde_json::Value> = tools.iter().map(|(name, args, layer)| {
                        let first = name.split_whitespace().next().unwrap_or(name);
                        let available = proc.is_available(first, args);
                        serde_json::json!({ "name": name, "layer": layer, "available": available })
                    }).collect();

                    let resp = create_response("success", request_id, Some(serde_json::json!({
                        "language": project.language.to_string(),
                        "frameworks": {
                            "is_nextjs": project.frameworks.is_nextjs,
                            "has_ai_deps": project.frameworks.has_ai_deps,
                            "ai_frameworks": project.frameworks.ai_frameworks,
                        },
                        "tools": tool_status,
                    })), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        other => {
            emit_error(request_id, format!("unknown command: '{}'. Valid commands: init, run, check", other));
            ExitCode::from(1)
        }
    }
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

fn cmd_check(path: Option<&std::path::Path>) -> error::Result<()> {
    use crate::detect::detect_project;
    use crate::process::{OsProcessRunner, SubprocessRunner};

    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    let project = detect_project(target)?;
    let proc = OsProcessRunner;

    println!(
        "{} Barzel tool check — {} project",
        "→".bright_blue(),
        project.language.to_string().bright_green()
    );
    println!();

    struct Tool {
        name: &'static str,
        check_args: &'static [&'static str],
        layer: &'static str,
        install: &'static str,
    }

    let tools: &[Tool] = &[
        // Rust toolchain
        Tool { name: "cargo", check_args: &["--version"], layer: "core", install: "https://rustup.rs" },
        Tool { name: "cargo kani", check_args: &["kani", "--version"], layer: "logic", install: "cargo install --locked kani-verifier" },
        Tool { name: "cargo mutants", check_args: &["mutants", "--version"], layer: "structural", install: "cargo install cargo-mutants" },
        Tool { name: "cargo fuzz", check_args: &["fuzz", "--version"], layer: "hostile", install: "cargo install cargo-fuzz" },
        // Python toolchain
        Tool { name: "pytest", check_args: &["--version"], layer: "logic", install: "pip install pytest" },
        Tool { name: "mypy", check_args: &["--version"], layer: "logic", install: "pip install mypy" },
        Tool { name: "mutmut", check_args: &["--version"], layer: "structural", install: "pip install mutmut" },
        Tool { name: "bandit", check_args: &["--version"], layer: "hostile", install: "pip install bandit" },
        // TypeScript/Node.js toolchain
        Tool { name: "node", check_args: &["--version"], layer: "core", install: "https://nodejs.org" },
        Tool { name: "npx", check_args: &["--version"], layer: "core", install: "https://nodejs.org" },
        // (jest/vitest/tsc/eslint are checked via node_modules/.bin — no global install needed)
        // Cross-language SAST
        Tool { name: "semgrep", check_args: &["--version"], layer: "hostile", install: "pip install semgrep  OR  brew install semgrep" },
        // Dependency vulnerability scanners — update this list when adding new audit runners
        Tool { name: "cargo audit", check_args: &["audit", "--version"], layer: "hostile", install: "cargo install cargo-audit" },
        Tool { name: "pip-audit", check_args: &["--version"], layer: "hostile", install: "pip install pip-audit" },
        // (npm-audit uses npm which is already listed above)
        // Go toolchain
        Tool { name: "go", check_args: &["version"], layer: "core", install: "https://go.dev/dl" },
    ];

    let mut missing = Vec::new();
    for tool in tools {
        let first_word = tool.name.split_whitespace().next().unwrap_or(tool.name);
        let available = proc.is_available(first_word, tool.check_args);
        let icon = if available { "✓".bright_green().to_string() } else { "✗".bright_red().to_string() };
        println!(
            "  {} {:<20} [{:<12}]{}",
            icon,
            tool.name,
            tool.layer,
            if available { String::new() } else { format!("  install: {}", tool.install.dimmed()) }
        );
        if !available {
            missing.push(tool);
        }
    }

    println!();
    if missing.is_empty() {
        println!("{} All tools available — run `barzel run` to start verification.", "✓".bright_green());
    } else {
        println!(
            "{} {} tool(s) missing. Install them to enable the corresponding layers.",
            "!".yellow(),
            missing.len()
        );
    }

    // Show detected frameworks
    if project.frameworks.has_ai_deps {
        println!();
        println!(
            "  {} AI frameworks detected: {}",
            "→".bright_blue(),
            project.frameworks.ai_frameworks.join(", ").bright_yellow()
        );
        println!("    aisec runner will activate automatically.");
    }
    if project.frameworks.is_nextjs {
        println!();
        println!("  {} Next.js project detected — playwright E2E runner available.", "→".bright_blue());
    }

    Ok(())
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
}
