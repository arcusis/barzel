use crate::cache;
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;

fn is_cacheable(layer: Layer) -> bool {
    matches!(layer, Layer::Structural)
}

/// Layer execution order for deterministic report output.
fn layer_priority(layer: Layer) -> u8 {
    match layer {
        Layer::Logic => 0,
        Layer::Structural => 1,
        Layer::Hostile => 2,
        Layer::Operational => 3,
    }
}

pub struct VerificationOrchestrator<'a> {
    runners: Vec<&'a dyn TestRunner>,
    no_cache: bool,
    fail_fast: bool,
}

impl<'a> VerificationOrchestrator<'a> {
    pub fn new(runners: Vec<&'a dyn TestRunner>) -> Self {
        Self { runners, no_cache: false, fail_fast: false }
    }

    pub fn with_no_cache(mut self) -> Self { self.no_cache = true; self }
    pub fn with_fail_fast(mut self) -> Self { self.fail_fast = true; self }

    pub fn run_with_progress<F>(&self, project: &ProjectInfo, on_start: F) -> Result<BarzelReport>
    where
        F: Fn(&str, &str) + Sync,
    {
        let project_root = Path::new(&project.root);

        // When fail_fast is set, run sequentially to stop on first failure.
        // When fail_fast is off, phase-1 (Logic + Hostile + Operational) runs in parallel.
        let indexed_results: Vec<(usize, LayerResult)> = if self.fail_fast {
            self.run_sequential(project, project_root, &on_start)
        } else {
            self.run_parallel(project, project_root, &on_start)
        };

        // Sort by (layer_priority, original_runner_index) for deterministic output
        let mut sorted = indexed_results;
        sorted.sort_by_key(|(i, r)| (layer_priority(layer_from_str(&r.name)), *i));

        let mut report = BarzelReport::new(project.clone());
        for (_, result) in sorted {
            report.add_layer(result);
        }

        Ok(report)
    }

    fn run_sequential<F>(
        &self,
        project: &ProjectInfo,
        project_root: &Path,
        on_start: &F,
    ) -> Vec<(usize, LayerResult)>
    where
        F: Fn(&str, &str) + Sync,
    {
        let mut results = Vec::new();
        for (i, runner) in self.runners.iter().enumerate() {
            let result = self.run_one(runner, project, project_root, on_start);
            let is_fail = matches!(result.status, LayerStatus::Fail);
            results.push((i, result));
            if is_fail {
                break;
            }
        }
        results
    }

    fn run_parallel<F>(
        &self,
        project: &ProjectInfo,
        project_root: &Path,
        on_start: &F,
    ) -> Vec<(usize, LayerResult)>
    where
        F: Fn(&str, &str) + Sync,
    {
        // Phase 1: Logic + Hostile + Operational in parallel (indexed for stable ordering)
        let (phase1, phase2): (Vec<_>, Vec<_>) = self
            .runners
            .iter()
            .enumerate()
            .partition(|(_, r)| !matches!(r.layer(), Layer::Structural));

        let mut phase1_results: Vec<(usize, LayerResult)> = std::thread::scope(|scope| {
            let handles: Vec<_> = phase1
                .iter()
                .map(|(i, runner)| {
                    let i = *i;
                    scope.spawn(move || (i, self.run_one(runner, project, project_root, on_start)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let logic_failed = phase1_results.iter().any(|(_, r)| {
            matches!(r.status, LayerStatus::Fail) && r.name == "logic"
        });

        // Phase 2: Structural runs after — skip if logic already failed (useless to mutate broken tests)
        let mut phase2_results: Vec<(usize, LayerResult)> = Vec::new();
        if !logic_failed {
            for (i, runner) in &phase2 {
                let result = self.run_one(runner, project, project_root, on_start);
                phase2_results.push((*i, result));
            }
        }

        phase1_results.append(&mut phase2_results);
        phase1_results
    }

    /// Run a single runner, handling unavailable/cached/error cases.
    fn run_one<F>(
        &self,
        runner: &&dyn TestRunner,
        project: &ProjectInfo,
        project_root: &Path,
        on_start: &F,
    ) -> LayerResult
    where
        F: Fn(&str, &str) + Sync,
    {
        if !runner.is_available(project) {
            return LayerResult {
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
            };
        }

        if !self.no_cache
            && is_cacheable(runner.layer())
            && cache::is_cached(project_root, project.language, runner.name())
        {
            return LayerResult {
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
            };
        }

        on_start(runner.name(), runner.layer().as_str());

        match runner.run(project) {
            Ok(result) => {
                if is_cacheable(runner.layer()) && !matches!(result.status, LayerStatus::Fail) {
                    cache::save_current_hash(project_root, project.language, runner.name());
                }
                result
            }
            Err(e) => LayerResult {
                name: runner.layer().as_str().to_string(),
                runner: runner.name().to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "RUNNER_FAILED".to_string(),
                    message: format!("Runner '{}' failed: {}", runner.name(), e),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: 0,
            },
        }
    }

    pub fn run(&self, project: &ProjectInfo) -> Result<BarzelReport> {
        self.run_with_progress(project, |_, _| {})
    }
}

fn layer_from_str(s: &str) -> Layer {
    match s {
        "logic" => Layer::Logic,
        "structural" => Layer::Structural,
        "hostile" => Layer::Hostile,
        "operational" => Layer::Operational,
        _ => Layer::Logic,
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
            workspace_root: None,
        }
    }

    struct PassRunner { layer: Layer, name: &'static str }
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
        let runner = UnavailableRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let _ = orch.run(&project).unwrap();
    }

    #[test]
    fn fail_fast_runs_sequentially_and_stops_on_first_failure() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        // Logic fails, then Hostile is registered but must NOT run
        let fail = FailRunner { layer: Layer::Logic };
        let pass = PassRunner { layer: Layer::Hostile, name: "hostile-runner" };
        let orch = VerificationOrchestrator::new(vec![
            &fail as &dyn TestRunner,
            &pass as &dyn TestRunner,
        ]).with_fail_fast();
        let report = orch.run(&project).unwrap();
        // Only the failing layer — hostile was never called
        assert_eq!(report.layers.len(), 1);
        assert!(matches!(report.layers[0].status, LayerStatus::Fail));
    }

    #[test]
    fn fail_fast_stops_on_any_layer_failure_not_just_logic() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let pass = PassRunner { layer: Layer::Logic, name: "logic-pass" };
        let fail = FailRunner { layer: Layer::Hostile };
        let pass2 = PassRunner { layer: Layer::Structural, name: "struct-pass" };
        let orch = VerificationOrchestrator::new(vec![
            &pass as &dyn TestRunner,
            &fail as &dyn TestRunner,
            &pass2 as &dyn TestRunner,
        ]).with_fail_fast();
        let report = orch.run(&project).unwrap();
        // Stopped after hostile failure — structural never ran
        assert_eq!(report.layers.len(), 2);
        assert!(report.layers.iter().any(|l| matches!(l.status, LayerStatus::Fail)));
        assert!(!report.layers.iter().any(|l| l.runner == "struct-pass"));
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

    #[test]
    fn results_are_sorted_by_layer_priority() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        // Register hostile before logic — output should still be logic first
        let hostile = PassRunner { layer: Layer::Hostile, name: "hostile-runner" };
        let logic = PassRunner { layer: Layer::Logic, name: "logic-runner" };
        let orch = VerificationOrchestrator::new(vec![
            &hostile as &dyn TestRunner,
            &logic as &dyn TestRunner,
        ]);
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers[0].name, "logic");
        assert_eq!(report.layers[1].name, "hostile");
    }

    #[test]
    fn structural_layer_is_cached_after_successful_run() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(crate::cache::is_cached(dir.path(), Language::Rust, "mock-mutants"));
    }

    #[test]
    fn logic_layer_is_not_cached() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Logic, name: "mock-proptest" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(!crate::cache::is_cached(dir.path(), Language::Rust, "mock-proptest"));
    }

