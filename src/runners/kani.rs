use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub struct KaniRunner;

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
        let has_bin = Command::new("cargo")
            .args(["kani", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !has_bin {
            return false;
        }

        has_kani_harnesses(Path::new(&project.root))
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let output = Command::new("cargo")
            .args(["kani"])
            .current_dir(root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                let combined = format!("{}\n{}", stdout, stderr);

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
    use proptest::prelude::*;
    use tempfile::tempdir;

    // ── runner metadata ───────────────────────────────────────────────────────

    #[test]
    fn name_is_kani() {
        assert_eq!(KaniRunner.name(), "kani");
    }

    #[test]
    fn layer_is_logic() {
        assert!(matches!(KaniRunner.layer(), crate::plugin::Layer::Logic));
    }

    #[test]
    fn skip_message_nonempty() {
        assert!(!KaniRunner.skip_message().is_empty());
        assert!(KaniRunner.skip_message().contains("kani::proof"));
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
            findings.iter().find(|f| matches!(f.severity, Severity::Critical | Severity::High)),
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
