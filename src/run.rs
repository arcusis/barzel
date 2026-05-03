use crate::config::BarzelConfig;
use crate::detect::{detect_workspace, Language, ProjectInfo, WorkspaceInfo};
use crate::error::Result;
use crate::orchestrator::VerificationOrchestrator;
use crate::report::{BarzelReport, LayerStatus, ReportStatus, Severity};
use crate::runners::aisec::AiSecRunner;
use crate::runners::bandit::BanditRunner;
use crate::runners::cargo_audit::CargoAuditRunner;
use crate::runners::cargo_fuzz::CargoFuzzRunner;
use crate::runners::eslint::EslintRunner;
use crate::runners::fastcheck::FastCheckRunner;
use crate::runners::go_mutesting::GoMutestingRunner;
use crate::runners::gotest::GoTestRunner;
use crate::runners::jest::JestRunner;
use crate::runners::kani::KaniRunner;
use crate::runners::mutants::MutantsRunner;
use crate::runners::mutmut::MutmutRunner;
use crate::runners::mypy::MypyRunner;
use crate::runners::npm_audit::NpmAuditRunner;
use crate::runners::pip_audit::PipAuditRunner;
use crate::runners::playwright::PlaywrightRunner;
use crate::runners::proptest::ProptestRunner;
use crate::runners::pytest::PytestRunner;
use crate::runners::semgrep::SemgrepRunner;
use crate::runners::stryker::StrykerRunner;
use crate::runners::tsc::TscRunner;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::path::Path;
use std::time::Duration;

pub fn run_verification(
    target: Option<&Path>,
    layers: Option<Vec<String>>,
    no_cache: bool,
    fail_fast: bool,
    stdio: bool,
    json_out: bool,
) -> Result<BarzelReport> {
    let target_path = target.unwrap_or_else(|| Path::new("."));
    let workspace = detect_workspace(target_path)?;
    let cfg = BarzelConfig::load_for_project(target_path);

    match workspace {
        WorkspaceInfo::Single(project) => {
            if !stdio && !json_out {
                println!(
                    "{} Verifying {} project at {}",
                    "→".bright_blue(),
                    project.language.to_string().bright_green(),
                    target_path.display().to_string().bright_cyan()
                );
                println!();
            }
            let mut report = run_project_report(&project, &cfg, &layers, no_cache, fail_fast, stdio)?;
            report.fail_on = cfg.reporting.fail_on.clone();
            emit_report(&report, stdio, json_out, target_path)?;
            Ok(report)
        }

        WorkspaceInfo::Multi { root: _, kind, members } => {
            if !stdio && !json_out {
                println!(
                    "{} {} workspace — {} package(s) at {}",
                    "→".bright_blue(),
                    kind.to_string().bright_green(),
                    members.len(),
                    target_path.display().to_string().bright_cyan()
                );
                println!();
            }

            // Aggregate report uses a synthetic workspace-root project — not the first member
            let workspace_project = ProjectInfo {
                language: Language::Unknown,
                root: target_path.to_string_lossy().to_string(),
                has_tests: members.iter().any(|(_, m)| m.has_tests),
                package_name: Some(format!("{}-workspace", kind)),
                frameworks: Default::default(),
            };
            let mut aggregate = BarzelReport::new(workspace_project);
            aggregate.fail_on = cfg.reporting.fail_on.clone();

            for (pkg_path, member) in &members {
                if !stdio && !json_out {
                    println!("  {} {}", "package:".dimmed(), pkg_path.bright_white());
                }

                let member_cfg = if Path::new(&member.root).join(".barzel.toml").exists() {
                    BarzelConfig::load_for_project(Path::new(&member.root))
                } else {
                    cfg.clone()
                };

                let member_report = run_project_report(member, &member_cfg, &layers, no_cache, fail_fast, stdio)?;

                // Store per-package report for rich stdio output
                use crate::report::WorkspaceMemberReport;
                aggregate.workspace_members.push(WorkspaceMemberReport {
                    package_path: pkg_path.clone(),
                    language: member.language.to_string(),
                    status: member_report.status,
                    layers: member_report.layers.clone(),
                    summary: member_report.summary.clone(),
                });

                // Fold member layers into aggregate summary/status
                for layer in member_report.layers {
                    aggregate.add_layer(layer);
                }
            }

            emit_report(&aggregate, stdio, json_out, target_path)?;
            Ok(aggregate)
        }
    }
}

