use crate::config::BarzelConfig;
use crate::detect::{detect_workspace, Language, ProjectInfo, WorkspaceInfo};
use crate::diff::DiffContext;
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
use crate::runners::health_check::HealthCheckRunner;
use crate::runners::operational_command::OperationalCommandRunner;
use crate::runners::stryker::StrykerRunner;
use crate::runners::tsc::TscRunner;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::path::Path;
use std::time::Duration;

/// The set of layer names accepted by `--layer` / stdio `layers`.
const VALID_LAYERS: &[&str] = &["logic", "structural", "hostile", "operational"];

/// Validate an explicitly requested layer list.
/// Returns `Err` if the list is empty or contains any unrecognised name.
fn validate_requested_layers(layers: &[String]) -> Result<()> {
    if layers.is_empty() {
        return Err(crate::error::BarzelError::Detection(format!(
            "layers must not be empty; valid layers: {}",
            VALID_LAYERS.join(", ")
        )));
    }
    let bad: Vec<&str> = layers
        .iter()
        .filter(|l| !VALID_LAYERS.contains(&l.as_str()))
        .map(String::as_str)
        .collect();
    if !bad.is_empty() {
        return Err(crate::error::BarzelError::Detection(format!(
            "unknown layer{}: {}; valid layers: {}",
            if bad.len() == 1 { "" } else { "s" },
            bad.join(", "),
            VALID_LAYERS.join(", ")
        )));
    }
    Ok(())
}

pub fn run_verification(
    target: Option<&Path>,
    layers: Option<Vec<String>>,
    no_cache: bool,
    fail_fast: bool,
    stdio: bool,
    json_out: bool,
    since: Option<&str>,
) -> Result<BarzelReport> {
    if let Some(ref requested) = layers {
        validate_requested_layers(requested)?;
    }

    let target_path = target.unwrap_or_else(|| Path::new("."));
    let workspace = detect_workspace(target_path)?;
    let cfg = BarzelConfig::load_for_project(target_path);

    // Resolve diff context when --since is provided.
    // If git lookup fails (not a git repo, bad rev), warn and fall back to full run.
    // diff_fallback tracks the requested rev so we can expose it in the report even on fallback.
    let (diff, diff_fallback_reason): (Option<DiffContext>, Option<String>) = match since {
        None => (None, None),
        Some(rev) => match DiffContext::since(target_path, rev) {
            Some(ctx) => (Some(ctx), None),
            None => {
                let reason = "not a git repository or invalid revision".to_string();
                if !stdio && !json_out {
                    eprintln!(
                        "{} --since {}: {} — running full suite",
                        "barzel:".yellow(),
                        rev,
                        reason
                    );
                }
                (None, Some(reason))
            }
        },
    };

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

            // Diff mode: skip if nothing in this project changed (and no root manifest changed)
            if let Some(ref ctx) = diff {
                let project_root = Path::new(&project.root);
                if !ctx.forces_full_run() && !ctx.affects_path(project_root) {
                    if !stdio && !json_out {
                        println!(
                            "  {} no changed files in project (--since {} — {})",
                            "↷".dimmed(),
                            since.unwrap_or(""),
                            ctx.summary()
                        );
                        println!();
                    }
                    let mut report = skip_report(&project, since, ctx);
                    report.fail_on = cfg.reporting.fail_on.clone();
                    emit_report(&report, stdio, json_out, target_path, &cfg.history)?;
                    return Ok(report);
                }
            }

            let mut report = run_project_report(&project, &cfg, &layers, no_cache, fail_fast, stdio)?;
            report.fail_on = cfg.reporting.fail_on.clone();
            // Record diff metadata whether diff succeeded or fell back
            report.diff_since = since.map(str::to_string);
            if let Some(ref ctx) = diff {
                report.diff_changed_files = Some(ctx.changed_files.len());
            } else {
                report.diff_fallback_reason = diff_fallback_reason.clone();
            }
            // Annotate before emit so regression findings appear in action_items.
            // Must run before emit_report, which calls save_from_report (history write).
            crate::history::annotate_metric_regressions(&mut report, target_path, &cfg.history);
            emit_report(&report, stdio, json_out, target_path, &cfg.history)?;
            Ok(report)
        }

        WorkspaceInfo::Multi { kind, members } => {
            // Determine which members to run
            let active_members: Vec<&(String, ProjectInfo)> = if let Some(ref ctx) = diff {
                if ctx.forces_full_run() {
                    if !stdio && !json_out {
                        println!(
                            "  {} root manifest/lockfile changed — running all {} package(s)",
                            "↷".dimmed(),
                            members.len()
                        );
                    }
                    members.iter().collect()
                } else {
                    let affected = select_active_members(&members, ctx);
                    if !stdio && !json_out && affected.len() < members.len() {
                        println!(
                            "  {} diff mode: {}/{} package(s) affected (--since {})",
                            "↷".dimmed(),
                            affected.len(),
                            members.len(),
                            since.unwrap_or("")
                        );
                    }
                    affected
                }
            } else {
                members.iter().collect()
            };

            if !stdio && !json_out {
                println!(
                    "{} {} workspace — {} package(s) at {}",
                    "→".bright_blue(),
                    kind.to_string().bright_green(),
                    active_members.len(),
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
            workspace_root: None,
            };
            let mut aggregate = BarzelReport::new(workspace_project);
            aggregate.fail_on = cfg.reporting.fail_on.clone();
            // Record diff metadata whether diff succeeded or fell back
            aggregate.diff_since = since.map(str::to_string);
            if let Some(ref ctx) = diff {
                aggregate.diff_changed_files = Some(ctx.changed_files.len());
            } else {
                aggregate.diff_fallback_reason = diff_fallback_reason.clone();
            }

            // When diff mode filtered to zero members, return a workspace-level skip report
            if active_members.is_empty() {
                if let Some(ref ctx) = diff {
                    if !stdio && !json_out {
                        println!(
                            "  {} no packages affected (--since {} — {})",
                            "↷".dimmed(),
                            since.unwrap_or(""),
                            ctx.summary()
                        );
                        println!();
                    }
                    aggregate.add_layer(workspace_skip_layer(since, ctx));
                    emit_report(&aggregate, stdio, json_out, target_path, &cfg.history)?;
                    return Ok(aggregate);
                }
            }

            for (pkg_path, member) in &active_members {
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
                    package_path: pkg_path.to_string(),
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

            crate::history::annotate_metric_regressions(&mut aggregate, target_path, &cfg.history);
            emit_report(&aggregate, stdio, json_out, target_path, &cfg.history)?;
            Ok(aggregate)
        }
    }
}

