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
