use crate::detect::ProjectInfo;
use crate::plugin::TestRunner;
use crate::report::{BarzelReport, LayerResult};
use crate::error::Result;

/// The main orchestrator that runs a set of TestRunners against a project.
/// This is the core of Barzel's plugin architecture.
pub struct VerificationOrchestrator<'a> {
    runners: Vec<&'a dyn TestRunner>,
}

impl<'a> VerificationOrchestrator<'a> {
    pub fn new(runners: Vec<&'a dyn TestRunner>) -> Self {
        Self { runners }
    }

    pub fn run(&self, project: &ProjectInfo) -> Result<BarzelReport> {
        let mut report = BarzelReport::new(project.clone());

        for runner in &self.runners {
            if runner.is_available(project) {
                match runner.run(project) {
                    Ok(layer_result) => {
                        report.add_layer(layer_result);
                    }
                    Err(e) => {
                        // Create a failure layer result
                        let failure = LayerResult {
                            name: runner.name().to_string(),
                            status: crate::report::LayerStatus::Fail,
                            findings: vec![crate::report::Finding {
                                severity: crate::report::Severity::Critical,
                                code: "RUNNER_FAILED".to_string(),
                                message: format!("Runner '{}' failed: {}", runner.name(), e),
                                location: None,
                            }],
                            metrics: crate::report::LayerMetrics {
                                tests_run: 0,
                                passed: 0,
                                failed: 1,
                                coverage: None,
                                mutation_score: None,
                            },
                            duration_ms: 0,
                        };
                        report.add_layer(failure);
                    }
                }
            }
        }

        Ok(report)
    }
}
