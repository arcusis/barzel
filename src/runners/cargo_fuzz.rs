use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub struct CargoFuzzRunner;

impl TestRunner for CargoFuzzRunner {
    fn name(&self) -> &'static str {
        "cargo-fuzz"
    }

    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn skip_message(&self) -> &'static str {
        "No fuzz targets found — create a `fuzz/` directory and run \
         `cargo fuzz init` to enable continuous fuzzing (requires nightly)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }
        let root = Path::new(&project.root);
        // fuzz/ directory must exist with at least one target
        if !root.join("fuzz").exists() {
            return false;
        }
        // cargo-fuzz must be installed
        Command::new("cargo")
            .args(["fuzz", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        // List available fuzz targets
        let targets = list_fuzz_targets(root);

        // Scan for crash artifacts (existing crashes from previous runs)
        let crashes = find_crash_artifacts(root);

        let mut findings = Vec::new();

        // Any known crash is a critical finding
        for crash_path in &crashes {
            findings.push(Finding {
                severity: Severity::Critical,
                code: "FUZZ_CRASH_ARTIFACT".to_string(),
                message: format!("Crash artifact found: {} — a previous fuzz run triggered a crash", crash_path),
                reproduce_cmd: Some(format!(
                    "cargo fuzz run {} {} 2>&1",
                    infer_target_from_artifact(crash_path),
                    crash_path
                )),
                suggestion: Some(
                    "Replay the crash with the artifact file to reproduce, then fix the root cause. \
                     Delete the artifact once fixed to clear this finding."
                        .to_string(),
                ),
                ..Default::default()
            });
        }

        // Inform about available targets
        if !targets.is_empty() && crashes.is_empty() {
            findings.push(Finding {
                severity: Severity::Info,
                code: "FUZZ_TARGETS_AVAILABLE".to_string(),
                message: format!(
                    "{} fuzz target(s) available: {} — no crashes found",
                    targets.len(),
                    targets.join(", ")
                ),
                reproduce_cmd: Some(format!(
                    "cargo fuzz run {} -- -max_total_time=60 2>&1",
                    targets.first().map(String::as_str).unwrap_or("fuzz_target_1")
                )),
                suggestion: Some(
                    "Run fuzzing in CI with a time budget: \
                     `cargo fuzz run <target> -- -max_total_time=300`"
                        .to_string(),
                ),
                ..Default::default()
            });
        }

        let status = if !crashes.is_empty() {
            LayerStatus::Fail
        } else if targets.is_empty() {
            LayerStatus::Skipped
        } else {
            LayerStatus::Pass
        };

        Ok(LayerResult {
            name: "hostile".to_string(),
            runner: "cargo-fuzz".to_string(),
            status,
            findings,
            metrics: LayerMetrics {
                tests_run: targets.len() as u64,
                failed: crashes.len() as u64,
                ..Default::default()
            },
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn list_fuzz_targets(root: &Path) -> Vec<String> {
    // cargo fuzz list is the canonical way
    let output = Command::new("cargo")
        .args(["fuzz", "list"])
        .current_dir(root)
        .output();

    match output {
        Ok(result) if result.status.success() => {
            String::from_utf8_lossy(&result.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        }
        // Fallback: scan fuzz/fuzz_targets/ directory
        _ => scan_target_directory(root),
    }
}

fn scan_target_directory(root: &Path) -> Vec<String> {
    let targets_dir = root.join("fuzz").join("fuzz_targets");
    let Ok(entries) = std::fs::read_dir(&targets_dir) else {
        return vec![];
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().map(|x| x == "rs").unwrap_or(false) {
                path.file_stem().map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect()
}

fn find_crash_artifacts(root: &Path) -> Vec<String> {
    let artifacts_dir = root.join("fuzz").join("artifacts");
    let Ok(entries) = std::fs::read_dir(&artifacts_dir) else {
        return vec![];
    };

    let mut crashes = Vec::new();
    for target_entry in entries.flatten() {
        if !target_entry.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(target_entry.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().to_string();
            // Skip .gitignore and README files
            if name.starts_with('.') || name.ends_with(".md") {
                continue;
            }
            crashes.push(file.path().to_string_lossy().to_string());
        }
    }
    crashes
}

fn infer_target_from_artifact(artifact_path: &str) -> String {
    // fuzz/artifacts/<target_name>/crash-... → target_name
    Path::new(artifact_path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "fuzz_target_1".to_string())
}
