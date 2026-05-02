use crate::detect::detect_project;
use crate::error::Result;
use crate::orchestrator::VerificationOrchestrator;
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use crate::runners::proptest::ProptestRunner;
use owo_colors::OwoColorize;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub fn run_verification(target: Option<&Path>, layers: Option<Vec<String>>, stdio: bool) -> Result<()> {
    let target_path = target.unwrap_or_else(|| Path::new("."));
    let project = detect_project(target_path)?;

    if !stdio {
        println!(
            "{} Running verification on {} project",
            "→".bright_blue(),
            project.language.to_string().bright_green()
        );
    }

    // Use the new plugin-based orchestrator
    let proptest_runner = ProptestRunner;
    let runners: Vec<&dyn crate::plugin::TestRunner> = vec![&proptest_runner];

    let orchestrator = VerificationOrchestrator::new(runners);
    let mut report = orchestrator.run(&project)?;

    // For now, still run the old structural and hostile layers (will be migrated next)
    let enabled_layers = layers.unwrap_or_else(|| {
        vec!["structural".to_string(), "hostile".to_string()]
    });

    for layer_name in &enabled_layers {
        let layer_start = Instant::now();

        let layer_result = match layer_name.as_str() {
            "structural" => run_structural_layer(&project, layer_start),
            "hostile" => run_hostile_layer(&project, layer_start),
            _ => LayerResult {
                name: layer_name.clone(),
                status: LayerStatus::Skipped,
                findings: vec![Finding {
                    severity: Severity::Info,
                    code: "LAYER_UNKNOWN".to_string(),
                    message: format!("Layer '{}' is not yet supported", layer_name),
                    location: None,
                }],
                metrics: LayerMetrics {
                    tests_run: 0,
                    passed: 0,
                    failed: 0,
                    coverage: None,
                    mutation_score: None,
                },
                duration_ms: 0,
            },
        };

        report.add_layer(layer_result);
    }

    // Save the report
    let report_path = report.save(target_path)?;

    if !stdio {
        println!();
        println!(
            "{} Report saved to {}",
            "✓".bright_green(),
            report_path.display().to_string().bright_cyan()
        );
        println!(
            "{} Status: {:?} | Findings: {}",
            "→".bright_blue(),
            report.status,
            report.summary.total_findings
        );
    }

    Ok(())
}

#[allow(dead_code)]
fn run_logic_layer(project: &crate::detect::ProjectInfo, start: Instant) -> LayerResult {
    let project_root = Path::new(&project.root);
    let cargo_toml = project_root.join("Cargo.toml");

    // Check if the project uses proptest
    let has_proptest = if cargo_toml.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            content.contains("proptest")
        } else {
            false
        }
    } else {
        false
    };

    if !has_proptest {
        return LayerResult {
            name: "logic".to_string(),
            status: LayerStatus::Skipped,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "NO_PBT_FOUND".to_string(),
                message: "No property-based tests detected. Add `proptest` to your dev-dependencies for invariant testing.".to_string(),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 0,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // Project has proptest — attempt to run tests
    let output = Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(project_root)
        .output();

    match output {
        Ok(result) => {
            let passed = result.status.success();
            let status = if passed { LayerStatus::Pass } else { LayerStatus::Fail };
            let message = if passed {
                "Property-based tests executed successfully via cargo test"
            } else {
                "Some property-based tests failed (see cargo test output for details)"
            };

            LayerResult {
                name: "logic".to_string(),
                status,
                findings: if passed {
                    vec![]
                } else {
                    vec![Finding {
                        severity: Severity::High,
                        code: "PBT_FAILURE".to_string(),
                        message: message.to_string(),
                        location: None,
                    }]
                },
                metrics: LayerMetrics {
                    tests_run: 1, // We don't parse exact count yet
                    passed: if passed { 1 } else { 0 },
                    failed: if passed { 0 } else { 1 },
                    coverage: None,
                    mutation_score: None,
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        Err(e) => LayerResult {
            name: "logic".to_string(),
            status: LayerStatus::Fail,
            findings: vec![Finding {
                severity: Severity::Critical,
                code: "PBT_EXECUTION_FAILED".to_string(),
                message: format!("Failed to execute cargo test: {}", e),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 1,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn run_structural_layer(project: &crate::detect::ProjectInfo, start: Instant) -> LayerResult {
    let project_root = Path::new(&project.root);

    // Check if cargo-mutants is available
    let mutants_available = Command::new("cargo")
        .args(["mutants", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !mutants_available {
        return LayerResult {
            name: "structural".to_string(),
            status: LayerStatus::Skipped,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "NO_MUTATION_TESTING".to_string(),
                message: "Mutation testing not available. Install `cargo-mutants` for high-quality test verification (aim for ≥95% mutation score).".to_string(),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 0,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // cargo-mutants is available — run it (this can take a long time, so we do a quick check first)
    // For M3 we run with --timeout 30s to keep it practical
    let output = Command::new("cargo")
        .args(["mutants", "--timeout", "30", "--no-shuffle"])
        .current_dir(project_root)
        .output();

    match output {
        Ok(result) => {
            let output_str = String::from_utf8_lossy(&result.stdout);
            let mutation_score = parse_mutation_score(&output_str);

            let status = if let Some(score) = mutation_score {
                if score >= 95.0 {
                    LayerStatus::Pass
                } else {
                    LayerStatus::Partial
                }
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
                    message: "Mutation testing completed. Check .cargo/mutants/ for detailed results.".to_string(),
                    location: None,
                }]
            };

            LayerResult {
                name: "structural".to_string(),
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
            }
        }
        Err(e) => LayerResult {
            name: "structural".to_string(),
            status: LayerStatus::Fail,
            findings: vec![Finding {
                severity: Severity::Critical,
                code: "MUTATION_EXECUTION_FAILED".to_string(),
                message: format!("Failed to run cargo-mutants: {}", e),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 1,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn parse_mutation_score(output: &str) -> Option<f64> {
    // Simple parser for cargo-mutants output
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

fn run_hostile_layer(project: &crate::detect::ProjectInfo, start: Instant) -> LayerResult {
    let project_root = Path::new(&project.root);

    // Check if semgrep is available
    let semgrep_available = Command::new("semgrep")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !semgrep_available {
        return LayerResult {
            name: "hostile".to_string(),
            status: LayerStatus::Skipped,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "NO_SAST".to_string(),
                message: "SAST not available. Install `semgrep` for automated security scanning.".to_string(),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 0,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // Run semgrep with default rules (quick scan)
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

            LayerResult {
                name: "hostile".to_string(),
                status,
                findings,
                metrics: LayerMetrics {
                    tests_run: 0,
                    passed: 0,
                    failed: 0,
                    coverage: None,
                    mutation_score: None,
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        Err(e) => LayerResult {
            name: "hostile".to_string(),
            status: LayerStatus::Fail,
            findings: vec![Finding {
                severity: Severity::Critical,
                code: "SAST_EXECUTION_FAILED".to_string(),
                message: format!("Failed to run semgrep: {}", e),
                location: None,
            }],
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 1,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn parse_semgrep_findings(output: &str) -> Vec<Finding> {
    // Very simple parser — in production we'd use proper JSON deserialization
    let mut findings = Vec::new();

    if output.contains("\"results\":[]") || output.is_empty() {
        return findings;
    }

    // If there are results, create a generic finding
    if output.contains("\"results\"") {
        findings.push(Finding {
            severity: Severity::Medium,
            code: "SAST_FINDINGS".to_string(),
            message: "Semgrep found potential security issues. Run `semgrep .` for details.".to_string(),
            location: None,
        });
    }

    findings
}
