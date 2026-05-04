use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct FastCheckRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for FastCheckRunner {
    fn default() -> Self {
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for FastCheckRunner {
    fn name(&self) -> &'static str {
        "fast-check"
    }

    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn skip_message(&self) -> &'static str {
        "No fast-check tests found — add `fast-check` to dev-dependencies for property-based testing"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        let root = Path::new(&project.root);
        let pkg = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
        pkg.contains("fast-check")
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
        let pm = detect_package_manager(root);
        let test_cmd = detect_test_command(&pkg_json, root, pm);

        match self.proc.run("sh", &["-c", &test_cmd], root) {
            Ok(out) => {
                let combined = out.combined();

                let status = if out.success {
                    LayerStatus::Pass
                } else {
                    LayerStatus::Fail
                };
                let (tests_run, tests_passed, tests_failed) = parse_ts_test_counts(&combined);

                let findings = if out.success {
                    vec![]
                } else {
                    let failing_tests = extract_failing_tests(&combined);
                    vec![Finding {
                        severity: Severity::High,
                        code: "PBT_FAILURE".to_string(),
                        message: format!("Property-based tests failed ({} failed)", tests_failed),
                        reproduce_cmd: Some(format!("{} test 2>&1 | head -80", pm)),
                        suggestion: if failing_tests.is_empty() {
                            Some("Run the test suite and check the property that failed. Examine the counterexample reported by fast-check and tighten your invariant.".to_string())
                        } else {
                            Some(format!(
                                "Failing: {}. Examine the counterexample and fix the invariant.",
                                failing_tests.join(", ")
                            ))
                        },
                        ..Default::default()
                    }]
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "fast-check".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run,
                        passed: tests_passed,
                        failed: tests_failed,
                        coverage: None,
                        mutation_score: None,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "fast-check".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "TEST_RUNNER_ERROR".to_string(),
                    message: format!("Failed to run test suite: {}", e),
                    reproduce_cmd: Some(format!("{} test", pm)),
                    suggestion: Some(
                        "Ensure node_modules are installed: run `pnpm install` (or npm install)."
                            .to_string(),
                    ),
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

pub fn detect_package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    }
}

fn detect_test_command(pkg_json: &str, root: &Path, pm: &str) -> String {
    // Prefer vitest if available
    if pkg_json.contains("vitest") {
        let vitest_bin = root.join("node_modules").join(".bin").join("vitest");
        if vitest_bin.exists() {
            return format!("{} exec vitest run", pm);
        }
        return "npx vitest run".to_string();
    }
    // Jest
    if pkg_json.contains("jest") {
        return format!("{} exec jest --passWithNoTests 2>&1", pm);
    }
    // Default: use the "test" script
    format!("{} test", pm)
}

fn parse_ts_test_counts(output: &str) -> (u64, u64, u64) {
    // Vitest: "✓ 5 tests" or "Test Files  1 passed (1)"
    // Jest:   "Tests: 2 passed, 5 total"
    for line in output.lines() {
        // Jest format
        if line.trim_start().starts_with("Tests:") {
            let passed = extract_label_count(line, "passed");
            let failed = extract_label_count(line, "failed");
            let total = extract_label_count(line, "total").max(passed + failed);
            return (total, passed, failed);
        }
        // Vitest format: "Tests  5 passed (5)"
        if line.contains(" passed") && (line.contains("Tests") || line.contains("test")) {
            let passed = extract_before_label(line, " passed");
            let failed = extract_before_label(line, " failed");
            let total = passed + failed;
            if total > 0 {
                return (total, passed, failed);
            }
        }
    }
    (1, 1, 0)
}

fn extract_label_count(line: &str, label: &str) -> u64 {
    // For "2 passed": find "passed", look backward for number
    if let Some(idx) = line.find(label) {
        let before = &line[..idx];
        if let Some(num_str) = before.split_whitespace().last() {
            return num_str.parse().unwrap_or(0);
        }
    }
    0
}

fn extract_before_label(line: &str, label: &str) -> u64 {
    extract_label_count(line, label)
}

fn extract_failing_tests(output: &str) -> Vec<String> {
    let mut tests = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        // Jest/Vitest failing test patterns
        if t.starts_with("● ") || t.starts_with("× ") || t.starts_with("FAIL") {
            let name = t.trim_start_matches(['●', '×', ' ']).trim();
            if !name.is_empty() && name.len() < 100 {
                tests.push(name.to_string());
            }
        }
    }
    tests.truncate(3);
    tests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn ts_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    fn runner_with(mock: MockProcessRunner) -> FastCheckRunner {
        FastCheckRunner {
            proc: Arc::new(mock),
        }
    }

    // ── metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn name_is_fast_check() {
        assert_eq!(FastCheckRunner::default().name(), "fast-check");
    }

    #[test]
    fn layer_is_logic() {
        assert!(matches!(FastCheckRunner::default().layer(), Layer::Logic));
    }

    #[test]
    fn skip_message_mentions_fast_check() {
        assert!(FastCheckRunner::default()
            .skip_message()
            .contains("fast-check"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn not_available_for_rust() {
        let dir = tempdir().unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(!FastCheckRunner::default().is_available(&info));
    }

    #[test]
    fn available_when_fast_check_in_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"devDependencies":{"fast-check":"3.0"}}"#,
        )
        .unwrap();
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(FastCheckRunner::default().is_available(&info));
    }

    #[test]
    fn not_available_when_fast_check_missing_from_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"devDependencies":{}}"#,
        )
        .unwrap();
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(!FastCheckRunner::default().is_available(&info));
    }

    // ── run() ─────────────────────────────────────────────────────────────────

    #[test]
    fn run_returns_pass_on_success() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"devDependencies":{"jest":"29"}}"#,
        )
        .unwrap();
        let stdout = "Tests: 5 passed, 5 total";
        let result = runner_with(MockProcessRunner::passing(stdout))
            .run(&ts_info(&dir.path().to_string_lossy()))
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_returns_fail_on_test_failure() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"devDependencies":{"jest":"29"}}"#,
        )
        .unwrap();
        let stdout = "Tests: 3 passed, 2 failed, 5 total";
        let result = runner_with(MockProcessRunner::failing(stdout))
            .run(&ts_info(&dir.path().to_string_lossy()))
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::High);
    }

    #[test]
    fn run_returns_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(
                &self,
                _: &str,
                _: &[&str],
                _: &Path,
            ) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "sh not found",
                ))
            }
        }
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), b"{}").unwrap();
        let runner = FastCheckRunner {
            proc: Arc::new(BrokenProc),
        };
        let result = runner.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    // ── detect_package_manager ────────────────────────────────────────────────

    #[test]
    fn detects_pnpm_from_lockfile() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"lockfileVersion: '9.0'").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "pnpm");
    }

    #[test]
    fn detects_yarn_from_lockfile() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"# yarn lockfile v1").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "yarn");
    }

    #[test]
    fn defaults_to_npm() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_package_manager(dir.path()), "npm");
    }

    #[test]
    fn pnpm_takes_priority_over_yarn() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "pnpm");
    }

    // ── parse_ts_test_counts ─────────────────────────────────────────────────

    #[test]
    fn parses_jest_format() {
        let output = "Tests: 5 passed, 2 failed, 7 total";
        let (total, passed, failed) = parse_ts_test_counts(output);
        assert_eq!(passed, 5);
        assert_eq!(failed, 2);
        assert_eq!(total, 7);
    }

    #[test]
    fn parses_vitest_tests_line() {
        let output = "Tests  8 passed (8)";
        let (total, passed, _failed) = parse_ts_test_counts(output);
        assert_eq!(passed, 8);
        assert_eq!(total, 8);
    }

    #[test]
    fn fallback_returns_one_passed() {
        let (total, passed, failed) = parse_ts_test_counts("no test output");
        assert_eq!(total, 1);
        assert_eq!(passed, 1);
        assert_eq!(failed, 0);
    }

    // ── extract_label_count ───────────────────────────────────────────────────

    #[test]
    fn extracts_count_correctly() {
        assert_eq!(extract_label_count("5 passed", "passed"), 5);
        assert_eq!(extract_label_count("10 failed", "failed"), 10);
        assert_eq!(extract_label_count("no match here", "passed"), 0);
    }

    // ── extract_failing_tests ─────────────────────────────────────────────────

    #[test]
    fn extracts_jest_bullet_failures() {
        let output = "● MyTest > should work\n● AnotherTest > fails";
        let tests = extract_failing_tests(output);
        assert_eq!(tests.len(), 2);
        assert!(tests[0].contains("MyTest"));
    }

    #[test]
    fn limits_to_three_failures() {
        let output = "● A\n● B\n● C\n● D\n● E";
        let tests = extract_failing_tests(output);
        assert_eq!(tests.len(), 3);
    }

    #[test]
    fn returns_empty_for_passing_output() {
        let output = "✓ all tests pass\n✓ more passing";
        let tests = extract_failing_tests(output);
        assert!(tests.is_empty());
    }

    proptest! {
        #[test]
        fn parse_ts_test_counts_never_panics(s in ".*") {
            let _ = parse_ts_test_counts(&s);
        }

        #[test]
        fn extract_label_count_never_panics(line in ".*", label in "[a-z]+") {
            let _ = extract_label_count(&line, &label);
        }

        #[test]
        fn extract_failing_tests_never_panics(s in ".*") {
            let _ = extract_failing_tests(&s);
        }
    }
}
