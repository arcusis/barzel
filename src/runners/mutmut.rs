use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct MutmutRunner {
    pub mutation_threshold: f64,
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for MutmutRunner {
    fn default() -> Self {
        Self {
            mutation_threshold: 80.0,
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl MutmutRunner {
    pub fn with_threshold(mutation_threshold: f64) -> Self {
        Self {
            mutation_threshold,
            ..Default::default()
        }
    }
}

impl TestRunner for MutmutRunner {
    fn name(&self) -> &'static str {
        "mutmut"
    }
    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn skip_message(&self) -> &'static str {
        "mutmut not installed — run `pip install mutmut` to enable Python mutation testing (target: ≥80%)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Python {
            return false;
        }
        let root = Path::new(&project.root);
        let local = root.join(".venv").join("bin").join("mutmut");
        if local.exists() {
            return true;
        }
        self.proc.is_available("mutmut", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);
        let threshold = self.mutation_threshold;

        let local = root.join(".venv").join("bin").join("mutmut");
        let mutmut_cmd = if local.exists() {
            local.to_string_lossy().to_string()
        } else {
            "mutmut".to_string()
        };

        match self.proc.run(&mutmut_cmd, &["run"], root) {
            Ok(_) => {
                // mutmut stores results in .mutmut-cache; query results
                let results_out = self.proc.run(&mutmut_cmd, &["results"], root);
                let mutation_score = results_out
                    .as_ref()
                    .ok()
                    .map(|o| parse_mutmut_results(&o.combined()))
                    .unwrap_or(None);

                let status = match mutation_score {
                    Some(s) if s >= threshold => LayerStatus::Pass,
                    Some(_) => LayerStatus::Partial,
                    None => LayerStatus::Partial,
                };

                let findings = match mutation_score {
                    Some(s) if s < threshold => vec![Finding {
                        severity: Severity::High,
                        code: "LOW_MUTATION_SCORE".to_string(),
                        message: format!(
                            "Python mutation score is {s:.1}% (target ≥{threshold:.0}%) — surviving mutants indicate under-tested logic",
                        ),
                        reproduce_cmd: Some(format!("{mutmut_cmd} results 2>&1")),
                        suggestion: Some(
                            "Run `mutmut show <id>` to inspect surviving mutants. \
                             Add tests for boundary conditions and branching logic."
                                .to_string(),
                        ),
                        ..Default::default()
                    }],
                    Some(_) => vec![],
                    None => vec![Finding {
                        severity: Severity::Info,
                        code: "MUTATION_RUN_COMPLETE".to_string(),
                        message: "mutmut run complete — check `.mutmut-cache` for surviving mutants".to_string(),
                        reproduce_cmd: Some(format!("{mutmut_cmd} results 2>&1")),
                        ..Default::default()
                    }],
                };

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "mutmut".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        mutation_score,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "structural".to_string(),
                runner: "mutmut".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "MUTMUT_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run mutmut: {}", e),
                    reproduce_cmd: Some(format!("{mutmut_cmd} run 2>&1")),
                    suggestion: Some(
                        "Install: `pip install mutmut` or `uv add --dev mutmut`. \
                         Ensure pytest passes before running mutation testing."
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

pub fn parse_mutmut_results(output: &str) -> Option<f64> {
    // mutmut results output:
    // "KILLED 42 (84%)\nSURVIVED 8 (16%)\n..." or
    // "42 out of 50 mutants killed (84.00%)"
    let mut killed = 0u64;
    let mut survived = 0u64;

    for line in output.lines() {
        let l = line.trim().to_uppercase();
        if l.starts_with("KILLED") {
            killed = extract_count_from_mutmut_line(line);
        } else if l.starts_with("SURVIVED") {
            survived = extract_count_from_mutmut_line(line);
        }
    }

    let total = killed + survived;
    if total > 0 {
        return Some(killed as f64 / total as f64 * 100.0);
    }

    // Alternative: "X out of Y mutants killed (Z%)"
    for line in output.lines() {
        if line.contains("mutants killed") {
            if let Some(pct) = parse_pct_from_line(line) {
                return Some(pct);
            }
        }
    }

    None
}

fn extract_count_from_mutmut_line(line: &str) -> u64 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn parse_pct_from_line(line: &str) -> Option<f64> {
    if let Some(start) = line.find('(') {
        if let Some(end) = line.find('%') {
            let s = line[start + 1..end].trim();
            return s.parse().ok();
        }
    }
    None
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

    fn runner_with(mock: MockProcessRunner) -> MutmutRunner {
        MutmutRunner {
            proc: Arc::new(mock),
            ..Default::default()
        }
    }

    #[test]
    fn name_is_mutmut() {
        assert_eq!(MutmutRunner::default().name(), "mutmut");
    }

    #[test]
    fn layer_is_structural() {
        assert!(matches!(MutmutRunner::default().layer(), Layer::Structural));
    }

    #[test]
    fn not_available_for_rust() {
        let r = MutmutRunner {
            proc: Arc::new(MockProcessRunner::passing("")),
            ..Default::default()
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
    fn available_when_system_mutmut_present() {
        let r = MutmutRunner {
            proc: Arc::new(MockProcessRunner::passing("mutmut 2.4.5")),
            ..Default::default()
        };
        assert!(r.is_available(&py_info()));
    }

    #[test]
    fn not_available_when_mutmut_missing() {
        let r = MutmutRunner {
            proc: Arc::new(MockProcessRunner::unavailable()),
            ..Default::default()
        };
        assert!(!r.is_available(&py_info()));
    }

    #[test]
    fn run_pass_above_threshold() {
        // First call (run) passes, second call (results) returns KILLED/SURVIVED
        let mock = MockProcessRunner::passing("KILLED 42\nSURVIVED 8");
        let result = runner_with(mock).run(&py_info()).unwrap();
        // default threshold 80%, 42/50 = 84%
        assert!(matches!(result.status, LayerStatus::Pass));
        let score = result.metrics.mutation_score.unwrap();
        assert!((score - 84.0).abs() < 0.1);
    }

    #[test]
    fn run_partial_below_threshold() {
        let mock = MockProcessRunner::passing("KILLED 6\nSURVIVED 4");
        let result = runner_with(mock).run(&py_info()).unwrap();
        // 6/10 = 60%, below 80% threshold
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].message.contains("60.0%"));
    }

    #[test]
    fn run_uses_configurable_threshold() {
        let mock = MockProcessRunner::passing("KILLED 6\nSURVIVED 4"); // 60%
        let r = MutmutRunner {
            mutation_threshold: 50.0,
            proc: Arc::new(mock),
        };
        assert!(matches!(
            r.run(&py_info()).unwrap().status,
            LayerStatus::Pass
        ));
    }

    #[test]
    fn run_fail_on_subprocess_error() {
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
                    "not found",
                ))
            }
        }
        let r = MutmutRunner {
            proc: Arc::new(BrokenProc),
            ..Default::default()
        };
        let res = r.run(&py_info()).unwrap();
        assert!(matches!(res.status, LayerStatus::Fail));
        assert_eq!(res.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn parse_killed_survived_format() {
        let s = parse_mutmut_results("KILLED 42\nSURVIVED 8").unwrap();
        assert!((s - 84.0).abs() < 0.1);
    }

    #[test]
    fn parse_pct_format() {
        let s = parse_mutmut_results("42 out of 50 mutants killed (84.00%)").unwrap();
        assert!((s - 84.0).abs() < 0.1);
    }

    #[test]
    fn parse_zero_mutants_returns_none() {
        assert!(parse_mutmut_results("no mutants found").is_none());
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".*") { let _ = parse_mutmut_results(&s); }

        #[test]
        fn score_in_range(killed in 0u64..500u64, survived in 0u64..500u64) {
            let line = format!("KILLED {killed}\nSURVIVED {survived}");
            if let Some(s) = parse_mutmut_results(&line) {
                prop_assert!(s >= 0.0 && s <= 100.0);
            }
        }
    }
}
