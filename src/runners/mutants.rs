use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct MutantsRunner {
    pub mutation_threshold: f64,
    pub timeout_seconds: u32,
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for MutantsRunner {
    fn default() -> Self {
        Self { mutation_threshold: 95.0, timeout_seconds: 30, proc: Arc::new(OsProcessRunner) }
    }
}

impl MutantsRunner {
    pub fn with_threshold(mutation_threshold: f64) -> Self {
        Self { mutation_threshold, ..Default::default() }
    }
}

impl TestRunner for MutantsRunner {
    fn name(&self) -> &'static str { "cargo-mutants" }
    fn layer(&self) -> Layer { Layer::Structural }

    fn skip_message(&self) -> &'static str {
        "cargo-mutants not installed — run `cargo install cargo-mutants` to enable mutation testing (target: ≥95% mutation score)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust { return false; }
        self.proc.is_available("cargo", &["mutants", "--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);
        let timeout = self.timeout_seconds.to_string();

        match self.proc.run("cargo", &["mutants", "--timeout", &timeout, "--no-shuffle"], root) {
            Ok(out) => {
                let mutation_score = parse_mutation_score(&out.combined());
                let threshold = self.mutation_threshold;

                let status = match mutation_score {
                    Some(s) if s >= threshold => LayerStatus::Pass,
                    _ => LayerStatus::Partial,
                };

                let findings = match mutation_score {
                    Some(s) if s < threshold => vec![Finding {
                        severity: Severity::High,
                        code: "LOW_MUTATION_SCORE".to_string(),
                        message: format!(
                            "Mutation score is {s:.1}% (target ≥{threshold:.0}%) — {:.1}% of mutants survived",
                            100.0 - s
                        ),
                        reproduce_cmd: Some("cargo mutants 2>&1 | tail -20".to_string()),
                        suggestion: Some(
                            "Check mutants.out/missed.txt. Add tests for boundary conditions.".to_string(),
                        ),
                        ..Default::default()
                    }],
                    Some(_) => vec![],
                    None => vec![Finding {
                        severity: Severity::Info,
                        code: "MUTATION_RUN_COMPLETE".to_string(),
                        message: "Mutation testing completed — check mutants.out/ for details".to_string(),
                        reproduce_cmd: Some("cargo mutants 2>&1".to_string()),
                        ..Default::default()
                    }],
                };

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "cargo-mutants".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics { mutation_score, ..Default::default() },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "structural".to_string(),
                runner: "cargo-mutants".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "MUTATION_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run cargo-mutants: {}", e),
                    reproduce_cmd: Some("cargo mutants 2>&1".to_string()),
                    suggestion: Some("Install: `cargo install cargo-mutants`".to_string()),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_mutation_score(output: &str) -> Option<f64> {
    for line in output.lines() {
        if line.contains("mutants tested") && line.contains("caught") {
            let caught = count_before_label(line, "caught");
            let missed = count_before_label(line, "missed");
            let total = caught + missed;
            if total > 0 { return Some(caught as f64 / total as f64 * 100.0); }
        }
        if line.contains("mutation score") && line.contains('%') {
            if let Some(pct) = line.split('%').next() {
                if let Some(n) = pct.split_whitespace().last() {
                    if let Ok(s) = n.parse::<f64>() { return Some(s); }
                }
            }
        }
    }
    None
}

pub fn count_before_label(line: &str, label: &str) -> u64 {
    if let Some(idx) = line.find(label) {
        let before = line[..idx].trim_end();
        if let Some(tok) = before.split_whitespace().last() {
            return tok.trim_end_matches(',').parse().unwrap_or(0);
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

    fn rust_info() -> ProjectInfo {
        ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: true, package_name: None, frameworks: Default::default(), workspace_root: None }
    }

    fn runner_with(mock: MockProcessRunner) -> MutantsRunner {
        MutantsRunner { proc: Arc::new(mock), ..Default::default() }
    }

    #[test]
    fn name_and_layer() {
        assert_eq!(MutantsRunner::default().name(), "cargo-mutants");
        assert!(matches!(MutantsRunner::default().layer(), Layer::Structural));
    }

    #[test]
    fn not_available_for_non_rust() {
        let r = MutantsRunner { proc: Arc::new(MockProcessRunner::passing("")), ..Default::default() };
        let i = ProjectInfo { language: Language::TypeScript, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn available_when_tool_present() {
        let r = MutantsRunner { proc: Arc::new(MockProcessRunner::passing("cargo-mutants 27")), ..Default::default() };
        assert!(r.is_available(&rust_info()));
    }

    #[test]
    fn not_available_when_tool_missing() {
        let r = MutantsRunner { proc: Arc::new(MockProcessRunner::unavailable()), ..Default::default() };
        assert!(!r.is_available(&rust_info()));
    }

    #[test]
    fn run_pass_at_100_percent() {
        let result = runner_with(MockProcessRunner::passing("10 mutants tested in 5s: 0 missed, 10 caught"))
            .run(&rust_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_partial_below_threshold() {
        let result = runner_with(MockProcessRunner::passing("10 mutants tested: 8 missed, 2 caught"))
            .run(&rust_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].message.contains("20.0%"));
    }

    #[test]
    fn run_uses_configurable_threshold() {
        let stdout = "10 mutants tested: 2 missed, 8 caught"; // 80%
        let r = MutantsRunner { mutation_threshold: 70.0, proc: Arc::new(MockProcessRunner::passing(stdout)), ..Default::default() };
        assert!(matches!(r.run(&rust_info()).unwrap().status, LayerStatus::Pass));
    }

    #[test]
    fn run_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
            }
        }
        let r = MutantsRunner { proc: Arc::new(BrokenProc), ..Default::default() };
        let res = r.run(&rust_info()).unwrap();
        assert!(matches!(res.status, LayerStatus::Fail));
        assert_eq!(res.metrics.failed, 1);
    }

    #[test]
    fn parses_new_format() {
        let s = parse_mutation_score("17 mutants tested in 23s: 11 missed, 6 caught").unwrap();
        assert!((s - 6.0 / 17.0 * 100.0).abs() < 0.01);
    }

    #[test]
    fn parses_with_unviable() {
        let s = parse_mutation_score("461 mutants tested in 8m: 395 missed, 37 caught, 29 unviable").unwrap();
        assert!((s - 37.0 / 432.0 * 100.0).abs() < 0.01);
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".*") { let _ = parse_mutation_score(&s); }

        #[test]
        fn score_in_range(caught in 0u64..1000u64, missed in 0u64..1000u64) {
            let line = format!("{} mutants tested in 1s: {} missed, {} caught", caught + missed, missed, caught);
            if let Some(s) = parse_mutation_score(&line) {
                prop_assert!(s >= 0.0 && s <= 100.0);
            }
        }
    }
}
