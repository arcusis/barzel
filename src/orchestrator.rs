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
        self.run_with_events(project, |event| {
            if let RunnerEvent::Started { runner, layer } = event {
                on_start(runner, layer);
            }
        })
    }

    #[cfg(test)]
    pub fn run(&self, project: &ProjectInfo) -> Result<BarzelReport> {
        self.run_with_progress(project, |_, _| {})
    }

    /// Run with a richer per-runner event callback.
    /// The callback receives a [`RunnerEvent`] before each runner starts and after it completes.
    /// Human spinner callers should continue using [`run_with_progress`]; this is for
    /// structured consumers such as the stdio protocol.
    pub fn run_with_events<F>(&self, project: &ProjectInfo, on_event: F) -> Result<BarzelReport>
    where
        F: Fn(RunnerEvent<'_>) + Sync,
    {
        // Adapt: fire Started before run_one, Completed after.
        // run_with_progress only fires on_start; we wrap it with a timing layer here
        // by using a different dispatch path that records timestamps around run_one.
        let project_root = Path::new(&project.root);
        let indexed_results: Vec<(usize, LayerResult)> = if self.fail_fast {
            self.run_sequential_with_events(project, project_root, &on_event)
        } else {
            self.run_parallel_with_events(project, project_root, &on_event)
        };

        let mut sorted = indexed_results;
        sorted.sort_by_key(|(i, r)| (layer_priority(layer_from_str(&r.name)), *i));

        let mut report = BarzelReport::new(project.clone());
        for (_, result) in sorted {
            report.add_layer(result);
        }
        Ok(report)
    }

    fn run_sequential_with_events<F>(
        &self,
        project: &ProjectInfo,
        project_root: &Path,
        on_event: &F,
    ) -> Vec<(usize, LayerResult)>
    where
        F: Fn(RunnerEvent<'_>) + Sync,
    {
        let mut results = Vec::new();
        for (i, runner) in self.runners.iter().enumerate() {
            let result = self.run_one_with_events(runner, project, project_root, on_event);
            let is_fail = matches!(result.status, LayerStatus::Fail);
            results.push((i, result));
            if is_fail { break; }
        }
        results
    }

    fn run_parallel_with_events<F>(
        &self,
        project: &ProjectInfo,
        project_root: &Path,
        on_event: &F,
    ) -> Vec<(usize, LayerResult)>
    where
        F: Fn(RunnerEvent<'_>) + Sync,
    {
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
                    scope.spawn(move || (i, self.run_one_with_events(runner, project, project_root, on_event)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let logic_failed = phase1_results.iter().any(|(_, r)| {
            matches!(r.status, LayerStatus::Fail) && r.name == "logic"
        });

        let mut phase2_results = Vec::new();
        if !logic_failed {
            for (i, runner) in &phase2 {
                let result = self.run_one_with_events(runner, project, project_root, on_event);
                phase2_results.push((*i, result));
            }
        }

        phase1_results.append(&mut phase2_results);
        phase1_results
    }

    fn run_one_with_events<F>(
        &self,
        runner: &&dyn TestRunner,
        project: &ProjectInfo,
        project_root: &Path,
        on_event: &F,
    ) -> LayerResult
    where
        F: Fn(RunnerEvent<'_>) + Sync,
    {
        // Unavailable and cached runners skip without events (nothing actually runs).
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

        on_event(RunnerEvent::Started {
            runner: runner.name(),
            layer: runner.layer().as_str(),
        });

        let result = match runner.run(project) {
            Ok(r) => {
                if is_cacheable(runner.layer()) && !matches!(r.status, LayerStatus::Fail) {
                    cache::save_current_hash(project_root, project.language, runner.name());
                }
                r
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
        };

        let status_str = match result.status {
            LayerStatus::Pass => "pass",
            LayerStatus::Partial => "partial",
            LayerStatus::Fail => "fail",
            LayerStatus::Skipped => "skipped",
        };
        on_event(RunnerEvent::Completed {
            runner: runner.name(),
            layer: runner.layer().as_str(),
            status: status_str,
            duration_ms: result.duration_ms,
        });

        result
    }
}

/// A progress event emitted by [`VerificationOrchestrator::run_with_events`].
#[derive(Debug)]
pub enum RunnerEvent<'a> {
    /// Fired immediately before a runner's subprocess is invoked.
    Started {
        runner: &'a str,
        layer: &'a str,
    },
    /// Fired immediately after a runner returns (pass, partial, or fail).
    Completed {
        runner: &'a str,
        layer: &'a str,
        /// The layer status string from the completed [`LayerResult`].
        status: &'a str,
        duration_ms: u64,
    },
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

    // ── run_with_events ───────────────────────────────────────────────────────

    #[test]
    fn run_with_events_emits_started_and_completed_for_available_runner() {
        use super::RunnerEvent;
        use std::sync::{Arc, Mutex};

        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Logic, name: "mock-logic" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_cb = events.clone();

        orch.run_with_events(&project, move |event| match event {
            RunnerEvent::Started { runner, layer } => {
                events_cb.lock().unwrap().push(format!("started:{}:{}", runner, layer));
            }
            RunnerEvent::Completed { runner, layer, status, .. } => {
                events_cb.lock().unwrap().push(format!("completed:{}:{}:{}", runner, layer, status));
            }
        }).unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2, "expected exactly one started and one completed event");
        assert_eq!(captured[0], "started:mock-logic:logic");
        assert!(captured[1].starts_with("completed:mock-logic:logic:"), "completed event must carry status: {:?}", &*captured);
    }

    #[test]
    fn run_with_events_started_comes_before_completed() {
        use super::RunnerEvent;
        use std::sync::{Arc, Mutex};

        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Logic, name: "mock-logic" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);

        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let order_cb = order.clone();

        orch.run_with_events(&project, move |event| {
            order_cb.lock().unwrap().push(match event {
                RunnerEvent::Started { .. } => "started",
                RunnerEvent::Completed { .. } => "completed",
            });
        }).unwrap();

        assert_eq!(*order.lock().unwrap(), vec!["started", "completed"]);
    }

    #[test]
    fn run_with_events_no_events_for_unavailable_runner() {
        use super::RunnerEvent;
        use std::sync::{Arc, Mutex};

        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = UnavailableRunner;
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);

        let count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let count_cb = count.clone();

        orch.run_with_events(&project, move |_: RunnerEvent<'_>| {
            *count_cb.lock().unwrap() += 1;
        }).unwrap();

        assert_eq!(*count.lock().unwrap(), 0, "unavailable runner must emit no events");
    }

    #[test]
    fn run_with_events_completed_carries_duration_and_pass_status() {
        use super::RunnerEvent;
        use std::sync::{Arc, Mutex};

        let dir = tempdir().unwrap();
        let project = rust_project(dir.path());
        let runner = PassRunner { layer: Layer::Logic, name: "mock-logic" };
        let orch = VerificationOrchestrator::new(vec![&runner as &dyn TestRunner]);

        let completed_status: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let status_cb = completed_status.clone();
        let completed_duration: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let duration_cb = completed_duration.clone();

        orch.run_with_events(&project, move |event| {
            if let RunnerEvent::Completed { status, duration_ms, .. } = event {
                *status_cb.lock().unwrap() = Some(status.to_string());
                *duration_cb.lock().unwrap() = Some(duration_ms);
            }
        }).unwrap();

        let status = completed_status.lock().unwrap().clone().expect("completed event must fire");
        assert_eq!(status, "pass");
        assert_eq!(*completed_duration.lock().unwrap(), Some(1));
    }
}
