use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

/// Runner for Property-Based Testing using `proptest`
pub struct ProptestRunner;

impl TestRunner for ProptestRunner {
    fn name(&self) -> &'static str {
        "proptest"
    }

    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }

        let cargo_toml = std::path::Path::new(&project.root).join("Cargo.toml");
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            content.contains("proptest")
        } else {
            false
        }
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        // Run cargo test (proptest tests are discovered automatically)
        let output = Command::new("cargo")
            .args(["test", "--quiet"])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let passed = result.status.success();
                let status = if passed { LayerStatus::Pass } else { LayerStatus::Fail };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    status,
                    findings: if passed {
                        vec![]
                    } else {
                        vec![Finding {
                            severity: Severity::High,
                            code: "PBT_FAILURE".to_string(),
                            message: "Property-based tests failed".to_string(),
                            location: None,
                        }]
                    },
                    metrics: crate::report::LayerMetrics {
                        tests_run: 1,
                        passed: if passed { 1 } else { 0 },
                        failed: if passed { 0 } else { 1 },
                        coverage: None,
                        mutation_score: None,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "PBT_EXECUTION_ERROR".to_string(),
                    message: format!("Failed to run proptest: {}", e),
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
