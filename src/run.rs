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

        WorkspaceInfo::Multi { kind, members } => {
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

    let mut report = if stdio {
        orchestrator.run(project)?
    } else {
        let pb = make_spinner();
        let pb_cb = pb.clone();
        let r = orchestrator.run_with_progress(project, move |runner_name, layer_name| {
            pb_cb.set_message(format!("  {:<12} [{:<14}]  running...", layer_name, runner_name));
            pb_cb.enable_steady_tick(Duration::from_millis(80));
        })?;
        pb.finish_and_clear();
        r
    };

    apply_coverage_threshold(&mut report, cfg.layers.logic.min_coverage);
    Ok(report)
}

/// Enforce `min_coverage` threshold against all Logic layers that reported coverage metrics.
/// Injects a High finding and marks the layer Partial if it was previously Pass.
/// Idempotent: skips layers that already have a COVERAGE_BELOW_THRESHOLD finding.
fn apply_coverage_threshold(report: &mut BarzelReport, min_coverage: Option<f64>) {
    let threshold = match min_coverage {
        Some(t) => t,
        None => return,
    };

    let mut any_injected = false;

    for layer in &mut report.layers {
        if layer.name != "logic" { continue; }
        let coverage = match layer.metrics.coverage {
            Some(c) => c,
            None => continue,
        };
        if coverage >= threshold { continue; }
        if layer.findings.iter().any(|f| f.code == "COVERAGE_BELOW_THRESHOLD") { continue; }

        let reproduce_cmd = reproduce_cmd_for_runner(&layer.runner);
        layer.findings.push(crate::report::Finding {
            severity: crate::report::Severity::High,
            code: "COVERAGE_BELOW_THRESHOLD".to_string(),
            message: format!(
                "Coverage {:.1}% is below the configured minimum of {:.1}% (runner: {})",
                coverage, threshold, layer.runner
            ),
            location: None,
            reproduce_cmd: Some(reproduce_cmd),
            suggestion: Some(format!(
                "Increase test coverage to at least {:.0}%. \
                 Add tests for uncovered branches and run with coverage reporting enabled.",
                threshold
            )),
        });

        if matches!(layer.status, crate::report::LayerStatus::Pass) {
            layer.status = crate::report::LayerStatus::Partial;
        }
        any_injected = true;
    }

    if any_injected {
        report.recompute_summary();
    }
}

fn reproduce_cmd_for_runner(runner: &str) -> String {
    match runner {
        "pytest"  => "pytest --cov --cov-report=term-missing 2>&1 | tail -20".to_string(),
        "jest"    => "npx jest --coverage 2>&1 | tail -20".to_string(),
        "go-test" => "go test ./... -cover 2>&1 | tail -20".to_string(),
        _         => format!("{} (with coverage enabled) 2>&1 | tail -20", runner),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks};
    use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, ReportStatus, Severity};

    fn logic_layer_with_coverage(runner: &str, coverage: f64, status: LayerStatus) -> LayerResult {
        LayerResult {
            name: "logic".to_string(),
            runner: runner.to_string(),
            status,
            findings: vec![],
            metrics: LayerMetrics { coverage: Some(coverage), ..Default::default() },
            duration_ms: 0,
        }
    }

    fn make_report_with_layer(layer: LayerResult) -> BarzelReport {
        let project = crate::detect::ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("testpkg".to_string()),
            frameworks: ProjectFrameworks::default(),
        };
        let mut report = BarzelReport::new(project);
        report.add_layer(layer);
        report
    }

    #[test]
    fn no_threshold_leaves_report_unchanged() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 40.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, None);
        assert!(report.layers[0].findings.is_empty());
        assert!(matches!(report.layers[0].status, LayerStatus::Pass));
    }

    #[test]
    fn coverage_above_threshold_no_finding() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 90.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(85.0));
        assert!(!report.layers[0].findings.iter().any(|f| f.code == "COVERAGE_BELOW_THRESHOLD"));
        assert!(matches!(report.layers[0].status, LayerStatus::Pass));
    }

    #[test]
    fn coverage_equal_to_threshold_no_finding() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 85.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(85.0));
        assert!(!report.layers[0].findings.iter().any(|f| f.code == "COVERAGE_BELOW_THRESHOLD"));
    }

    #[test]
    fn coverage_below_threshold_injects_high_finding() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 72.5, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(85.0));

        let finding = report.layers[0].findings.iter().find(|f| f.code == "COVERAGE_BELOW_THRESHOLD");
        assert!(finding.is_some(), "expected COVERAGE_BELOW_THRESHOLD finding");
        let f = finding.unwrap();
        assert_eq!(f.severity, Severity::High);
        assert!(f.reproduce_cmd.is_some(), "finding must include reproduce_cmd");
        assert!(f.reproduce_cmd.as_ref().unwrap().contains("pytest"));
        assert!(f.message.contains("72.5"));
        assert!(f.message.contains("85.0"));
    }

    #[test]
    fn coverage_below_threshold_marks_layer_partial() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 50.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(80.0));
        assert!(matches!(report.layers[0].status, LayerStatus::Partial));
    }

    #[test]
    fn coverage_below_threshold_updates_report_summary() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 50.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(80.0));
        assert_eq!(report.summary.high, 1);
        assert_eq!(report.summary.total_findings, 1);
        assert!(matches!(report.status, ReportStatus::Partial));
    }

    #[test]
    fn enforcement_is_idempotent() {
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 50.0, LayerStatus::Pass));
        apply_coverage_threshold(&mut report, Some(80.0));
        apply_coverage_threshold(&mut report, Some(80.0));
        let count = report.layers[0].findings.iter()
            .filter(|f| f.code == "COVERAGE_BELOW_THRESHOLD")
            .count();
        assert_eq!(count, 1, "duplicate findings must not be injected");
    }

    #[test]
    fn non_logic_layer_not_affected() {
        let project = crate::detect::ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("testpkg".to_string()),
            frameworks: ProjectFrameworks::default(),
        };
        let mut report = BarzelReport::new(project);
        report.add_layer(LayerResult {
            name: "structural".to_string(),
            runner: "mutmut".to_string(),
            status: LayerStatus::Pass,
            findings: vec![],
            metrics: LayerMetrics { coverage: Some(40.0), ..Default::default() },
            duration_ms: 0,
        });
        apply_coverage_threshold(&mut report, Some(80.0));
        assert!(report.layers[0].findings.is_empty());
    }

    #[test]
    fn layer_without_coverage_metric_not_affected() {
        let project = crate::detect::ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("testpkg".to_string()),
            frameworks: ProjectFrameworks::default(),
        };
        let mut report = BarzelReport::new(project);
        report.add_layer(LayerResult {
            name: "logic".to_string(),
            runner: "pytest".to_string(),
            status: LayerStatus::Pass,
            findings: vec![],
            metrics: LayerMetrics { coverage: None, ..Default::default() },
            duration_ms: 0,
        });
        apply_coverage_threshold(&mut report, Some(80.0));
        assert!(report.layers[0].findings.is_empty());
    }

    #[test]
    fn failing_layer_status_unchanged_when_below_threshold() {
        // If a layer already Fails (e.g. test failures), stay Fail — don't downgrade to Partial
        let mut report = make_report_with_layer(logic_layer_with_coverage("pytest", 30.0, LayerStatus::Fail));
        apply_coverage_threshold(&mut report, Some(80.0));
        assert!(matches!(report.layers[0].status, LayerStatus::Fail));
    }
}
