use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use super::fastcheck::detect_package_manager;

pub struct StrykerRunner {
    pub mutation_threshold: f64,
}

impl Default for StrykerRunner {
    fn default() -> Self {
        Self { mutation_threshold: 95.0 }
    }
}

impl TestRunner for StrykerRunner {
    fn name(&self) -> &'static str {
        "stryker"
    }

    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn skip_message(&self) -> &'static str {
        "Stryker not configured — add `@stryker-mutator/core` and a stryker.config.mjs to enable mutation testing (target: ≥95% mutation score)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        let root = Path::new(&project.root);
        let has_config = root.join("stryker.conf.json").exists()
            || root.join("stryker.config.js").exists()
            || root.join("stryker.config.mjs").exists()
            || root.join("stryker.config.cjs").exists();

        let has_dep = {
            let pkg = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
            pkg.contains("@stryker-mutator")
        };

        has_config || has_dep
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);
        let pm = detect_package_manager(root);

        // Run stryker with JSON reporter
        let output = Command::new("npx")
            .args(["stryker", "run", "--reporters", "json,clear-text"])
            .current_dir(root)
            .output();

        match output {
            Ok(result) => {
                // Try to parse the JSON mutation report
                let report_path = root.join("reports").join("mutation").join("mutation.json");
                let mutation_score = if report_path.exists() {
                    parse_stryker_report(&report_path)
                } else {
                    // Fall back to parsing stdout
                    let stdout = String::from_utf8_lossy(&result.stdout);
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    parse_stryker_text_output(&format!("{}\n{}", stdout, stderr))
                };

                let threshold = self.mutation_threshold;
                let status = match mutation_score {
                    Some(score) if score >= threshold => LayerStatus::Pass,
                    Some(_) => LayerStatus::Partial,
                    None if result.status.success() => LayerStatus::Partial,
                    None => LayerStatus::Fail,
                };

                let findings = build_stryker_findings(mutation_score, self.mutation_threshold, pm);

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "stryker".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: 0,
                        passed: 0,
                        failed: 0,
                        coverage: None,
                        mutation_score,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "structural".to_string(),
                runner: "stryker".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "STRYKER_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run stryker: {}", e),
                    reproduce_cmd: Some("npx stryker run".to_string()),
                    suggestion: Some(format!(
                        "Install stryker: `{} add -D @stryker-mutator/core @stryker-mutator/vitest-runner`",
                        pm
                    )),
                    ..Default::default()
                }],
                metrics: LayerMetrics {
                    tests_run: 0,
                    passed: 0,
                    failed: 1,
                    coverage: None,
                    mutation_score: None,
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

fn parse_stryker_report(path: &Path) -> Option<f64> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let files = json.get("files")?.as_object()?;

    let mut killed = 0u64;
    let mut survived = 0u64;
    let mut no_coverage = 0u64;

    for (_file, file_data) in files {
        let mutants = file_data.get("mutants")?.as_array()?;
        for mutant in mutants {
            match mutant.get("status").and_then(|s| s.as_str()) {
                Some("Killed") | Some("Timeout") => killed += 1,
                Some("Survived") => survived += 1,
                Some("NoCoverage") => no_coverage += 1,
                _ => {}
            }
        }
    }

    let total = killed + survived + no_coverage;
    if total == 0 {
        return None;
    }
    Some(killed as f64 / total as f64 * 100.0)
}

fn parse_stryker_text_output(output: &str) -> Option<f64> {
    // Look for "Mutation score: 87.50%"
    for line in output.lines() {
        if line.contains("Mutation score") && line.contains('%') {
            if let Some(pct) = line.split('%').next() {
                if let Some(num) = pct.split_whitespace().last() {
                    return num.parse().ok();
                }
            }
        }
        // Alternative: "87.50 (34/39)"
        if line.contains("(") && line.contains("/") && line.contains(")") {
            if let Some(score_str) = line.split_whitespace().next() {
                if let Ok(score) = score_str.parse::<f64>() {
                    if score <= 100.0 {
                        return Some(score);
                    }
                }
            }
        }
    }
    None
}

fn build_stryker_findings(mutation_score: Option<f64>, threshold: f64, _pm: &str) -> Vec<Finding> {
    match mutation_score {
        Some(score) if score >= threshold => vec![],
        Some(score) => vec![Finding {
            severity: Severity::High,
            code: "LOW_MUTATION_SCORE".to_string(),
            message: format!(
                "Mutation score is {:.1}% (target ≥{:.0}%) — {:.1}% of mutants survived",
                score, threshold, 100.0 - score
            ),
            reproduce_cmd: Some("npx stryker run".to_string()),
            suggestion: Some(
                "Open reports/mutation/mutation.html to see surviving mutants. \
                 Add test cases that specifically target the uncovered conditions."
                    .to_string(),
            ),
            ..Default::default()
        }],
        None => vec![Finding {
            severity: Severity::Info,
            code: "MUTATION_NO_SCORE".to_string(),
            message: "Stryker ran but could not determine mutation score. Check reports/mutation/."
                .to_string(),
            reproduce_cmd: Some("npx stryker run && open reports/mutation/mutation.html".to_string()),
            suggestion: None,
            ..Default::default()
        }],
    }
}
