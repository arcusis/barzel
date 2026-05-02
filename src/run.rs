use crate::detect::detect_project;
use crate::error::Result;
use crate::report::{BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
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

    let mut report = BarzelReport::new(project.clone());
    let _start = Instant::now();

    let enabled_layers = layers.unwrap_or_else(|| {
        vec!["logic".to_string(), "structural".to_string(), "hostile".to_string()]
    });

    for layer_name in &enabled_layers {
        let layer_start = Instant::now();

        let layer_result = match layer_name.as_str() {
            "logic" => run_logic_layer(&project, layer_start),
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

fn run_structural_layer(_project: &crate::detect::ProjectInfo, start: Instant) -> LayerResult {
    LayerResult {
        name: "structural".to_string(),
        status: LayerStatus::Partial,
        findings: vec![Finding {
            severity: Severity::Info,
            code: "STRUCTURAL_STUB".to_string(),
            message: "Structural layer (Mutation Testing + MC/DC) not yet implemented — coming in M2".to_string(),
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
    }
}

fn run_hostile_layer(_project: &crate::detect::ProjectInfo, start: Instant) -> LayerResult {
    LayerResult {
        name: "hostile".to_string(),
        status: LayerStatus::Partial,
        findings: vec![Finding {
            severity: Severity::Info,
            code: "HOSTILE_STUB".to_string(),
            message: "Hostile layer (Fuzzing + SAST) not yet implemented — coming in M2".to_string(),
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
    }
}
