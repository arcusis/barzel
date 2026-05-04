use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use crate::runners::python_venv::venv_tool;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct PytestRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for PytestRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for PytestRunner {
    fn name(&self) -> &'static str { "pytest" }
    fn layer(&self) -> Layer { Layer::Logic }

    fn skip_message(&self) -> &'static str {
        "pytest not found — add `pytest` and optionally `hypothesis` to dev-dependencies for property-based testing"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Python { return false; }
        let root = Path::new(&project.root);
        if venv_tool(root, "pytest").is_some() { return true; }
        self.proc.is_available("pytest", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let pytest_cmd = venv_tool(root, "pytest").unwrap_or_else(|| "pytest".to_string());

        // Add --cov if pytest-cov is available in the environment
        let has_cov = has_pytest_cov(root);
        let args: Vec<&str> = if has_cov {
            vec!["--tb=short", "-q", "--color=no", "--cov=.", "--cov-report=term-missing:skip-covered"]
        } else {
            vec!["--tb=short", "-q", "--color=no"]
        };

        match self.proc.run(&pytest_cmd, &args, root) {
            Ok(out) => {
                let combined = out.combined();
                let (passed, failed, errors) = parse_pytest_output(&combined);
                let total = passed + failed + errors;
                let coverage = if has_cov { parse_coverage_pct(&combined) } else { None };

                let status = if out.success {
                    LayerStatus::Pass
                } else {
                    LayerStatus::Fail
                };

                let findings = if out.success {
                    // Check if hypothesis is being used (adds extra PBT value)
                    if !has_hypothesis(root) {
                        vec![Finding {
                            severity: Severity::Info,
                            code: "NO_HYPOTHESIS".to_string(),
                            message: "Tests pass but no Hypothesis property-based tests detected. \
                                      Add `hypothesis` for invariant testing."
                                .to_string(),
                            reproduce_cmd: Some(format!("pip install hypothesis && {pytest_cmd} --hypothesis-seed=0")),
                            suggestion: Some(
                                "Use `from hypothesis import given, strategies as st` \
                                 to add property-based tests that find edge cases automatically."
                                    .to_string(),
                            ),
                            ..Default::default()
                        }]
                    } else {
                        vec![]
                    }
                } else {
                    let failing = extract_pytest_failures(&combined);
                    vec![Finding {
                        severity: Severity::High,
                        code: "PYTEST_FAILURE".to_string(),
                        message: format!(
                            "{} test(s) failed out of {}{}",
                            failed + errors,
                            total,
                            if failing.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", failing.join(", "))
                            }
                        ),
                        reproduce_cmd: Some(format!("{pytest_cmd} -v 2>&1 | head -60")),
                        suggestion: Some(
                            "Run `pytest -v --tb=long` to see the full failure trace."
                                .to_string(),
                        ),
                        ..Default::default()
                    }]
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "pytest".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: total,
                        passed,
                        failed: failed + errors,
                        coverage,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "pytest".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "PYTEST_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run pytest: {}", e),
                    reproduce_cmd: Some("pytest --version 2>&1".to_string()),
                    suggestion: Some(
                        "Install pytest: `pip install pytest` or `uv add --dev pytest`. \
                         Consider using a virtual environment: `python -m venv .venv && .venv/bin/pip install pytest`"
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

fn has_pytest_cov(root: &Path) -> bool {
    for file in &["requirements.txt", "requirements-dev.txt", "pyproject.toml"] {
        if let Ok(content) = std::fs::read_to_string(root.join(file)) {
            if content.contains("pytest-cov") { return true; }
        }
    }
    // Also check if pytest-cov is installed in venv
    root.join(".venv").join("lib").exists()
        && venv_tool(root, "pytest").is_some()
        && std::fs::read_dir(root.join(".venv").join("lib"))
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .map(|site| site.path().join("site-packages").join("pytest_cov").exists())
            .unwrap_or(false)
}

/// Parse `TOTAL ... 85%` line from pytest-cov output.
pub fn parse_coverage_pct(output: &str) -> Option<f64> {
    for line in output.lines().rev() {
        let t = line.trim();
        if t.starts_with("TOTAL") {
            // "TOTAL   1234   200   84%"
            let last = t.split_whitespace().last()?;
            let pct_str = last.trim_end_matches('%');
            return pct_str.parse().ok();
        }
    }
    None
}

fn has_hypothesis(root: &Path) -> bool {
    // Check requirements files
    for file in &["requirements.txt", "requirements-dev.txt", "pyproject.toml"] {
        if let Ok(content) = std::fs::read_to_string(root.join(file)) {
            if content.contains("hypothesis") { return true; }
        }
    }
    // Check if hypothesis is imported in any test file
    if walk_contains(root, "from hypothesis", ".py") || walk_contains(root, "import hypothesis", ".py") {
        return true;
    }
    false
}

fn walk_contains(dir: &Path, pattern: &str, ext: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if walk_contains(&path, pattern, ext) { return true; }
        } else if path.to_string_lossy().ends_with(ext) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains(pattern) { return true; }
            }
        }
    }
    false
}

