use crate::config::BarzelConfig;
use crate::detect::{detect_project, Language};
use crate::error::Result;
use crate::orchestrator::VerificationOrchestrator;
use crate::report::{BarzelReport, LayerStatus, ReportStatus, Severity};
use crate::runners::cargo_fuzz::CargoFuzzRunner;
use crate::runners::fastcheck::FastCheckRunner;
use crate::runners::go_mutesting::GoMutestingRunner;
use crate::runners::gotest::GoTestRunner;
use crate::runners::kani::KaniRunner;
use crate::runners::mutants::MutantsRunner;
use crate::runners::proptest::ProptestRunner;
use crate::runners::semgrep::SemgrepRunner;
use crate::runners::stryker::StrykerRunner;
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
) -> Result<BarzelReport> {
    let target_path = target.unwrap_or_else(|| Path::new("."));
    let project = detect_project(target_path)?;
    let cfg = BarzelConfig::load_for_project(target_path);

    if !stdio {
        println!(
            "{} Verifying {} project at {}",
            "→".bright_blue(),
            project.language.to_string().bright_green(),
            target_path.display().to_string().bright_cyan()
        );
        println!();
    }

    let threshold = cfg.layers.structural.mutation_threshold;

    // Instantiate runners — all must be named locals so their borrows outlive `filtered`
    let proptest = ProptestRunner;
    let kani = KaniRunner;
    let mutants = MutantsRunner { mutation_threshold: threshold };
    let cargo_fuzz = CargoFuzzRunner;
    let fastcheck = FastCheckRunner;
    let stryker = StrykerRunner { mutation_threshold: threshold };
    let gotest = GoTestRunner;
    let go_mutesting = GoMutestingRunner { mutation_threshold: threshold };
    let semgrep = SemgrepRunner;

    let language_runners: Vec<&dyn crate::plugin::TestRunner> = match project.language {
        Language::Rust => vec![&proptest, &kani, &mutants, &cargo_fuzz, &semgrep],
        Language::TypeScript => vec![&fastcheck, &stryker, &semgrep],
        Language::Go => vec![&gotest, &go_mutesting, &semgrep],
        Language::Unknown => vec![&semgrep],
    };

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
    if no_cache {
        orchestrator = orchestrator.with_no_cache();
    }
    if fail_fast {
        orchestrator = orchestrator.with_fail_fast();
    }

    let report = if stdio {
        orchestrator.run(&project)?
    } else {
        let pb = make_spinner();
        let pb_cb = pb.clone();

        let r = orchestrator.run_with_progress(&project, move |runner_name, layer_name| {
            pb_cb.set_message(format!(
                "  {:<12} [{:<14}]  running...",
                layer_name, runner_name
            ));
            pb_cb.enable_steady_tick(Duration::from_millis(80));
        })?;

        pb.finish_and_clear();
        r
    };

    if !stdio {
        print_human_report(&report);
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

    Ok(report)
}

fn print_human_report(report: &BarzelReport) {
    for layer in &report.layers {
        let status_str = match layer.status {
            LayerStatus::Pass => "PASS   ".bright_green().to_string(),
            LayerStatus::Fail => "FAIL   ".bright_red().to_string(),
            LayerStatus::Partial => "PARTIAL".yellow().to_string(),
            LayerStatus::Skipped => "SKIPPED".dimmed().to_string(),
        };

        let detail = match layer.status {
            LayerStatus::Pass => {
                let m = &layer.metrics;
                if m.tests_run > 0 {
                    format!("{} tests · {}ms", m.tests_run, layer.duration_ms)
                } else {
                    format!("{}ms", layer.duration_ms)
                }
            }
            LayerStatus::Skipped => layer
                .findings
                .first()
                .map(|f| f.message.clone())
                .unwrap_or_default(),
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

        // Show high/critical findings inline with reproduce command
        for finding in layer
            .findings
            .iter()
            .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        {
            println!("    {} {}", "↳".dimmed(), finding.message.dimmed());
            if let Some(cmd) = &finding.reproduce_cmd {
                println!("      {} {}", "run:".dimmed(), cmd.bright_cyan().dimmed());
            }
        }
    }

    println!();
}

fn make_spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.blue} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb
}