    #[test]
    fn hostile_layer_is_not_cached() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Hostile, name: "mock-semgrep" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(!crate::cache::is_cached(dir.path(), Language::Rust, "mock-semgrep"));
    }

    #[test]
    fn failed_structural_run_does_not_write_cache() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = FailRunner { layer: Layer::Structural };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        orch.run(&project).unwrap();
        assert!(!crate::cache::is_cached(dir.path(), Language::Rust, "fail-runner"));
    }

    #[test]
    fn cached_structural_layer_is_skipped() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        crate::cache::save_current_hash(dir.path(), Language::Rust, "mock-mutants");
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let report = orch.run(&project).unwrap();
        assert!(matches!(report.layers[0].status, LayerStatus::Skipped));
        assert_eq!(report.layers[0].findings[0].code, "CACHED");
    }

    #[test]
    fn no_cache_flag_bypasses_cached_layer() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        crate::cache::save_current_hash(dir.path(), Language::Rust, "mock-mutants");
        let runner = PassRunner { layer: Layer::Structural, name: "mock-mutants" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]).with_no_cache();
        let report = orch.run(&project).unwrap();
        assert!(matches!(report.layers[0].status, LayerStatus::Pass));
    }

    #[test]
    fn runner_error_produces_fail_layer_with_failed_metric() {
        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = ErrorRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);
        let report = orch.run(&project).unwrap();
        assert_eq!(report.layers.len(), 1);
        assert!(matches!(report.layers[0].status, LayerStatus::Fail));
        assert_eq!(report.layers[0].metrics.failed, 1);
        assert!(report.layers[0].findings[0].code.contains("RUNNER_FAILED"));
    }

    #[test]
    fn logic_and_hostile_run_in_parallel() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        static CONCURRENT_COUNT: AtomicU32 = AtomicU32::new(0);
        static MAX_CONCURRENT: AtomicU32 = AtomicU32::new(0);

        struct SlowRunner { layer: Layer, name: &'static str }
        impl TestRunner for SlowRunner {
            fn name(&self) -> &'static str { self.name }
            fn layer(&self) -> Layer { self.layer }
            fn is_available(&self, _: &ProjectInfo) -> bool { true }
            fn run(&self, _: &ProjectInfo) -> Result<LayerResult> {
                let count = CONCURRENT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_CONCURRENT.fetch_max(count, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                CONCURRENT_COUNT.fetch_sub(1, Ordering::SeqCst);
                Ok(LayerResult {
                    name: self.layer.as_str().to_string(),
                    runner: self.name.to_string(),
                    status: LayerStatus::Pass,
                    findings: vec![],
                    metrics: LayerMetrics::default(),
                    duration_ms: 20,
                })
            }
        }

        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let logic = SlowRunner { layer: Layer::Logic, name: "slow-logic" };
        let hostile = SlowRunner { layer: Layer::Hostile, name: "slow-hostile" };
        let _ = Arc::new(()); // prevent optimization

        let orch = VerificationOrchestrator::new(vec![
            &logic as &dyn TestRunner,
            &hostile as &dyn TestRunner,
        ]);
        orch.run(&project).unwrap();

        // Both ran concurrently — max concurrent count should be 2
        assert_eq!(MAX_CONCURRENT.load(Ordering::SeqCst), 2,
            "Logic and Hostile should run in parallel");
    }
}
