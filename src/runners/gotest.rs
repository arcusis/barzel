use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub struct GoTestRunner;

impl TestRunner for GoTestRunner {
    fn name(&self) -> &'static str {
        "go-test"
    }

    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn skip_message(&self) -> &'static str {
        "go not found or no go.mod present — install Go and run `go mod init` to enable testing"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Go {
            return false;
        }
        Command::new("go")
            .args(["version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let output = Command::new("go")
            .args(["test", "./...", "-json", "-count=1"])
            .current_dir(root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                let (passed, failed, panics) = parse_go_json_output(&stdout);
                let total = passed + failed;

                let status = if failed > 0 || !panics.is_empty() {
                    LayerStatus::Fail
                } else if total == 0 {
                    LayerStatus::Skipped
                } else {
                    LayerStatus::Pass
                };

                let mut findings = Vec::new();

                if failed > 0 {
                    findings.push(Finding {
                        severity: Severity::High,
                        code: "GO_TEST_FAILURE".to_string(),
                        message: format!("{} test(s) failed out of {}", failed, total),
                        reproduce_cmd: Some("go test ./... -v 2>&1 | head -100".to_string()),
                        suggestion: Some(
                            "Run `go test ./... -v -run TestFailingName` to isolate the failure. \
                             Check for race conditions with `-race`."
                                .to_string(),
                        ),
                        ..Default::default()
                    });
                }

                for panic_loc in &panics {
                    findings.push(Finding {
                        severity: Severity::Critical,
                        code: "GO_PANIC".to_string(),
                        message: format!("Test panic detected: {}", panic_loc),
                        reproduce_cmd: Some("go test ./... -v 2>&1".to_string()),
                        suggestion: Some(
                            "A panic in tests indicates a nil dereference or unrecovered error. \
                             Add defer/recover or fix the root cause."
                                .to_string(),
                        ),
                        ..Default::default()
                    });
                }

                if total == 0 && stderr.contains("no test files") {
                    findings.push(Finding {
                        severity: Severity::Info,
                        code: "NO_GO_TESTS".to_string(),
                        message: "No test files found. Add *_test.go files for logic verification."
                            .to_string(),
                        reproduce_cmd: None,
                        suggestion: Some(
                            "Consider adding property-based tests using the `rapid` library \
                             (github.com/nicholasgasior/rapid) or `gopbt`."
                                .to_string(),
                        ),
                        ..Default::default()
                    });
                }

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "go-test".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: total,
                        passed,
                        failed,
                        coverage: None,
                        mutation_score: None,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "go-test".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "GO_TEST_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run go test: {}", e),
                    reproduce_cmd: Some("go test ./... -v".to_string()),
                    suggestion: Some(
                        "Ensure `go` is in PATH and the module is initialized with `go mod tidy`."
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

/// Parse `go test -json` output. Each line is a JSON event object.
fn parse_go_json_output(output: &str) -> (u64, u64, Vec<String>) {
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut panics = Vec::new();

    for line in output.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let action = event.get("Action").and_then(|a| a.as_str()).unwrap_or("");
        let test = event.get("Test").and_then(|t| t.as_str());

        // Only count leaf test actions (not package-level summaries)
        if test.is_none() {
            continue;
        }

        match action {
            "pass" => passed += 1,
            "fail" => {
                failed += 1;
                // Check if output contains panic
                if let Some(out) = event.get("Output").and_then(|o| o.as_str()) {
                    if out.contains("panic:") {
                        panics.push(out.lines().next().unwrap_or("unknown").trim().to_string());
                    }
                }
            }
            _ => {}
        }
    }

    (passed, failed, panics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn make_event(action: &str, test: Option<&str>) -> String {
        if let Some(t) = test {
            format!(r#"{{"Action":"{}","Test":"{}","Package":"example.com/pkg"}}"#, action, t)
        } else {
            format!(r#"{{"Action":"{}","Package":"example.com/pkg"}}"#, action)
        }
    }

    #[test]
    fn counts_pass_and_fail_events() {
        let output = [
            make_event("pass", Some("TestFoo")),
            make_event("pass", Some("TestBar")),
            make_event("fail", Some("TestBaz")),
        ]
        .join("\n");
        let (passed, failed, panics) = parse_go_json_output(&output);
        assert_eq!(passed, 2);
        assert_eq!(failed, 1);
        assert!(panics.is_empty());
    }

    #[test]
    fn ignores_package_level_events() {
        // Package-level pass/fail (no "Test" field) must not be counted
        let output = [
            make_event("pass", Some("TestFoo")),
            make_event("pass", None), // package-level
        ]
        .join("\n");
        let (passed, failed, _) = parse_go_json_output(&output);
        assert_eq!(passed, 1);
        assert_eq!(failed, 0);
    }

    #[test]
    fn detects_panic_in_fail_event() {
        let event = r#"{"Action":"fail","Test":"TestPanic","Package":"pkg","Output":"panic: nil pointer"}"#;
        let (_, failed, panics) = parse_go_json_output(event);
        assert_eq!(failed, 1);
        assert_eq!(panics.len(), 1);
        assert!(panics[0].contains("panic"));
    }

    #[test]
    fn empty_output_returns_zeros() {
        let (passed, failed, panics) = parse_go_json_output("");
        assert_eq!(passed, 0);
        assert_eq!(failed, 0);
        assert!(panics.is_empty());
    }

    #[test]
    fn ignores_non_json_lines() {
        let output = "not json\n{\"Action\":\"pass\",\"Test\":\"TestFoo\",\"Package\":\"p\"}";
        let (passed, _, _) = parse_go_json_output(output);
        assert_eq!(passed, 1);
    }

    proptest! {
        #[test]
        fn parse_go_json_never_panics(s in ".*") {
            let _ = parse_go_json_output(&s);
        }
    }
}
