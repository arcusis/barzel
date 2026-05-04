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
mod stdio;
mod tool_registry;

use clap::Parser;
use cli::{Cli, Commands};
use owo_colors::OwoColorize;
use report::{BarzelReport, LayerStatus, ReportStatus, Severity};
use std::path::Path;
use std::process::ExitCode;

// ── Shared helpers (pub(crate) so stdio.rs can use them) ─────────────────────

pub(crate) fn report_exit_code(report: &BarzelReport) -> ExitCode {
    let fail = match report.fail_on.as_str() {
        "critical" => report.summary.critical > 0,
        "medium"   => report.summary.critical > 0 || report.summary.high > 0 || report.summary.medium > 0,
        "low"      => report.summary.critical > 0 || report.summary.high > 0 || report.summary.medium > 0 || report.summary.low > 0,
        "any"      => report.summary.total_findings > 0,
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

pub(crate) fn load_report_by_ref(base: &Path, id_ref: &str) -> error::Result<BarzelReport> {
    let report = if id_ref == "latest" {
        BarzelReport::load_latest(base)?
    } else {
        BarzelReport::load_by_id(base, id_ref)?
    };
    report.ok_or_else(|| crate::error::BarzelError::Detection(format!("report not found: {}", id_ref)))
}

/// Validate per-member `.barzel.toml` overrides in a workspace.
/// Single projects and members without a local config are skipped.
pub(crate) fn validate_workspace_member_configs(workspace: &detect::WorkspaceInfo) -> error::Result<()> {
    use detect::WorkspaceInfo;
    let members = match workspace {
        WorkspaceInfo::Single(_) => return Ok(()),
        WorkspaceInfo::Multi { members, .. } => members,
    };
    for (rel_path, member) in members {
        let config_path = std::path::Path::new(&member.root).join(".barzel.toml");
        if !config_path.exists() {
            continue;
        }
        let mcfg = config::BarzelConfig::try_load_for_project(std::path::Path::new(&member.root))
            .map_err(|e| error::BarzelError::Config(format!("{}: {}", rel_path, e)))?;
        mcfg.validate().map_err(|e| {
            error::BarzelError::Config(format!("{}: {}", rel_path, e))
        })?;
    }
    Ok(())
}

/// Build the JSON payload for a `check` command. Extracted for testability.
pub(crate) fn build_check_payload(
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

fn cmd_compare(baseline_ref: &str, head_ref: &str, json_out: bool) -> error::Result<()> {
    let base = Path::new(".");
    let baseline = load_report_by_ref(base, baseline_ref)?;
    let head = load_report_by_ref(base, head_ref)?;

    let cmp = compare::compare_reports(&baseline, &head);

    if json_out {
        println!("{}", serde_json::to_string_pretty(&cmp).unwrap_or_default());
        return Ok(());
    }

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
    use crate::detect::{detect_workspace, WorkspaceInfo};
    use crate::process::OsProcessRunner;

    let target = path.unwrap_or_else(|| std::path::Path::new("."));
    let cfg = config::BarzelConfig::try_load_for_project(target)?;
    cfg.validate()?;

    let workspace = detect_workspace(target)?;
    validate_workspace_member_configs(&workspace)?;

    let (statuses, header, frameworks) = match workspace {
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

    if !applicable_disabled.is_empty() {
        println!();
        println!("  {} Applicable but layer disabled in config:", "→".dimmed());
        for s in &applicable_disabled {
            let mark = if s.available { "✓" } else { "–" };
            println!("    {} {:<20} [{:<12}]  (layer disabled)", mark.dimmed(), s.name.dimmed(), s.layer.dimmed());
        }
    }

    if !not_applicable.is_empty() {
        println!();
        println!("  {} Other ecosystem tools (not applicable to this project):", "→".dimmed());
        for s in &not_applicable {
            let mark = if s.available { "✓" } else { "–" };
            println!("    {} {:<20} [{:<12}]", mark.dimmed(), s.name.dimmed(), s.layer.dimmed());
        }
    }

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
        return stdio::handle_stdio();
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
