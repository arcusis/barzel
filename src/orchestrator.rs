use crate::cache;
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;

fn is_cacheable(layer: Layer) -> bool {
    matches!(layer, Layer::Structural)
}

pub struct VerificationOrchestrator<'a> {
    runners: Vec<&'a dyn TestRunner>,
    no_cache: bool,
    fail_fast: bool,
}

impl<'a> VerificationOrchestrator<'a> {
    pub fn new(runners: Vec<&'a dyn TestRunner>) -> Self {
        Self {
            runners,
            no_cache: false,
            fail_fast: false,
        }
    }

    pub fn with_no_cache(mut self) -> Self {
        self.no_cache = true;
        self
    }

    pub fn with_fail_fast(mut self) -> Self {
        self.fail_fast = true;
        self
    }

    pub fn run_with_progress<F>(
        &self,
        project: &ProjectInfo,
        on_start: F,
    ) -> Result<BarzelReport>
    where
        F: Fn(&str, &str),
    {
        let mut report = BarzelReport::new(project.clone());
        let project_root = Path::new(&project.root);

        for runner in &self.runners {
            if !runner.is_available(project) {
                report.add_layer(LayerResult {
                    name: runner.layer().as_str().to_string(),
                    runner: runner.name().to_string(),
                    status: LayerStatus::Skipped,
                    findings: vec![Finding {
                        severity: Severity::Info,
                        code: "RUNNER_UNAVAILABLE".to_string(),
                        message: runner.skip_message().to_string(),
                        ..Default::default()
                    }],
                    metrics: LayerMetrics::default(),
                    duration_ms: 0,
                });
                continue;
            }

            if !self.no_cache
                && is_cacheable(runner.layer())
                && cache::is_cached(project_root, runner.name())
            {
                report.add_layer(LayerResult {
                    name: runner.layer().as_str().to_string(),
                    runner: runner.name().to_string(),
                    status: LayerStatus::Skipped,
                    findings: vec![Finding {
                        severity: Severity::Info,
                        code: "CACHED".to_string(),
                        message: "Source unchanged since last run — using cached result".to_string(),
                        ..Default::default()
                    }],
                    metrics: LayerMetrics::default(),
                    duration_ms: 0,
                });
                continue;
            }

            on_start(runner.name(), runner.layer().as_str());

            match runner.run(project) {
                Ok(layer_result) => {
                    if is_cacheable(runner.layer())
                        && !matches!(layer_result.status, LayerStatus::Fail)
                    {
                        cache::save_current_hash(project_root, runner.name());
                    }
                    let is_fail = matches!(layer_result.status, LayerStatus::Fail);
                    report.add_layer(layer_result);
                    if self.fail_fast && is_fail {
                        break;
                    }
                }
                Err(e) => {
                    report.add_layer(LayerResult {
                        name: runner.layer().as_str().to_string(),
                        runner: runner.name().to_string(),
                        status: LayerStatus::Fail,
                        findings: vec![Finding {
                            severity: Severity::Critical,
                            code: "RUNNER_FAILED".to_string(),
                            message: format!("Runner '{}' failed: {}", runner.name(), e),
                            ..Default::default()
                        }],
                        metrics: LayerMetrics {
                            failed: 1,
                            ..Default::default()
                        },
                        duration_ms: 0,
                    });
                    if self.fail_fast {
                        break;
                    }
                }
            }
        }

        Ok(report)
    }

    pub fn run(&self, project: &ProjectInfo) -> Result<BarzelReport> {
        self.run_with_progress(project, |_, _| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::report::Severity;
    use std::path::Path;
    use tempfile::tempdir;

    fn rust_project(root: &Path) -> ProjectInfo {
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        ProjectInfo {
            language: Language::Rust,
            root: root.to_string_lossy().to_string(),
            has_tests: false,
            package_name: Some("test".to_string()),
            frameworks: Default::default(),
        }
    }

    // ── Mock runners ──────────────────────────────────────────────────────────

    struct PassRunner {
        layer: Layer,
        name: &'static str,
    }
    impl TestRunner for PassRunner {
        fn name(&self) -> &'static str { self.name }
        fn layer(&self) -> Layer { self.layer }
        fn is_available(&self, _: &ProjectInfo) -> bool { true }
        fn run(&self, _: &ProjectInfo) -> Result<LayerResult> {
            Ok(LayerResult {
                name: self.layer.as_str().to_string(),
                runner: self.name.to_string(),
                status: LayerStatus::Pass,
                findings: vec![],
                metrics: LayerMetrics::default(),
                duration_ms: 1,
            })
        }
    }

    struct FailRunner { layer: Layer }
    impl TestRunner for FailRunner {
        fn name(&self) -> &'static str { "fail-runner" }
        fn layer(&self) -> Layer { self.layer }
        fn is_available(&self, _: &ProjectInfo) -> bool { true }
        fn run(&self, _: &ProjectInfo) -> Result<LayerResult> {
            Ok(LayerResult {
                name: self.layer.as_str().to_string(),
                runner: "fail-runner".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "TEST_FAIL".to_string(),
                    message: "forced failure".to_string(),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: 1,
            })
        }
    }

    struct ErrorRunner;
    impl TestRunner for ErrorRunner {
        fn name(&self) -> &'static str { "error-runner" }
        fn layer(&self) -> Layer { Layer::Logic }
        fn is_available(&self, _: &ProjectInfo) -> bool { true }
        fn run(&self, _: &ProjectInfo) -> Result<LayerResult> {
            Err(crate::error::BarzelError::Detection("forced error".to_string()))
        }
    }

    struct UnavailableRunner;
    impl TestRunner for UnavailableRunner {
        fn name(&self) -> &'static str { "unavailable" }
        fn layer(&self) -> Layer { Layer::Logic }
        fn is_available(&self, _: &ProjectInfo) -> bool { false }
        fn skip_message(&self) -> &'static str { "tool not installed" }
        fn run(&self, _: &ProjectInfo) -> Result<LayerResult> { unreachable!() }
    }

    // ── Unavailable runner handling ───────────────────────────────────────────

    #[test]
    fn unavailable_runner_produces_skipped_result() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = UnavailableRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers.len(), 1);
        assert!(matches!(report.layers[0].status, LayerStatus::Skipped));
        assert_eq!(report.layers[0].findings[0].message, "tool not installed");
    }

