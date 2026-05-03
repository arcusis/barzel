mod cache;
mod cli;
mod config;
mod detect;
mod error;
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
            match run::run_verification(path, req.layers, req.no_cache, req.fail_fast, true) {
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

        other => {
            emit_error(request_id, format!("unknown command: {}", other));
            ExitCode::from(1)
        }
    }
}

/// Build the structured data payload an AI agent receives after `run`.
fn build_run_data(report: &BarzelReport) -> serde_json::Value {
    let passed = matches!(report.status, ReportStatus::Pass);

    // Flatten all non-info findings into a prioritised action list, sorted by priority
    let mut action_items: Vec<serde_json::Value> = report
        .layers
        .iter()
        .flat_map(|layer| {
            layer.findings.iter().filter_map(move |f| {
                if matches!(f.severity, Severity::Info) {
                    return None;
                }
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
        })
        .collect();
    action_items.sort_by_key(|v| v["priority"].as_u64().unwrap_or(99));

    // Per-layer summary with full findings for agent consumption
    let layers: Vec<serde_json::Value> = report
        .layers
        .iter()
        .map(|l| {
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
        })
        .collect();

    serde_json::json!({
        "passed":          passed,
        "overall_status":  report.status,
        "total_findings":  report.summary.total_findings,
        "critical":        report.summary.critical,
        "high":            report.summary.high,
        "medium":          report.summary.medium,
        "low":             report.summary.low,
        "layers":          layers,
        "action_items":    action_items,
        "report_id":       report.id,
        "timestamp":       report.timestamp,
    })
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
        Tool { name: "cargo", check_args: &["--version"], layer: "core", install: "https://rustup.rs" },
        Tool { name: "cargo kani", check_args: &["kani", "--version"], layer: "logic", install: "cargo install --locked kani-verifier" },
        Tool { name: "cargo mutants", check_args: &["mutants", "--version"], layer: "structural", install: "cargo install cargo-mutants" },
        Tool { name: "cargo fuzz", check_args: &["fuzz", "--version"], layer: "hostile", install: "cargo install cargo-fuzz" },
        Tool { name: "semgrep", check_args: &["--version"], layer: "hostile", install: "pip install semgrep" },
        Tool { name: "pytest", check_args: &["--version"], layer: "logic", install: "pip install pytest" },
        Tool { name: "mutmut", check_args: &["--version"], layer: "structural", install: "pip install mutmut" },
        Tool { name: "bandit", check_args: &["--version"], layer: "hostile", install: "pip install bandit" },
        Tool { name: "npx", check_args: &["--version"], layer: "core", install: "Install Node.js from https://nodejs.org" },
        Tool { name: "node", check_args: &["--version"], layer: "core", install: "Install Node.js from https://nodejs.org" },
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

        Commands::Run { path, layer, no_cache, fail_fast } => {
            match run::run_verification(path.as_deref(), layer, no_cache, fail_fast, false) {
                Ok(report) => report_exit_code(&report),
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red(), e);
                    ExitCode::from(1)
                }
            }
        }

        Commands::Report { id } => match cmd_report(id) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red(), e);
                ExitCode::from(1)
            }
        },

        Commands::Check { path } => match cmd_check(path.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red(), e);
                ExitCode::from(1)
            }
        },
    }
}
