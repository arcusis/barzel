use crate::config::HealthCheckConfig;
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::http::{HttpClient, UreqClient};
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::sync::Arc;
use std::time::Instant;

pub struct HealthCheckRunner {
    checks: Vec<HealthCheckConfig>,
    client: Arc<dyn HttpClient>,
}

impl HealthCheckRunner {
    pub fn new(checks: Vec<HealthCheckConfig>) -> Self {
        Self { checks, client: Arc::new(UreqClient) }
    }
}

impl TestRunner for HealthCheckRunner {
    fn name(&self) -> &'static str { "health-check" }
    fn layer(&self) -> Layer { Layer::Operational }

    fn skip_message(&self) -> &'static str {
        "no health_checks configured — add [[layers.operational.health_checks]] to .barzel.toml"
    }

    fn is_available(&self, _project: &ProjectInfo) -> bool {
        !self.checks.is_empty()
    }

    fn run(&self, _project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let mut findings = Vec::new();
        let mut passed: u64 = 0;
        let mut failed: u64 = 0;

        for check in &self.checks {
            let timeout_secs = check.timeout_ms as f64 / 1000.0;
            let reproduce_cmd = format!(
                "curl -i --max-time {:.1} {}",
                timeout_secs, check.url
            );

            match self.client.get(&check.url, check.timeout_ms) {
                Ok(status) if status == check.expected_status => {
                    passed += 1;
                }
                Ok(actual_status) => {
                    failed += 1;
                    findings.push(Finding {
                        severity: Severity::High,
                        code: "HEALTH_CHECK_STATUS_MISMATCH".to_string(),
                        message: format!(
                            "Health check '{}' returned HTTP {} (expected {})",
                            check.name, actual_status, check.expected_status
                        ),
                        location: Some(check.url.clone()),
                        reproduce_cmd: Some(reproduce_cmd),
                        suggestion: Some(format!(
                            "Investigate why {} returns {}. \
                             Check application logs and service health.",
                            check.url, actual_status
                        )),
                    });
                }
                Err(err) => {
                    failed += 1;
                    findings.push(Finding {
                        severity: Severity::Critical,
                        code: "HEALTH_CHECK_UNREACHABLE".to_string(),
                        message: format!(
                            "Health check '{}' failed to connect: {}",
                            check.name, err
                        ),
                        location: Some(check.url.clone()),
                        reproduce_cmd: Some(reproduce_cmd),
                        suggestion: Some(format!(
                            "Ensure the service at {} is running and reachable. \
                             Verify network connectivity and that the service is started.",
                            check.url
                        )),
                    });
                }
            }
        }

        let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical)) {
            LayerStatus::Fail
        } else if findings.iter().any(|f| matches!(f.severity, Severity::High)) {
            LayerStatus::Partial
        } else {
            LayerStatus::Pass
        };

        Ok(LayerResult {
            name: "operational".to_string(),
            runner: "health-check".to_string(),
            status,
            findings,
            metrics: LayerMetrics {
                tests_run: self.checks.len() as u64,
                passed,
                failed,
                ..Default::default()
            },
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::http::MockHttpClient;

    fn project() -> ProjectInfo {
        ProjectInfo {
            language: Language::Unknown,
            root: "/tmp".to_string(),
            has_tests: false,
            package_name: None,
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    fn check(name: &str, url: &str) -> HealthCheckConfig {
        HealthCheckConfig {
            name: name.to_string(),
            url: url.to_string(),
            expected_status: 200,
            timeout_ms: 1000,
        }
    }

    fn runner_with(checks: Vec<HealthCheckConfig>, client: MockHttpClient) -> HealthCheckRunner {
        HealthCheckRunner { checks, client: Arc::new(client) }
    }

    // ── metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn name_is_health_check() {
        assert_eq!(HealthCheckRunner::new(vec![]).name(), "health-check");
    }

    #[test]
    fn layer_is_operational() {
        assert!(matches!(HealthCheckRunner::new(vec![]).layer(), Layer::Operational));
    }

    #[test]
    fn not_available_when_no_checks() {
        assert!(!HealthCheckRunner::new(vec![]).is_available(&project()));
    }

    #[test]
    fn available_when_checks_configured() {
        assert!(HealthCheckRunner::new(vec![check("api", "http://localhost:3000/health")]).is_available(&project()));
    }

    // ── pass ──────────────────────────────────────────────────────────────────

    #[test]
    fn pass_when_expected_status_matches() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_ok(200),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
        assert_eq!(result.metrics.tests_run, 1);
        assert_eq!(result.metrics.passed, 1);
        assert_eq!(result.metrics.failed, 0);
    }

    #[test]
    fn all_pass_counted_in_metrics() {
        let r = runner_with(
            vec![
                check("api", "http://localhost:3000/health"),
                check("db", "http://localhost:5432/health"),
            ],
            MockHttpClient::always_ok(200),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.metrics.tests_run, 2);
        assert_eq!(result.metrics.passed, 2);
        assert_eq!(result.metrics.failed, 0);
    }

    // ── status mismatch ───────────────────────────────────────────────────────

    #[test]
    fn status_mismatch_is_high_severity() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_ok(503),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::High);
        assert_eq!(result.findings[0].code, "HEALTH_CHECK_STATUS_MISMATCH");
        assert!(result.findings[0].message.contains("503"));
        assert!(result.findings[0].message.contains("200"));
    }

    #[test]
    fn status_mismatch_has_reproduce_cmd() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_ok(500),
        );
        let result = r.run(&project()).unwrap();
        let cmd = result.findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(cmd.contains("curl"), "reproduce_cmd must use curl");
        assert!(cmd.contains("http://localhost:3000/health"));
    }

    #[test]
    fn status_mismatch_layer_is_partial() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_ok(404),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.metrics.failed, 1);
    }

    // ── client error ──────────────────────────────────────────────────────────

    #[test]
    fn client_error_is_critical_severity() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_err("connection refused"),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert_eq!(result.findings[0].code, "HEALTH_CHECK_UNREACHABLE");
        assert!(result.findings[0].message.contains("connection refused"));
    }

    #[test]
    fn client_error_has_reproduce_cmd() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_err("timeout"),
        );
        let result = r.run(&project()).unwrap();
        let cmd = result.findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("http://localhost:3000/health"));
        assert!(cmd.contains("--max-time"));
    }

    #[test]
    fn client_error_layer_is_fail() {
        let r = runner_with(
            vec![check("api", "http://localhost:3000/health")],
            MockHttpClient::always_err("dns error"),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.failed, 1);
    }

    // ── mixed results ─────────────────────────────────────────────────────────

    #[test]
    fn mixed_pass_and_mismatch_partial_status() {
        let r = runner_with(
            vec![
                check("api", "http://localhost:3000/health"),
                check("db", "http://localhost:5432/health"),
            ],
            MockHttpClient::new(vec![Ok(200), Ok(503)]),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.metrics.passed, 1);
        assert_eq!(result.metrics.failed, 1);
    }

    #[test]
    fn critical_error_dominates_high_mismatch() {
        let r = runner_with(
            vec![
                check("api", "http://localhost:3000/health"),
                check("db", "http://localhost:5432/health"),
            ],
            MockHttpClient::new(vec![Ok(404), Err("refused".to_string())]),
        );
        let result = r.run(&project()).unwrap();
        // Fail because Critical finding present
        assert!(matches!(result.status, LayerStatus::Fail));
    }
}
