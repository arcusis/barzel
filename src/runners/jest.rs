use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct JestRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for JestRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

/// Detect which test runner the project uses: vitest or jest.
/// Only returns a command when the binary is present in node_modules/.bin.
pub fn detect_js_test_command(root: &Path) -> Option<String> {
    let bin = root.join("node_modules").join(".bin");

    // vitest is preferred — faster and native ESM
    if bin.join("vitest").exists() {
        return Some("./node_modules/.bin/vitest run --reporter=verbose 2>&1".to_string());
    }

    // jest
    if bin.join("jest").exists() {
        return Some("./node_modules/.bin/jest --no-coverage 2>&1".to_string());
    }

    None
}

impl TestRunner for JestRunner {
    fn name(&self) -> &'static str { "jest" }
    fn layer(&self) -> Layer { Layer::Logic }

    fn skip_message(&self) -> &'static str {
        "No jest/vitest found — add jest or vitest to devDependencies and define a test script in package.json"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        detect_js_test_command(Path::new(&project.root)).is_some()
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let cmd = match detect_js_test_command(root) {
            Some(c) => c,
            None => {
                return Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "jest".to_string(),
                    status: LayerStatus::Skipped,
                    findings: vec![Finding {
                        severity: Severity::Info,
                        code: "NO_JS_TEST_RUNNER".to_string(),
                        message: self.skip_message().to_string(),
                        ..Default::default()
                    }],
                    metrics: LayerMetrics::default(),
                    duration_ms: 0,
                });
            }
        };

        match self.proc.run("sh", &["-c", &cmd], root) {
            Ok(out) => {
                let combined = out.combined();
                let (passed, failed, total) = parse_js_test_output(&combined);
                let status = if out.success { LayerStatus::Pass } else { LayerStatus::Fail };

                let findings = if out.success {
                    vec![]
                } else {
                    let failing = extract_js_failures(&combined);
                    vec![Finding {
                        severity: Severity::High,
                        code: "TEST_FAILURE".to_string(),
                        message: format!(
                            "{} test(s) failed{}",
                            failed,
                            if failing.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", failing.join(", "))
                            }
                        ),
                        reproduce_cmd: Some(cmd.clone()),
                        suggestion: Some(
                            "Run tests with --verbose to see full failure output. \
                             Check for missing mocks, broken imports, or environment issues."
                                .to_string(),
                        ),
                        ..Default::default()
                    }]
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "jest".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: total,
                        passed,
                        failed,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "jest".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "TEST_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run JS tests: {}", e),
                    reproduce_cmd: Some(cmd),
                    suggestion: Some(
                        "Ensure node_modules is installed: `npm install` or `pnpm install`."
                            .to_string(),
                    ),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_js_test_output(output: &str) -> (u64, u64, u64) {
    // vitest: "✓ 12 | ✗ 2 | ↓ 0" or "Tests  12 passed | 2 failed"
    // jest:   "Tests: 12 passed, 2 failed, 14 total"
    for line in output.lines() {
        let l = line.trim();

        // Jest format: "Tests: X passed, Y failed, Z total"
        if l.starts_with("Tests:") {
            let passed = count_before_label(l, " passed");
            let failed = count_before_label(l, " failed");
            let total = count_before_label(l, " total");
            if total > 0 { return (passed, failed, total); }
        }

        // Vitest format: "Tests  X passed (Y)"
        if l.contains("passed") && l.contains("Tests") {
            let passed = count_before_label(l, " passed");
            let failed = count_before_label(l, " failed");
            return (passed, failed, passed + failed);
        }

        // Jest summary: "X tests passed"
        if l.contains(" tests passed") || l.contains(" test passed") {
            let n = l.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            return (n, 0, n);
        }
    }
    (0, 0, 0)
}

fn count_before_label(line: &str, label: &str) -> u64 {
    if let Some(idx) = line.find(label) {
        let before = line[..idx].trim_end();
        if let Some(tok) = before.split_whitespace().last() {
            return tok.trim_matches(',').parse().unwrap_or(0);
        }
    }
    0
}

fn extract_js_failures(output: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        // jest: "● test name"
        if t.starts_with("● ") {
            let name = t.trim_start_matches("● ").trim();
            if !name.is_empty() && !name.starts_with("Console") {
                failures.push(name.to_string());
            }
        }
        // vitest: "FAIL src/..."  or "× test name"
        if t.starts_with("× ") || t.starts_with("✗ ") {
            let name = t.trim_start_matches(['×', '✗', ' ']).trim();
            if !name.is_empty() { failures.push(name.to_string()); }
        }
    }
    failures.truncate(3);
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn ts_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: ProjectFrameworks { is_nextjs: true, ..Default::default() },
            workspace_root: None,
        }
    }

    fn runner_with(mock: MockProcessRunner) -> JestRunner {
        JestRunner { proc: Arc::new(mock) }
    }

    fn setup_jest_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            br#"{"devDependencies":{"jest":"^29"}}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/jest"), b"#!/bin/sh\necho hi").unwrap();
        dir
    }

    #[test]
    fn name_is_jest() { assert_eq!(JestRunner::default().name(), "jest"); }

    #[test]
    fn layer_is_logic() { assert!(matches!(JestRunner::default().layer(), Layer::Logic)); }

    #[test]
    fn not_available_for_rust() {
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!JestRunner::default().is_available(&i));
    }

    #[test]
    fn available_when_jest_in_node_modules() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            br#"{"scripts":{"test":"jest"},"devDependencies":{"jest":"^29"}}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/jest"), b"").unwrap();
        let i = ts_info(&dir.path().to_string_lossy());
        assert!(JestRunner::default().is_available(&i));
    }

    #[test]
    fn available_when_vitest_in_node_modules() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            br#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"^1"}}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/vitest"), b"").unwrap();
        let i = ts_info(&dir.path().to_string_lossy());
        assert!(JestRunner::default().is_available(&i));
    }

    #[test]
    fn not_available_without_node_modules() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            br#"{"scripts":{"test":"jest"},"devDependencies":{"jest":"^29"}}"#).unwrap();
        // no node_modules/.bin/jest
        let i = ts_info(&dir.path().to_string_lossy());
        assert!(!JestRunner::default().is_available(&i));
    }

    #[test]
    fn run_pass_parses_jest_counts() {
        let dir = setup_jest_dir();
        let out = "Tests: 12 passed, 0 failed, 12 total\nTest Suites: 3 passed";
        let result = runner_with(MockProcessRunner::passing(out)).run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert_eq!(result.metrics.passed, 12);
        assert_eq!(result.metrics.failed, 0);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_fail_parses_counts_and_failure_names() {
        let dir = setup_jest_dir();
        let out = "● MyComponent › renders correctly\nTests: 10 passed, 2 failed, 12 total";
        let result = runner_with(MockProcessRunner::failing(out)).run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.failed, 2);
        assert!(result.findings[0].message.contains("2"));
    }

    #[test]
    fn run_fail_on_subprocess_error() {
        let dir = setup_jest_dir();
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "node not found"))
            }
        }
        let result = JestRunner { proc: Arc::new(BrokenProc) }.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn parse_jest_passed_and_failed() {
        let (p, f, t) = parse_js_test_output("Tests: 10 passed, 2 failed, 12 total");
        assert_eq!(p, 10); assert_eq!(f, 2); assert_eq!(t, 12);
    }

    #[test]
    fn parse_jest_passed_only() {
        let (p, f, t) = parse_js_test_output("Tests: 5 passed, 5 total");
        assert_eq!(p, 5); assert_eq!(f, 0); assert_eq!(t, 5);
    }

    #[test]
    fn parse_vitest_output() {
        let (p, f, _) = parse_js_test_output("Tests  12 passed (12)");
        assert_eq!(p, 12); assert_eq!(f, 0);
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".*") { let _ = parse_js_test_output(&s); }
    }
}
