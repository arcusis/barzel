use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct CargoFuzzRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for CargoFuzzRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl CargoFuzzRunner {
    fn list_fuzz_targets(&self, root: &Path) -> Vec<String> {
        match self.proc.run("cargo", &["fuzz", "list"], root) {
            Ok(out) if out.success => out
                .stdout
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            // Fallback: scan fuzz/fuzz_targets/ directory
            _ => scan_target_directory(root),
        }
    }
}

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
        self.proc.is_available("cargo", &["fuzz", "--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        // List available fuzz targets
        let targets = self.list_fuzz_targets(root);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn rust_info(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::Rust, root: root.to_string(), has_tests: true, package_name: None, frameworks: Default::default() }
    }

    fn runner_with(mock: MockProcessRunner) -> CargoFuzzRunner {
        CargoFuzzRunner { proc: Arc::new(mock) }
    }

    // ── runner metadata ───────────────────────────────────────────────────────

    #[test]
    fn name_is_cargo_fuzz() {
        assert_eq!(CargoFuzzRunner::default().name(), "cargo-fuzz");
    }

    #[test]
    fn layer_is_hostile() {
        assert!(matches!(CargoFuzzRunner::default().layer(), crate::plugin::Layer::Hostile));
    }

    #[test]
    fn skip_message_nonempty() {
        assert!(!CargoFuzzRunner::default().skip_message().is_empty());
        assert!(CargoFuzzRunner::default().skip_message().contains("fuzz"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn not_available_for_go() {
        let dir = tempdir().unwrap();
        let info = ProjectInfo { language: Language::Go, root: dir.path().to_string_lossy().to_string(), has_tests: false, package_name: None, frameworks: Default::default() };
        assert!(!CargoFuzzRunner::default().is_available(&info));
    }

    #[test]
    fn not_available_when_no_fuzz_dir() {
        let dir = tempdir().unwrap();
        let info = rust_info(&dir.path().to_string_lossy());
        let r = runner_with(MockProcessRunner::passing("cargo-fuzz 0.12"));
        assert!(!r.is_available(&info));
    }

    #[test]
    fn not_available_when_cargo_fuzz_missing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("fuzz")).unwrap();
        let info = rust_info(&dir.path().to_string_lossy());
        let r = runner_with(MockProcessRunner::unavailable());
        assert!(!r.is_available(&info));
    }

    #[test]
    fn available_when_fuzz_dir_exists_and_cargo_fuzz_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("fuzz")).unwrap();
        let info = rust_info(&dir.path().to_string_lossy());
        let r = runner_with(MockProcessRunner::passing("cargo-fuzz 0.12"));
        assert!(r.is_available(&info));
    }

    // ── run() with mock ───────────────────────────────────────────────────────

    #[test]
    fn run_returns_pass_with_targets_and_no_crashes() {
        let dir = tempdir().unwrap();
        // Set up fuzz targets dir for fallback
        let targets_dir = dir.path().join("fuzz").join("fuzz_targets");
        std::fs::create_dir_all(&targets_dir).unwrap();
        std::fs::write(targets_dir.join("fuzz_json.rs"), b"#![no_main]").unwrap();

        // Mock returns the target list from `cargo fuzz list`
        let result = runner_with(MockProcessRunner::passing("fuzz_json"))
            .run(&rust_info(&dir.path().to_string_lossy()))
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
    }

    #[test]
    fn run_returns_fail_when_crash_artifacts_exist() {
        let dir = tempdir().unwrap();
        let crash_dir = dir.path().join("fuzz").join("artifacts").join("fuzz_target_1");
        std::fs::create_dir_all(&crash_dir).unwrap();
        std::fs::write(crash_dir.join("crash-deadbeef"), b"\x00").unwrap();

        let result = runner_with(MockProcessRunner::passing("fuzz_target_1"))
            .run(&rust_info(&dir.path().to_string_lossy()))
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn run_returns_skipped_when_no_targets() {
        let dir = tempdir().unwrap();
        // Mock returns empty list; no fallback targets either
        let result = runner_with(MockProcessRunner::passing(""))
            .run(&rust_info(&dir.path().to_string_lossy()))
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Skipped));
    }

    // ── list_fuzz_targets (via method) ────────────────────────────────────────

    #[test]
    fn list_uses_cargo_fuzz_output_when_successful() {
        let dir = tempdir().unwrap();
        let r = runner_with(MockProcessRunner::passing("fuzz_json\nfuzz_http\n"));
        let targets = r.list_fuzz_targets(dir.path());
        assert_eq!(targets, vec!["fuzz_json", "fuzz_http"]);
    }

    #[test]
    fn list_falls_back_to_directory_scan_on_failure() {
        let dir = tempdir().unwrap();
        let targets_dir = dir.path().join("fuzz").join("fuzz_targets");
        std::fs::create_dir_all(&targets_dir).unwrap();
        std::fs::write(targets_dir.join("fuzz_json.rs"), b"#![no_main]").unwrap();

        let r = runner_with(MockProcessRunner::failing(""));
        let targets = r.list_fuzz_targets(dir.path());
        assert_eq!(targets, vec!["fuzz_json"]);
    }

    // ── infer_target_from_artifact ────────────────────────────────────────────

    #[test]
    fn infers_target_name_from_path() {
        let path = "/project/fuzz/artifacts/fuzz_target_1/crash-abc123";
        assert_eq!(infer_target_from_artifact(path), "fuzz_target_1");
    }

    #[test]
    fn fallback_for_root_path() {
        assert_eq!(infer_target_from_artifact("crash"), "fuzz_target_1");
    }

    #[test]
    fn fallback_for_empty_path() {
        assert_eq!(infer_target_from_artifact(""), "fuzz_target_1");
    }

    // ── scan_target_directory ─────────────────────────────────────────────────

    #[test]
    fn finds_rust_fuzz_targets() {
        let dir = tempdir().unwrap();
        let targets_dir = dir.path().join("fuzz").join("fuzz_targets");
        std::fs::create_dir_all(&targets_dir).unwrap();
        std::fs::write(targets_dir.join("fuzz_json.rs"), b"#![no_main]").unwrap();
        std::fs::write(targets_dir.join("fuzz_http.rs"), b"#![no_main]").unwrap();
        std::fs::write(targets_dir.join("README.md"), b"docs").unwrap(); // should be ignored

        let targets = scan_target_directory(dir.path());
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&"fuzz_json".to_string()));
        assert!(targets.contains(&"fuzz_http".to_string()));
    }

    #[test]
    fn returns_empty_when_no_targets_dir() {
        let dir = tempdir().unwrap();
        let targets = scan_target_directory(dir.path());
        assert!(targets.is_empty());
    }

    // ── find_crash_artifacts ──────────────────────────────────────────────────

    #[test]
    fn finds_crash_files_in_artifacts_dir() {
        let dir = tempdir().unwrap();
        let crash_dir = dir.path().join("fuzz").join("artifacts").join("fuzz_target_1");
        std::fs::create_dir_all(&crash_dir).unwrap();
        std::fs::write(crash_dir.join("crash-deadbeef"), b"\x00\x01\x02").unwrap();
        std::fs::write(crash_dir.join(".gitignore"), b"*").unwrap(); // should be ignored
        std::fs::write(crash_dir.join("README.md"), b"docs").unwrap(); // should be ignored

        let crashes = find_crash_artifacts(dir.path());
        assert_eq!(crashes.len(), 1);
        assert!(crashes[0].contains("crash-deadbeef"));
    }

    #[test]
    fn returns_empty_when_no_artifacts_dir() {
        let dir = tempdir().unwrap();
        let crashes = find_crash_artifacts(dir.path());
        assert!(crashes.is_empty());
    }

    #[test]
    fn returns_empty_when_no_crash_files() {
        let dir = tempdir().unwrap();
        let crash_dir = dir.path().join("fuzz").join("artifacts").join("target_1");
        std::fs::create_dir_all(&crash_dir).unwrap();
        std::fs::write(crash_dir.join(".gitignore"), b"*").unwrap();
        let crashes = find_crash_artifacts(dir.path());
        assert!(crashes.is_empty());
    }
}
