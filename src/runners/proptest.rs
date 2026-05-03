use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

pub struct ProptestRunner;

impl TestRunner for ProptestRunner {
    fn name(&self) -> &'static str {
        "proptest"
    }

    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn skip_message(&self) -> &'static str {
        "No property-based tests found — add `proptest` to dev-dependencies for invariant testing"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }
        let cargo_toml = std::path::Path::new(&project.root).join("Cargo.toml");
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            content.contains("proptest")
        } else {
            false
        }
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        let output = Command::new("cargo")
            .args(["test"])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                let combined = format!("{}\n{}", stdout, stderr);

                let passed = result.status.success();
                let status = if passed { LayerStatus::Pass } else { LayerStatus::Fail };
                let (tests_run, tests_passed, tests_failed) = parse_test_counts(&combined);

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "proptest".to_string(),
                    status,
                    findings: if passed {
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
                    suggestion: Some("Ensure `cargo` is in PATH and the project compiles: `cargo check`.".to_string()),
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

fn parse_test_counts(output: &str) -> (u64, u64, u64) {
    for line in output.lines() {
        if line.starts_with("test result:") {
            let passed = extract_count(line, " passed");
            let failed = extract_count(line, " failed");
            return (passed + failed, passed, failed);
        }
    }
    (1, 1, 0)
}

fn extract_count(line: &str, label: &str) -> u64 {
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
    use proptest::prelude::*;

    // ── runner metadata ───────────────────────────────────────────────────────

    #[test]
    fn name_is_proptest() {
        assert_eq!(ProptestRunner.name(), "proptest");
    }

    #[test]
    fn layer_is_logic() {
        assert!(matches!(ProptestRunner.layer(), crate::plugin::Layer::Logic));
    }

    #[test]
    fn skip_message_mentions_proptest() {
        let msg = ProptestRunner.skip_message();
        assert!(!msg.is_empty());
        assert!(msg.contains("proptest"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    fn info(language: Language, root: &str) -> ProjectInfo {
        ProjectInfo { language, root: root.to_string(), has_tests: false, package_name: None }
    }

    #[test]
    fn not_available_for_typescript() {
        assert!(!ProptestRunner.is_available(&info(Language::TypeScript, "/tmp")));
    }

    #[test]
    fn not_available_for_go() {
        assert!(!ProptestRunner.is_available(&info(Language::Go, "/tmp")));
    }

    #[test]
    fn not_available_when_proptest_not_in_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        assert!(!ProptestRunner.is_available(&info(Language::Rust, &dir.path().to_string_lossy())));
    }

    #[test]
    fn available_when_proptest_in_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            b"[dev-dependencies]\nproptest = \"1.0\"",
        )
        .unwrap();
        assert!(ProptestRunner.is_available(&info(Language::Rust, &dir.path().to_string_lossy())));
    }

    // ── parse_test_counts ─────────────────────────────────────────────────────

    #[test]
    fn parses_standard_summary_line() {
        let line = "test result: ok. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s";
        let (total, passed, failed) = parse_test_counts(line);
        assert_eq!(passed, 5);
        assert_eq!(failed, 2);
        assert_eq!(total, 7);
    }

    #[test]
    fn parses_zero_counts() {
        let line = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let (total, passed, failed) = parse_test_counts(line);
        assert_eq!(passed, 0);
        assert_eq!(failed, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn fallback_for_no_summary() {
        let (total, passed, failed) = parse_test_counts("no summary line here");
        assert_eq!(total, 1);
        assert_eq!(passed, 1);
        assert_eq!(failed, 0);
    }

    proptest! {
        #[test]
        fn parse_test_counts_never_panics(s in ".*") {
            let _ = parse_test_counts(&s);
        }

        #[test]
        fn parse_test_counts_total_is_passed_plus_failed(
            passed in 0u64..500u64,
            failed in 0u64..500u64,
        ) {
            let line = format!(
                "test result: ok. {passed} passed; {failed} failed; 0 ignored; 0 measured; 0 filtered out"
            );
            let (total, p, f) = parse_test_counts(&line);
            prop_assert_eq!(p, passed);
            prop_assert_eq!(f, failed);
            prop_assert_eq!(total, passed + failed);
        }

        #[test]
        fn extract_count_never_panics(line in ".*", label in ".*") {
            let _ = extract_count(&line, &label);
        }
    }
}
