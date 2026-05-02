use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

/// Runner for Mutation Testing using `cargo-mutants`
pub struct MutantsRunner;

impl TestRunner for MutantsRunner {
    fn name(&self) -> &'static str {
        "cargo-mutants"
    }

    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }

        // Check if cargo-mutants is installed
        Command::new("cargo")
            .args(["mutants", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        let output = Command::new("cargo")
            .args(["mutants", "--timeout", "30", "--no-shuffle"])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                let mutation_score = parse_mutation_score(&output_str);

                let status = if let Some(score) = mutation_score {
                    if score >= 95.0 { LayerStatus::Pass } else { LayerStatus::Partial }
                } else {
                    LayerStatus::Partial
                };

                let findings = if let Some(score) = mutation_score {
                    if score < 95.0 {
                        vec![Finding {
                            severity: Severity::High,
                            code: "LOW_MUTATION_SCORE".to_string(),
                            message: format!("Mutation score is {:.1}% (target ≥95%)", score),
                            location: None,
                        }]
                    } else {
                        vec![]
                    }
                } else {
                    vec![Finding {
                        severity: Severity::Info,
                        code: "MUTATION_RUN_COMPLETE".to_string(),
                        message: "Mutation testing completed".to_string(),
                        location: None,
                    }]
                };

                Ok(LayerResult {
                    name: "structural".to_string(),
                    status,
                    findings,
                    metrics: crate::report::LayerMetrics {
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
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "MUTATION_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run cargo-mutants: {}", e),
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

fn parse_mutation_score(output: &str) -> Option<f64> {
    for line in output.lines() {
        if line.contains("mutation score") {
            if let Some(percent) = line.split('%').next() {
                if let Some(num_str) = percent.split_whitespace().last() {
                    if let Ok(score) = num_str.parse::<f64>() {
                        return Some(score);
                    }
                }
            }
        }
    }
    None
}