/// Execute runners for a single `ProjectInfo` and return the report.
fn run_project_report(
    project: &ProjectInfo,
    cfg: &BarzelConfig,
    layers: &Option<Vec<String>>,
    no_cache: bool,
    fail_fast: bool,
    stdio: bool,
) -> Result<BarzelReport> {
    let threshold = cfg.layers.structural.mutation_threshold;

    // All runner instances must be named locals — their borrows must outlive `filtered`
    let proptest = ProptestRunner::default();
    let kani = KaniRunner::default();
    let mutants = MutantsRunner::with_threshold(threshold);
    let cargo_fuzz = CargoFuzzRunner::default();
    let cargo_audit = CargoAuditRunner::default();
    let npm_audit = NpmAuditRunner::default();
    let pip_audit = PipAuditRunner::default();
    let tsc = TscRunner::default();
    let eslint = EslintRunner::default();
    let fastcheck = FastCheckRunner::default();
    let jest = JestRunner::default();
    let stryker = StrykerRunner::with_threshold(threshold);
    let gotest = GoTestRunner::default();
    let go_mutesting = GoMutestingRunner::with_threshold(threshold);
    let semgrep = SemgrepRunner::default();
    let pytest = PytestRunner::default();
    let mypy = MypyRunner::default();
    let mutmut = MutmutRunner::with_threshold(threshold);
    let bandit = BanditRunner::default();
    let aisec = AiSecRunner::default();
    let playwright = PlaywrightRunner::default();

    let mut language_runners: Vec<&dyn crate::plugin::TestRunner> = match project.language {
        Language::Rust => vec![&proptest, &kani, &mutants, &cargo_fuzz, &cargo_audit, &semgrep],
        Language::TypeScript => vec![&jest, &tsc, &fastcheck, &stryker, &playwright, &eslint, &npm_audit, &semgrep],
        Language::Python => vec![&pytest, &mypy, &mutmut, &bandit, &pip_audit, &semgrep],
        Language::Go => vec![&gotest, &go_mutesting, &semgrep],
        Language::Unknown => vec![&semgrep],
    };

    if project.frameworks.has_ai_deps {
        language_runners.push(&aisec);
    }

    let enabled = &cfg.layers.enabled;
    let filtered: Vec<&dyn crate::plugin::TestRunner> = language_runners
        .into_iter()
        .filter(|runner| {
            let layer_str = runner.layer().as_str();
            let cli_ok = layers
                .as_ref()
                .map(|req| req.iter().any(|r| r == layer_str))
                .unwrap_or(true);
            let cfg_ok = enabled.iter().any(|e| e == layer_str);
            cli_ok && cfg_ok
        })
        .collect();

    let mut orchestrator = VerificationOrchestrator::new(filtered);
    if no_cache { orchestrator = orchestrator.with_no_cache(); }
    if fail_fast { orchestrator = orchestrator.with_fail_fast(); }

    if stdio {
        orchestrator.run(project)
    } else {
        let pb = make_spinner();
        let pb_cb = pb.clone();
        let r = orchestrator.run_with_progress(project, move |runner_name, layer_name| {
            pb_cb.set_message(format!("  {:<12} [{:<14}]  running...", layer_name, runner_name));
            pb_cb.enable_steady_tick(Duration::from_millis(80));
        })?;
        pb.finish_and_clear();
        Ok(r)
    }
}

fn emit_report(report: &BarzelReport, stdio: bool, json_out: bool, target_path: &Path) -> Result<()> {
    if json_out {
        println!("{}", serde_json::to_string(report).unwrap_or_default());
        report.save(target_path)?;
    } else if !stdio {
        print_human_report(report);
        report.save(target_path)?;

        let overall = match report.status {
            ReportStatus::Pass => "PASS".bright_green().to_string(),
            ReportStatus::Partial => "PARTIAL".yellow().to_string(),
            ReportStatus::Fail => "FAIL".bright_red().to_string(),
        };
        let report_path = target_path.join(".barzel").join("reports");
        println!(
            "{} {}  |  {} finding(s)  |  {}",
            "→".bright_blue(),
            overall,
            report.summary.total_findings,
            report_path.display().to_string().bright_cyan()
        );
    } else {
        report.save(target_path)?;
    }
    Ok(())
}

fn print_human_report(report: &BarzelReport) {
    // For workspaces: render per-package grouped output from workspace_members
    if !report.workspace_members.is_empty() {
        for member in &report.workspace_members {
            println!("  {} {}", "package:".dimmed(), member.package_path.bright_white());
            for layer in &member.layers {
                print_layer_row(layer);
            }
        }
        println!();
        return;
    }

    for layer in &report.layers {
        print_layer_row(layer);
    }

    println!();
}

fn print_layer_row(layer: &crate::report::LayerResult) {
    let status_str = match layer.status {
        LayerStatus::Pass => "PASS   ".bright_green().to_string(),
        LayerStatus::Fail => "FAIL   ".bright_red().to_string(),
        LayerStatus::Partial => "PARTIAL".yellow().to_string(),
        LayerStatus::Skipped => "SKIPPED".dimmed().to_string(),
    };

    let detail = match layer.status {
        LayerStatus::Pass => {
            let m = &layer.metrics;
            let mut parts = Vec::new();
            if m.tests_run > 0 { parts.push(format!("{} tests", m.tests_run)); }
            if let Some(cov) = m.coverage { parts.push(format!("{cov:.0}% cov")); }
            if let Some(ms) = Some(m.mutation_score).flatten() { parts.push(format!("{ms:.0}% mut")); }
            parts.push(format!("{}ms", layer.duration_ms));
            parts.join(" · ")
        }
        LayerStatus::Skipped => layer.findings.first().map(|f| f.message.clone()).unwrap_or_default(),
        LayerStatus::Fail | LayerStatus::Partial => {
            let count = layer.findings.len();
            format!("{} finding{}", count, if count == 1 { "" } else { "s" })
        }
    };

    println!(
        "  {:<12} [{:<14}]  {}  {}",
        layer.name.bright_white(),
        layer.runner.dimmed(),
        status_str,
        detail.dimmed()
    );

    for finding in layer.findings.iter().filter(|f| matches!(f.severity, Severity::Critical | Severity::High)) {
        println!("    {} {}", "↳".dimmed(), finding.message.dimmed());
        if let Some(cmd) = &finding.reproduce_cmd {
            println!("      {} {}", "run:".dimmed(), cmd.bright_cyan().dimmed());
        }
    }
}

fn make_spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.blue} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb
}
