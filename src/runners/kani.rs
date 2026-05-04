use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct KaniRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for KaniRunner {
    fn default() -> Self {
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for KaniRunner {
    fn name(&self) -> &'static str {
        "kani"
    }

    fn layer(&self) -> Layer {
        Layer::Logic
    }

    fn skip_message(&self) -> &'static str {
        "No #[kani::proof] harnesses found — annotate critical functions with \
         #[kani::proof] and add `kani-verifier` to dev-dependencies for formal verification"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }
        let has_bin = self.proc.is_available("cargo", &["kani", "--version"]);

        if !has_bin {
            return false;
        }

        has_kani_harnesses(Path::new(&project.root))
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        match self.proc.run("cargo", &["kani"], root) {
            Ok(out) => {
                let combined = out.combined();
                let (verified, failed) = parse_kani_output(&combined);
                let total = verified + failed;

                let status = if failed > 0 {
                    LayerStatus::Fail
                } else if total == 0 {
                    LayerStatus::Skipped
                } else {
                    LayerStatus::Pass
                };

                let findings = if failed > 0 {
                    extract_kani_failures(&combined)
                } else {
                    vec![]
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "kani".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: total,
                        passed: verified,
                        failed,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "kani".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "KANI_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run kani: {}", e),
                    reproduce_cmd: Some("cargo kani 2>&1".to_string()),
                    suggestion: Some(
                        "Install kani: `cargo install --locked kani-verifier && cargo kani setup`"
                            .to_string(),
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

fn has_kani_harnesses(root: &Path) -> bool {
    let src = root.join("src");
    dir_contains_pattern(&src, "#[kani::proof]")
}

fn dir_contains_pattern(dir: &Path, pattern: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains_pattern(&path, pattern) {
                return true;
            }
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains(pattern) {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_kani_output(output: &str) -> (u64, u64) {
    let mut verified = 0u64;
    let mut failed = 0u64;

    for line in output.lines() {
        // "VERIFICATION:- SUCCESSFUL"
        if line.contains("VERIFICATION") && line.contains("SUCCESSFUL") {
            verified += 1;
        }
        // "VERIFICATION:- FAILED"
        if line.contains("VERIFICATION") && line.contains("FAILED") {
            failed += 1;
        }
    }

    (verified, failed)
}

fn extract_kani_failures(output: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    for line in output.lines() {
        if line.contains("VERIFICATION") && line.contains("FAILED") {
            // Try to extract the harness name from context
            let harness = output
                .lines()
                .find(|l| l.contains("Checking harness"))
                .and_then(|l| l.split_whitespace().last())
                .unwrap_or("unknown")
                .to_string();

            findings.push(Finding {
                severity: Severity::Critical,
                code: "KANI_VERIFICATION_FAILED".to_string(),
                message: format!(
                    "Formal verification failed for harness `{}` — a reachable assertion is violated",
                    harness
                ),
                reproduce_cmd: Some(format!("cargo kani --harness {} 2>&1", harness)),
                suggestion: Some(
                    "Kani found a concrete counterexample. Check the trace printed above \
                     to identify which assertion fails and under what inputs."
                        .to_string(),
                ),
                ..Default::default()
            });
        }
    }

    if findings.is_empty() {
        findings.push(Finding {
            severity: Severity::High,
            code: "KANI_FAILED".to_string(),
            message: "Kani verification failed — run `cargo kani 2>&1` for the full trace"
                .to_string(),
            reproduce_cmd: Some("cargo kani 2>&1".to_string()),
            ..Default::default()
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn rust_info() -> ProjectInfo {
        ProjectInfo {
            language: Language::Rust,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    fn runner_with(mock: MockProcessRunner) -> KaniRunner {
        KaniRunner {
            proc: Arc::new(mock),
        }
    }

    // ── runner metadata ───────────────────────────────────────────────────────

    #[test]
    fn name_is_kani() {
        assert_eq!(KaniRunner::default().name(), "kani");
    }

    #[test]
    fn layer_is_logic() {
        assert!(matches!(
            KaniRunner::default().layer(),
            crate::plugin::Layer::Logic
        ));
    }

    #[test]
    fn skip_message_nonempty() {
        assert!(!KaniRunner::default().skip_message().is_empty());
        assert!(KaniRunner::default().skip_message().contains("kani::proof"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn not_available_for_typescript() {
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: "/tmp".to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(!KaniRunner::default().is_available(&info));
    }

    #[test]
    fn not_available_when_binary_missing() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), b"#[kani::proof]\nfn verify() {}").unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        let r = KaniRunner {
            proc: Arc::new(MockProcessRunner::unavailable()),
        };
        assert!(!r.is_available(&info));
    }

    #[test]
    fn not_available_when_no_harnesses() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), b"fn main() {}").unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        let r = KaniRunner {
            proc: Arc::new(MockProcessRunner::passing("kani 0.40")),
        };
        assert!(!r.is_available(&info));
    }

    #[test]
    fn available_when_binary_present_and_harnesses_exist() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), b"#[kani::proof]\nfn verify() {}").unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        let r = KaniRunner {
            proc: Arc::new(MockProcessRunner::passing("kani 0.40")),
        };
        assert!(r.is_available(&info));
    }

    // ── run() ─────────────────────────────────────────────────────────────────

    #[test]
    fn run_returns_pass_on_successful_verification() {
        let stdout = "VERIFICATION:- SUCCESSFUL\nVERIFICATION:- SUCCESSFUL";
        let result = runner_with(MockProcessRunner::passing(stdout))
            .run(&rust_info())
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert_eq!(result.metrics.passed, 2);
        assert_eq!(result.metrics.failed, 0);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_returns_fail_on_verification_failure() {
        let stdout = "VERIFICATION:- FAILED";
        let result = runner_with(MockProcessRunner::failing(stdout))
            .run(&rust_info())
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert!(!result.findings.is_empty());
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
                    "cargo not found",
                ))
            }
        }
        let runner = KaniRunner {
            proc: Arc::new(BrokenProc),
        };
        let result = runner.run(&rust_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    // ── has_kani_harnesses ────────────────────────────────────────────────────

    #[test]
    fn detects_kani_proof_annotation() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), b"#[kani::proof]\nfn verify() {}").unwrap();
        assert!(has_kani_harnesses(dir.path()));
    }

    #[test]
    fn returns_false_when_no_annotation() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), b"fn main() {}").unwrap();
        assert!(!has_kani_harnesses(dir.path()));
    }

    #[test]
    fn returns_false_when_no_src_dir() {
        let dir = tempdir().unwrap();
        assert!(!has_kani_harnesses(dir.path()));
    }

    #[test]
    fn detects_annotation_in_subdirectory() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("src").join("auth");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("verify.rs"), b"#[kani::proof]\nfn check() {}").unwrap();
        assert!(has_kani_harnesses(dir.path()));
    }

    // ── parse_kani_output ─────────────────────────────────────────────────────

    #[test]
    fn counts_successful_verifications() {
        let output = "VERIFICATION:- SUCCESSFUL\nVERIFICATION:- SUCCESSFUL";
        let (verified, failed) = parse_kani_output(output);
        assert_eq!(verified, 2);
        assert_eq!(failed, 0);
    }

    #[test]
    fn counts_failed_verifications() {
        let output = "VERIFICATION:- FAILED\nVERIFICATION:- SUCCESSFUL\nVERIFICATION:- FAILED";
        let (verified, failed) = parse_kani_output(output);
        assert_eq!(verified, 1);
        assert_eq!(failed, 2);
    }

    #[test]
    fn empty_output_returns_zeros() {
        let (v, f) = parse_kani_output("");
        assert_eq!(v, 0);
        assert_eq!(f, 0);
    }

    // ── extract_kani_failures ─────────────────────────────────────────────────

    #[test]
    fn returns_fallback_finding_when_no_harness_info() {
        let output = "VERIFICATION:- FAILED";
        let findings = extract_kani_failures(output);
        assert!(!findings.is_empty());
        assert!(matches!(
            findings
                .iter()
                .find(|f| matches!(f.severity, Severity::Critical | Severity::High)),
            Some(_)
        ));
    }

    #[test]
    fn extracts_harness_name_when_present() {
        let output = "Checking harness my_proof_harness\nVERIFICATION:- FAILED";
        let findings = extract_kani_failures(output);
        assert!(findings[0].message.contains("my_proof_harness"));
    }

    #[test]
    fn generic_finding_when_no_failed_line() {
        let findings = extract_kani_failures("no failures here");
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::High));
    }

    proptest! {
        #[test]
        fn parse_kani_output_never_panics(s in ".*") {
            let _ = parse_kani_output(&s);
        }

        #[test]
        fn extract_kani_failures_never_panics(s in ".*") {
            let _ = extract_kani_failures(&s);
        }
    }
}