/// Produce a passing report representing a project that was skipped because
/// no files changed since the given revision.
fn skip_report(project: &ProjectInfo, since: Option<&str>, ctx: &DiffContext) -> BarzelReport {
    use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
    let mut report = BarzelReport::new(project.clone());
    report.diff_since = since.map(str::to_string);
    report.diff_changed_files = Some(ctx.changed_files.len());
    report.add_layer(LayerResult {
        name: "skipped".to_string(),
        runner: "diff".to_string(),
        status: LayerStatus::Skipped,
        findings: vec![Finding {
            severity: Severity::Info,
            code: "NO_CHANGES_SINCE_REV".to_string(),
            message: format!(
                "No source files changed since {} ({})",
                since.unwrap_or(""),
                ctx.summary()
            ),
            location: None,
            reproduce_cmd: Some(format!("git diff --name-only {} --", since.unwrap_or(""))),
            suggestion: None,
        }],
        metrics: LayerMetrics::default(),
        duration_ms: 0,
    });
    report
}

/// Filter workspace members whose roots contain changed files.
/// Full-run bypass is handled by the caller before this helper is called.
fn select_active_members<'a>(
    members: &'a [(String, ProjectInfo)],
    ctx: &DiffContext,
) -> Vec<&'a (String, ProjectInfo)> {
    members.iter()
        .filter(|(_, m)| ctx.affects_path(Path::new(&m.root)))
        .collect()
}

