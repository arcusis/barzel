/// mypy type checker runner for Python projects.
/// Python AI apps frequently use dynamic typing that breaks at runtime —
/// mypy catches these issues statically before they reach production.
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct MypyRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for MypyRunner {
    fn default() -> Self {
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for MypyRunner {
    fn name(&self) -> &'static str {
        "mypy"
    }
    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn skip_message(&self) -> &'static str {
        "mypy not installed — run `pip install mypy` for Python static type checking (critical for AI apps)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Python {
            return false;
        }
        let root = Path::new(&project.root);
        let local = root.join(".venv").join("bin").join("mypy");
        if local.exists() {
            return true;
        }
        self.proc.is_available("mypy", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let local = root.join(".venv").join("bin").join("mypy");
        let mypy_cmd = if local.exists() {
            local.to_string_lossy().to_string()
        } else {
            "mypy".to_string()
        };

        match self.proc.run(
            &mypy_cmd,
            &[".", "--ignore-missing-imports", "--no-error-summary"],
            root,
        ) {
            Ok(out) => {
                let combined = out.combined();
                let errors = parse_mypy_errors(&combined);
                let error_count = errors.len() as u64;

                let status = if out.success {
                    LayerStatus::Pass
                } else {
                    LayerStatus::Fail
                };

                let findings = if out.success {
                    vec![]
                } else {
                    errors.into_iter().take(10).map(|(loc, code, msg)| Finding {
                        severity: Severity::High,
                        code,
                        message: msg,
                        location: Some(loc),
                        reproduce_cmd: Some(format!("{mypy_cmd} . --ignore-missing-imports 2>&1 | head -30")),
                        suggestion: Some(
                            "Add type annotations or use `# type: ignore` for intentionally untyped code. \
                             AI-generated Python frequently has type mismatches that cause AttributeError at runtime."
                                .to_string(),
                        ),
                    }).chain(if error_count > 10 {
                        vec![Finding {
                            severity: Severity::Info,
                            code: "MYPY_ERRORS_TRUNCATED".to_string(),
                            message: format!("{} mypy error(s) total — showing first 10", error_count),
                            reproduce_cmd: Some(format!("{mypy_cmd} . --ignore-missing-imports 2>&1")),
                            ..Default::default()
                        }]
                    } else {
                        vec![]
                    }).collect()
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "mypy".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        failed: error_count,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "mypy".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "MYPY_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run mypy: {}", e),
                    reproduce_cmd: Some(format!("{mypy_cmd} . 2>&1")),
                    suggestion: Some(
                        "Install: `pip install mypy` or `uv add --dev mypy`".to_string(),
                    ),
                    ..Default::default()
                }],
                metrics: LayerMetrics {
                    failed: 1,
                    ..Default::default()
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

/// Parse mypy output into (location, code, message) tuples.
/// mypy format: "path/file.py:12: error: Cannot assign to ... [attr-defined]"
pub fn parse_mypy_errors(output: &str) -> Vec<(String, String, String)> {
    let mut errors = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        // Format: "file.py:line: error: message [rule]"
        if !t.contains(": error: ") {
            continue;
        }

        let parts: Vec<&str> = t.splitn(3, ": error: ").collect();
        if parts.len() < 2 {
            continue;
        }

        let location = parts[0].to_string();
        let rest = parts[1];

        // Extract rule code from trailing "[rule-name]"
        let (message, code) = if let Some(bracket_start) = rest.rfind('[') {
            if let Some(bracket_end) = rest.rfind(']') {
                let rule = &rest[bracket_start + 1..bracket_end];
                let msg = rest[..bracket_start].trim().to_string();
                let code = format!("MYPY_{}", rule.replace('-', "_").to_uppercase());
                (msg, code)
            } else {
                (rest.to_string(), "MYPY_ERROR".to_string())
            }
        } else {
            (rest.to_string(), "MYPY_ERROR".to_string())
        };

        if !message.is_empty() {
            errors.push((location, code, message));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;

    fn py_info() -> ProjectInfo {
        ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    #[test]
    fn name_is_mypy() {
        assert_eq!(MypyRunner::default().name(), "mypy");
    }

    #[test]
    fn layer_is_logic() {
        assert!(matches!(MypyRunner::default().layer(), Layer::Logic));
    }

    #[test]
    fn not_available_for_rust() {
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::passing("")),
        };
        let i = ProjectInfo {
            language: Language::Rust,
            root: "/tmp".to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn available_when_mypy_installed() {
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::passing("mypy 1.8.0")),
        };
        assert!(r.is_available(&py_info()));
    }

    #[test]
    fn not_available_when_mypy_missing() {
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::unavailable()),
        };
        assert!(!r.is_available(&py_info()));
    }

    #[test]
    fn run_pass_returns_pass() {
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::passing(
                "Success: no issues found in 5 source files",
            )),
        };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_fail_parses_type_errors() {
        let output = "app/main.py:42: error: Argument 1 to \"get_user\" has incompatible type \"str\"; expected \"int\" [arg-type]";
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::failing(output)),
        };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert!(!result.findings.is_empty());
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0]
            .location
            .as_ref()
            .unwrap()
            .contains("app/main.py:42"));
        assert!(result.findings[0].code.contains("ARG_TYPE"));
    }

    #[test]
    fn run_truncates_to_10_errors() {
        let lines: String = (1..=15)
            .map(|i| format!("src/f.py:{i}: error: Type error {i} [assignment]\n"))
            .collect();
        let r = MypyRunner {
            proc: Arc::new(MockProcessRunner::failing(&lines)),
        };
        let result = r.run(&py_info()).unwrap();
        let type_errors: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.code.starts_with("MYPY_"))
            .collect();
        assert!(type_errors.len() <= 11); // 10 errors + 1 truncation notice
    }

    #[test]
    fn parse_error_with_rule_code() {
        let errors = parse_mypy_errors("app.py:10: error: Item \"None\" of \"Optional[str]\" has no attribute \"upper\" [union-attr]");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "app.py:10");
        assert!(errors[0].1.contains("UNION_ATTR"));
        assert!(errors[0].2.contains("upper"));
    }

    #[test]
    fn parse_skips_non_error_lines() {
        let errors = parse_mypy_errors("Found 2 errors in 1 file (checked 5 source files)");
        assert!(errors.is_empty());
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".*") { let _ = parse_mypy_errors(&s); }
    }
}
