use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

/// Runner for SAST using Semgrep
pub struct SemgrepRunner;

impl TestRunner for SemgrepRunner {
    fn name(&self) -> &'static str {
        "semgrep"
    }

    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn is_available(&self, _project: &ProjectInfo) -> bool {
        Command::new("semgrep")
            .args(["--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        let output = Command::new("semgrep")
            .args(["--json", "--quiet", "."])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                let findings = parse_semgrep_findings(&output_str);

                let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical | Severity::High)) {
                    LayerStatus::Fail
                } else if !findings.is_empty() {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    status,
                    findings,
                    metrics: crate::report::LayerMetrics {
                        tests_run: 0,
                        passed: 0,
                        failed: 0,
                        coverage: None,
                        mutation_score: None,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "SAST_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run semgrep: {}", e),
                    location: None,
                }],
                metrics: crate::report::LayerMetrics {
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

fn parse_semgrep_findings(output: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    if output.contains("\"results\":[]") || output.is_empty() {
        return findings;
    }

    if output.contains("\"results\"") {
        findings.push(Finding {
            severity: Severity::Medium,
            code: "SAST_FINDINGS".to_string(),
            message: "Semgrep found potential security issues".to_string(),
            location: None,
        });
    }

    findings
}