/// Build the skipped-layer entry for a workspace where no members were affected.
fn workspace_skip_layer(since: Option<&str>, ctx: &DiffContext) -> crate::report::LayerResult {
    use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
    LayerResult {
        name: "skipped".to_string(),
        runner: "diff".to_string(),
        status: LayerStatus::Skipped,
        findings: vec![Finding {
            severity: Severity::Info,
            code: "NO_CHANGES_SINCE_REV".to_string(),
            message: format!(
                "No workspace packages affected since {} ({})",
                since.unwrap_or(""),
                ctx.summary()
            ),
            location: None,
            reproduce_cmd: Some(format!("git diff --name-only {} --", since.unwrap_or(""))),
            suggestion: None,
        }],
        metrics: LayerMetrics::default(),
        duration_ms: 0,
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
    let health_check = HealthCheckRunner::new(cfg.layers.operational.health_checks.clone());
    let operational_cmd = OperationalCommandRunner::new(cfg.layers.operational.commands.clone());

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
    if !cfg.layers.operational.health_checks.is_empty() {
        language_runners.push(&health_check);
    }
    if !cfg.layers.operational.commands.is_empty() {
        language_runners.push(&operational_cmd);
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

fn emit_report(report: &BarzelReport, stdio: bool, json_out: bool, target_path: &Path, history_cfg: &crate::config::HistoryConfig) -> Result<()> {
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

    // Best-effort: persist metric snapshots for future trend/regression detection.
    // A write failure is warned on stderr but never propagates — run result is unaffected.
    if let Err(e) = crate::history::save_from_report(report, target_path, history_cfg) {
        if !stdio {
            eprintln!("{} history write failed: {}", "barzel:".yellow(), e);
        }
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
    use crate::report::{LayerMetrics, LayerResult, LayerStatus, ReportStatus, Severity};

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
            workspace_root: None,
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
            workspace_root: None,
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
            workspace_root: None,
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

    // ── skip_report ───────────────────────────────────────────────────────────

    fn diff_ctx_empty() -> crate::diff::DiffContext {
        crate::diff::DiffContext {
            changed_files: vec![],
            repo_root: std::path::PathBuf::from("/tmp"),
        }
    }

    fn project_info() -> crate::detect::ProjectInfo {
        crate::detect::ProjectInfo {
            language: Language::Python,
            root: "/tmp/myproj".to_string(),
            has_tests: true,
            package_name: Some("myproj".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    #[test]
    fn skip_report_has_no_changes_finding() {
        let ctx = diff_ctx_empty();
        let report = skip_report(&project_info(), Some("HEAD~1"), &ctx);
        assert!(report.layers.iter().any(|l| l.runner == "diff"));
        let layer = report.layers.iter().find(|l| l.runner == "diff").unwrap();
        assert!(matches!(layer.status, LayerStatus::Skipped));
        assert!(layer.findings.iter().any(|f| f.code == "NO_CHANGES_SINCE_REV"));
    }

    #[test]
    fn skip_report_finding_has_reproduce_cmd() {
        let ctx = diff_ctx_empty();
        let report = skip_report(&project_info(), Some("abc123"), &ctx);
        let finding = report.layers[0].findings.iter()
            .find(|f| f.code == "NO_CHANGES_SINCE_REV").unwrap();
        assert!(finding.reproduce_cmd.is_some());
        assert!(finding.reproduce_cmd.as_ref().unwrap().contains("abc123"));
    }

    #[test]
    fn skip_report_records_diff_since() {
        let ctx = diff_ctx_empty();
        let report = skip_report(&project_info(), Some("main"), &ctx);
        assert_eq!(report.diff_since, Some("main".to_string()));
        assert_eq!(report.diff_changed_files, Some(0));
    }

    #[test]
    fn skip_report_status_is_pass() {
        let ctx = diff_ctx_empty();
        let report = skip_report(&project_info(), Some("HEAD"), &ctx);
        assert!(matches!(report.status, ReportStatus::Pass));
    }

    #[test]
    fn skip_report_summary_matches_layer_findings() {
        let ctx = diff_ctx_empty();
        let report = skip_report(&project_info(), Some("HEAD~1"), &ctx);
        let layer_finding_count: usize = report.layers.iter().map(|l| l.findings.len()).sum();
        assert_eq!(
            report.summary.total_findings, layer_finding_count,
            "summary.total_findings must match actual finding count in layers"
        );
        assert_eq!(report.summary.total_findings, 1, "skip report has exactly one Info finding");
    }

    // ── health check runner registration ─────────────────────────────────────

    #[test]
    fn no_health_checks_means_no_health_check_runner_in_report() {
        use crate::config::{BarzelConfig, OperationalConfig};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        // Minimal project — no Cargo.toml, no .barzel.toml → defaults
        let project = crate::detect::ProjectInfo {
            language: Language::Unknown,
            root: dir.path().to_string_lossy().into_owned(),
            has_tests: false,
            package_name: None,
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };

        let mut cfg = BarzelConfig::default();
        cfg.layers.operational = OperationalConfig { health_checks: vec![], commands: vec![] };

        // Filter to Operational layer only so no language runners (Semgrep etc.) are
        // invoked — the test must not depend on external tool availability.
        let layers = Some(vec!["operational".to_string()]);
        let report = run_project_report(&project, &cfg, &layers, false, false, true).unwrap();

        let has_health_check_layer = report.layers.iter().any(|l| l.runner == "health-check");
        assert!(!has_health_check_layer,
            "with no health_checks configured, no health-check runner result must appear in the report");
        let has_cmd_layer = report.layers.iter().any(|l| l.runner == "operational-cmd");
        assert!(!has_cmd_layer,
            "with no commands configured, no operational-cmd runner result must appear in the report");
    }

    // ── workspace member selection (diff mode) ────────────────────────────────

    use std::process::Command as StdCommand;

    fn git_init(dir: &std::path::Path) {
        StdCommand::new("git").args(["init"]).current_dir(dir).output().unwrap();
        StdCommand::new("git").args(["config", "user.email", "t@t.com"]).current_dir(dir).output().unwrap();
        StdCommand::new("git").args(["config", "user.name", "T"]).current_dir(dir).output().unwrap();
    }

    fn git_commit_all(dir: &std::path::Path, msg: &str) {
        StdCommand::new("git").args(["add", "-A"]).current_dir(dir).output().unwrap();
        StdCommand::new("git").args(["commit", "-m", msg, "--allow-empty"]).current_dir(dir).output().unwrap();
    }

    fn head_sha(dir: &std::path::Path) -> String {
        String::from_utf8(
            StdCommand::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap().stdout
        ).unwrap().trim().to_string()
    }

    fn member_info(root: &std::path::Path) -> (String, crate::detect::ProjectInfo) {
        let rel = root.file_name().unwrap().to_string_lossy().to_string();
        let info = crate::detect::ProjectInfo {
            language: Language::Rust,
            root: root.to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some(rel.clone()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        (rel, info)
    }

    #[test]
    fn select_active_members_returns_only_changed_package() {
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let pkg_a = repo.path().join("crates/a");
        let pkg_b = repo.path().join("crates/b");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::create_dir_all(&pkg_b).unwrap();
        std::fs::write(pkg_a.join("lib.rs"), b"// a").unwrap();
        std::fs::write(pkg_b.join("lib.rs"), b"// b").unwrap();
        git_commit_all(repo.path(), "init");
        let rev = head_sha(repo.path());

        // Change only pkg_a
        std::fs::write(pkg_a.join("lib.rs"), b"// changed").unwrap();
        git_commit_all(repo.path(), "touch a");

        let ctx = DiffContext::since(repo.path(), &rev).unwrap();
        let members = vec![member_info(&pkg_a), member_info(&pkg_b)];
        let active = select_active_members(&members, &ctx);

        assert_eq!(active.len(), 1, "only pkg_a is affected");
        assert_eq!(active[0].0, "a");
    }

    #[test]
    fn full_run_bypass_remains_caller_responsibility() {
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let pkg_a = repo.path().join("crates/a");
        let pkg_b = repo.path().join("crates/b");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::create_dir_all(&pkg_b).unwrap();
        std::fs::write(repo.path().join("Cargo.lock"), b"# lock").unwrap();
        git_commit_all(repo.path(), "init");
        let rev = head_sha(repo.path());

        // Touch the lockfile — forces_full_run() will return true
        std::fs::write(repo.path().join("Cargo.lock"), b"# updated").unwrap();
        git_commit_all(repo.path(), "update lock");

        let ctx = DiffContext::since(repo.path(), &rev).unwrap();
        assert!(ctx.forces_full_run(), "lockfile change must force full run");

        // select_active_members should not be called when forces_full_run() is true;
        // verify it would return only affected members (not all) — full-run bypass is in run_verification
        let members = vec![member_info(&pkg_a), member_info(&pkg_b)];
        let affected = select_active_members(&members, &ctx);
        // Neither pkg has changed files — but caller (run_verification) uses all() when forces_full_run
        assert_eq!(affected.len(), 0,
            "select_active_members itself does prefix filtering; full-run bypass is caller's responsibility");
    }

    #[test]
    fn select_active_members_empty_when_no_changes() {
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let pkg_a = repo.path().join("crates/a");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::write(pkg_a.join("lib.rs"), b"// a").unwrap();
        git_commit_all(repo.path(), "init");
        let rev = head_sha(repo.path());
        // No further changes
        let ctx = DiffContext::since(repo.path(), &rev).unwrap();
        let members = vec![member_info(&pkg_a)];
        let active = select_active_members(&members, &ctx);
        assert!(active.is_empty(), "zero changed files → no active members");
    }

    #[test]
    fn workspace_skip_layer_has_correct_shape() {
        let ctx = diff_ctx_empty();
        let layer = workspace_skip_layer(Some("HEAD~2"), &ctx);
        assert_eq!(layer.runner, "diff");
        assert!(matches!(layer.status, LayerStatus::Skipped));
        let f = layer.findings.iter().find(|f| f.code == "NO_CHANGES_SINCE_REV")
            .expect("must have NO_CHANGES_SINCE_REV finding");
        assert!(matches!(f.severity, Severity::Info));
        let rc = f.reproduce_cmd.as_deref().unwrap_or("");
        assert!(!rc.trim().is_empty(), "reproduce_cmd must be non-empty");
        assert!(rc.contains("HEAD~2"), "reproduce_cmd must reference the since rev");
        assert!(f.message.contains("HEAD~2"), "message must reference the since rev");
    }

    #[test]
    fn deleted_file_in_member_still_triggers_that_member() {
        // Documents the canonicalize-fallback behavior: when a file is deleted,
        // canonicalize() fails on the missing path and falls back to repo_root.join(rel).
        // On systems without symlinks in the path, starts_with still matches correctly.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let pkg_a = repo.path().join("crates/a");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::write(pkg_a.join("old.rs"), b"// will be deleted").unwrap();
        git_commit_all(repo.path(), "init");
        let rev = head_sha(repo.path());

        // Delete the file — it now appears in `git diff` but does not exist on disk
        std::fs::remove_file(pkg_a.join("old.rs")).unwrap();
        git_commit_all(repo.path(), "delete old.rs");

        let ctx = DiffContext::since(repo.path(), &rev).unwrap();
        let members = vec![member_info(&pkg_a)];
        let active = select_active_members(&members, &ctx);
        assert_eq!(active.len(), 1,
            "a deletion inside a member must still mark that member as affected");
    }

    // ── validate_requested_layers ─────────────────────────────────────────────

    fn strs(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn valid_single_layer_accepted() {
        for layer in VALID_LAYERS {
            validate_requested_layers(&strs(&[layer])).unwrap_or_else(|e| {
                panic!("valid layer '{layer}' was rejected: {e}")
            });
        }
    }

    #[test]
    fn valid_multiple_layers_accepted() {
        validate_requested_layers(&strs(&["logic", "hostile"])).unwrap();
        validate_requested_layers(&strs(&["logic", "structural", "hostile", "operational"])).unwrap();
    }

    #[test]
    fn empty_layers_rejected() {
        let err = validate_requested_layers(&[]).unwrap_err().to_string();
        assert!(err.contains("not be empty"), "error must mention empty: {err}");
        assert!(err.contains("logic"), "error must list valid layers: {err}");
    }

    #[test]
    fn uppercase_layer_rejected() {
        let err = validate_requested_layers(&strs(&["Hostile"])).unwrap_err().to_string();
        assert!(err.contains("Hostile"), "error must name the bad value: {err}");
        assert!(err.contains("hostile"), "error must show correct spelling: {err}");
    }

    #[test]
    fn misspelled_layer_rejected() {
        let err = validate_requested_layers(&strs(&["security"])).unwrap_err().to_string();
        assert!(err.contains("security"), "error must name the bad value: {err}");
        assert!(err.contains("hostile"), "error must list valid layers: {err}");
    }

    #[test]
    fn mixed_valid_and_invalid_layers_rejected() {
        let err = validate_requested_layers(&strs(&["logic", "LOGIC"])).unwrap_err().to_string();
        assert!(err.contains("LOGIC"), "error must name the bad value: {err}");
    }

    #[test]
    fn run_verification_rejects_unknown_layer_before_running() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let result = run_verification(
            Some(dir.path()),
            Some(strs(&["security"])),
            false, false, true, false, None,
        );
        assert!(result.is_err(), "unknown layer must cause run_verification to return Err");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unknown layer"), "error must say 'unknown layer': {msg}");
        assert!(msg.contains("security"), "error must name the bad layer: {msg}");
        assert!(msg.contains("hostile"), "error must list valid layers: {msg}");
    }

    #[test]
    fn operational_layer_still_accepted() {
        // Regression: existing tests that pass Some(vec!["operational"]) must not break
        validate_requested_layers(&strs(&["operational"])).unwrap();
    }
}
