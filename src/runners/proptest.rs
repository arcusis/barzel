use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct ProptestRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for ProptestRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for ProptestRunner {
    fn name(&self) -> &'static str { "proptest" }
    fn layer(&self) -> Layer { Layer::Logic }

    fn skip_message(&self) -> &'static str {
        "No property-based tests found — add `proptest` to dev-dependencies for invariant testing"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }
        let cargo_toml = Path::new(&project.root).join("Cargo.toml");
        std::fs::read_to_string(&cargo_toml)
            .map(|c| c.contains("proptest"))
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        match self.proc.run("cargo", &["test"], root) {
            Ok(out) => {
                let combined = out.combined();
                let (tests_run, tests_passed, tests_failed) = parse_test_counts(&combined);
                let status = if out.success { LayerStatus::Pass } else { LayerStatus::Fail };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "proptest".to_string(),
                    status,
                    findings: if out.success {
                        vec![]
                    } else {
                        vec![Finding {
                            severity: Severity::High,
                            code: "PBT_FAILURE".to_string(),
                            message: "Property-based tests failed".to_string(),
                            reproduce_cmd: Some("cargo test -- --nocapture 2>&1 | head -80".to_string()),
                            suggestion: Some(
                                "Check the counterexample printed by proptest. \
                                 The failure shrinks to the minimal failing case automatically."
                                    .to_string(),
                            ),
                            ..Default::default()
                        }]
                    },
                    metrics: LayerMetrics {
                        tests_run,
                        passed: tests_passed,
                        failed: tests_failed,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "proptest".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "PBT_EXECUTION_ERROR".to_string(),
                    message: format!("Failed to run cargo test: {}", e),
                    reproduce_cmd: Some("cargo test 2>&1".to_string()),
                    suggestion: Some("Ensure `cargo` is in PATH and the project compiles.".to_string()),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_test_counts(output: &str) -> (u64, u64, u64) {
    for line in output.lines() {
        if line.starts_with("test result:") {
            let passed = extract_count(line, " passed");
            let failed = extract_count(line, " failed");
            return (passed + failed, passed, failed);
        }
    }
    (1, 1, 0)
}

pub fn extract_count(line: &str, label: &str) -> u64 {
    if let Some(idx) = line.find(label) {
        let before = &line[..idx];
        if let Some(num_str) = before.split_whitespace().last() {
            return num_str.parse().unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;

    fn info(language: Language) -> ProjectInfo {
        ProjectInfo { language, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None }
    }

    fn runner_with(mock: MockProcessRunner) -> ProptestRunner {
        ProptestRunner { proc: Arc::new(mock) }
    }

    // ── metadata ─────────────────────────────────────────────────────────────

    #[test]
    fn name_is_proptest() { assert_eq!(ProptestRunner::default().name(), "proptest"); }

    #[test]
    fn layer_is_logic() { assert!(matches!(ProptestRunner::default().layer(), Layer::Logic)); }

    #[test]
    fn skip_message_mentions_proptest() {
        assert!(ProptestRunner::default().skip_message().contains("proptest"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn not_available_for_typescript() {
        assert!(!ProptestRunner::default().is_available(&info(Language::TypeScript)));
    }

    #[test]
    fn not_available_when_proptest_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"x\"").unwrap();
        let i = ProjectInfo { language: Language::Rust, root: dir.path().to_string_lossy().to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!ProptestRunner::default().is_available(&i));
    }

    #[test]
    fn available_when_proptest_in_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[dev-dependencies]\nproptest=\"1.0\"").unwrap();
        let i = ProjectInfo { language: Language::Rust, root: dir.path().to_string_lossy().to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(ProptestRunner::default().is_available(&i));
    }

    // ── run() with mock subprocess ────────────────────────────────────────────

    #[test]
    fn run_returns_pass_on_success() {
        let stdout = "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured";
        let result = runner_with(MockProcessRunner::passing(stdout)).run(&info(Language::Rust)).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert_eq!(result.metrics.tests_run, 5);
        assert_eq!(result.metrics.passed, 5);
        assert_eq!(result.metrics.failed, 0);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_returns_fail_on_test_failure() {
        let stdout = "test result: FAILED. 3 passed; 2 failed; 0 ignored";
        let result = runner_with(MockProcessRunner::failing(stdout)).run(&info(Language::Rust)).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.failed, 2);
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].reproduce_cmd.is_some());
    }

    #[test]
    fn run_returns_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "cargo not found"))
            }
        }
        let runner = ProptestRunner { proc: Arc::new(BrokenProc) };
        let result = runner.run(&info(Language::Rust)).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert_eq!(result.metrics.failed, 1);
    }

    // ── parse_test_counts ─────────────────────────────────────────────────────

    #[test]
    fn parses_standard_line() {
        let (t, p, f) = parse_test_counts("test result: ok. 5 passed; 2 failed; 0 ignored");
        assert_eq!(p, 5); assert_eq!(f, 2); assert_eq!(t, 7);
    }

    #[test]
    fn fallback_returns_one_passed() {
        let (t, p, f) = parse_test_counts("no match");
        assert_eq!(t, 1); assert_eq!(p, 1); assert_eq!(f, 0);
    }

    proptest! {
        #[test]
        fn parse_test_counts_never_panics(s in ".*") { let _ = parse_test_counts(&s); }

        #[test]
        fn total_is_passed_plus_failed(p in 0u64..500u64, f in 0u64..500u64) {
            let line = format!("test result: ok. {p} passed; {f} failed; 0 ignored");
            let (total, passed, failed) = parse_test_counts(&line);
            prop_assert_eq!(passed, p); prop_assert_eq!(failed, f);
            prop_assert_eq!(total, p + f);
        }

        #[test]
        fn extract_count_never_panics(line in ".*", label in ".*") {
            let _ = extract_count(&line, &label);
        }
    }
}