    #[test]
    fn unavailable_runner_is_not_called() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        // If run() were called on UnavailableRunner it would panic — this should not panic
        let runner = UnavailableRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let _ = orch.run(&project).unwrap(); // must not panic
    }

    // ── fail_fast ─────────────────────────────────────────────────────────────

    #[test]
    fn fail_fast_stops_after_first_failure() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let fail = FailRunner { layer: Layer::Logic };
        let pass = PassRunner { layer: Layer::Structural, name: "second-runner" };
        let orch = VerificationOrchestrator::new(vec![
            &fail as &dyn TestRunner,
            &pass as &dyn TestRunner,
        ])
        .with_fail_fast();
        let report = orch.run(&project).unwrap();
        // Only the failing layer — second runner was not reached
        assert_eq!(report.layers.len(), 1);
        assert!(matches!(report.layers[0].status, LayerStatus::Fail));
    }

    #[test]
    fn without_fail_fast_all_runners_execute() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let fail = FailRunner { layer: Layer::Logic };
        let pass = PassRunner { layer: Layer::Hostile, name: "second-runner" };
        let orch = VerificationOrchestrator::new(vec![
            &fail as &dyn TestRunner,
            &pass as &dyn TestRunner,
        ]);
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers.len(), 2);
    }

    // ── Caching: is_cacheable ─────────────────────────────────────────────────

    #[test]
    fn structural_layer_is_cached_after_successful_run() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        // Cache should be written for structural layer
        assert!(crate::cache::is_cached(dir.path(), "mock-mutants"));
    }

    #[test]
    fn logic_layer_is_not_cached() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Logic, name: "mock-proptest" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        // Logic layer must NOT write cache
        assert!(!crate::cache::is_cached(dir.path(), "mock-proptest"));
    }

    #[test]
    fn hostile_layer_is_not_cached() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Hostile, name: "mock-semgrep" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(!crate::cache::is_cached(dir.path(), "mock-semgrep"));
    }

    #[test]
    fn failed_structural_run_does_not_write_cache() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = FailRunner { layer: Layer::Structural };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(!crate::cache::is_cached(dir.path(), "fail-runner"));
    }

    #[test]
    fn cached_structural_layer_is_skipped() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        // Pre-populate cache
        crate::cache::save_current_hash(dir.path(), "mock-mutants");
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let report = orch.run(&project).unwrap();
        // Should be skipped (cached), not Pass
        assert!(matches!(report.layers[0].status, LayerStatus::Skipped));
        assert_eq!(report.layers[0].findings[0].code, "CACHED");
    }

    #[test]
    fn no_cache_flag_bypasses_cached_layer() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        // Pre-populate cache
        crate::cache::save_current_hash(dir.path(), "mock-mutants");
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]).with_no_cache();
        let report = orch.run(&project).unwrap();
        // With no_cache: runner executes and returns Pass, not Skipped
        assert!(matches!(report.layers[0].status, LayerStatus::Pass));
    }

    // ── Error runner handling ─────────────────────────────────────────────────

    #[test]
    fn runner_error_produces_fail_layer_with_failed_metric() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = ErrorRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers.len(), 1);
        assert!(matches!(report.layers[0].status, LayerStatus::Fail));
        // Catches the "delete field failed" mutation
        assert_eq!(report.layers[0].metrics.failed, 1);
        assert!(report.layers[0].findings[0].code.contains("RUNNER_FAILED"));
    }

    #[test]
    fn runner_error_with_fail_fast_stops_pipeline() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let err = ErrorRunner;
        let pass = PassRunner { layer: Layer::Structural, name: "second" };
        let orch = VerificationOrchestrator::new(vec![
            &err as &dyn TestRunner,
            &pass as &dyn TestRunner,
        ]).with_fail_fast();
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers.len(), 1); // stopped after error
    }
}