pub fn parse_pytest_output(output: &str) -> (u64, u64, u64) {
    // pytest summary line: "5 passed, 2 failed, 1 error in 0.45s"
    // or: "5 passed in 0.45s"
    for line in output.lines().rev() {
        let line = line.trim();
        if line.contains(" passed") || line.contains(" failed") || line.contains(" error") {
            let passed = count_before(line, " passed");
            let failed = count_before(line, " failed");
            let errors = count_before(line, " error");
            if passed + failed + errors > 0 {
                return (passed, failed, errors);
            }
        }
    }
    (0, 0, 0)
}

fn count_before(line: &str, label: &str) -> u64 {
    if let Some(idx) = line.find(label) {
        let before = line[..idx].trim_end();
        if let Some(tok) = before.split_whitespace().last() {
            return tok.trim_matches(',').parse().unwrap_or(0);
        }
    }
    0
}

fn extract_pytest_failures(output: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.starts_with("FAILED ") {
            let name = t.trim_start_matches("FAILED ").trim();
            if !name.is_empty() { failures.push(name.to_string()); }
        }
    }
    failures.truncate(3);
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;

    fn py_info() -> ProjectInfo {
        ProjectInfo { language: Language::Python, root: "/tmp".to_string(), has_tests: true, package_name: None, frameworks: Default::default(), workspace_root: None }
    }

    fn runner_with(mock: MockProcessRunner) -> PytestRunner {
        PytestRunner { proc: Arc::new(mock) }
    }

    #[test]
    fn name_is_pytest() { assert_eq!(PytestRunner::default().name(), "pytest"); }

    #[test]
    fn layer_is_logic() { assert!(matches!(PytestRunner::default().layer(), Layer::Logic)); }

    #[test]
    fn not_available_for_rust() {
        let r = PytestRunner { proc: Arc::new(MockProcessRunner::passing("")) };
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn available_via_windows_scripts_exe() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join(".venv").join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("pytest.exe"), b"").unwrap();
        let info = ProjectInfo {
            language: Language::Python,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        let r = PytestRunner { proc: Arc::new(MockProcessRunner::unavailable()) };
        assert!(r.is_available(&info), "pytest.exe in .venv/Scripts must make runner available");
    }

    #[test]
    fn run_uses_windows_venv_pytest_exe() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join(".venv").join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("pytest.exe"), b"").unwrap();
        let info = ProjectInfo {
            language: Language::Python,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };

        struct CapturingProc;
        impl SubprocessRunner for CapturingProc {
            fn run(&self, cmd: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                assert!(cmd.contains("Scripts") && cmd.ends_with("pytest.exe"),
                    "run() must use .venv/Scripts/pytest.exe on Windows layout, got: {cmd}");
                Ok(crate::process::ProcessOutput { stdout: "1 passed in 0.1s".to_string(), stderr: String::new(), success: true })
            }
        }
        let r = PytestRunner { proc: Arc::new(CapturingProc) };
        r.run(&info).unwrap();
    }

    #[test]
    fn run_pass_parses_counts() {
        let stdout = "5 passed in 0.45s";
        let result = runner_with(MockProcessRunner::passing(stdout)).run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert_eq!(result.metrics.passed, 5);
        assert_eq!(result.metrics.failed, 0);
    }

    #[test]
    fn run_fail_parses_counts() {
        let stdout = "3 passed, 2 failed in 0.45s\nFAILED test_foo.py::test_bar";
        let result = runner_with(MockProcessRunner::failing(stdout)).run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].message.contains("2"));
    }

    #[test]
    fn run_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "pytest not found"))
            }
        }
        let r = PytestRunner { proc: Arc::new(BrokenProc) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn parses_passed_only() {
        let (passed, failed, errors) = parse_pytest_output("5 passed in 0.12s");
        assert_eq!(passed, 5); assert_eq!(failed, 0); assert_eq!(errors, 0);
    }

    #[test]
    fn parses_mixed_results() {
        let (passed, failed, errors) = parse_pytest_output("3 passed, 2 failed, 1 error in 0.45s");
        assert_eq!(passed, 3); assert_eq!(failed, 2); assert_eq!(errors, 1);
    }

    #[test]
    fn parse_coverage_pct_extracts_total_line() {
        let output = "Name    Stmts Miss  Cover\n---\napp.py   100    15    85%\nTOTAL    200    30    85%";
        let pct = parse_coverage_pct(output);
        assert_eq!(pct, Some(85.0));
    }

    #[test]
    fn parse_coverage_pct_returns_none_when_missing() {
        let pct = parse_coverage_pct("5 passed in 0.45s");
        assert!(pct.is_none());
    }

    proptest! {
        #[test]
        fn parse_pytest_never_panics(s in ".*") { let _ = parse_pytest_output(&s); }

        #[test]
        fn parse_coverage_never_panics(s in ".*") { let _ = parse_coverage_pct(&s); }
    }
}
