use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct BanditRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for BanditRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for BanditRunner {
    fn name(&self) -> &'static str { "bandit" }
    fn layer(&self) -> Layer { Layer::Hostile }

    fn skip_message(&self) -> &'static str {
        "bandit not installed — run `pip install bandit` for Python security analysis (SQLi, shell injection, hardcoded passwords, etc.)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Python { return false; }
        let root = Path::new(&project.root);
        let local = root.join(".venv").join("bin").join("bandit");
        if local.exists() { return true; }
        self.proc.is_available("bandit", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let local = root.join(".venv").join("bin").join("bandit");
        let bandit_cmd = if local.exists() {
            local.to_string_lossy().to_string()
        } else {
            "bandit".to_string()
        };

        match self.proc.run(&bandit_cmd, &["-r", ".", "-f", "json", "-q"], root) {
            Ok(out) => {
                let combined = out.combined();
                let findings = parse_bandit_json(&combined);

                let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical)) {
                    LayerStatus::Fail
                } else if findings.iter().any(|f| matches!(f.severity, Severity::High | Severity::Medium)) {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                let findings = if findings.is_empty() {
                    vec![Finding {
                        severity: Severity::Info,
                        code: "BANDIT_PASSED".to_string(),
                        message: "No security issues detected by bandit".to_string(),
                        suggestion: Some(
                            "Consider also running `safety check` to audit Python dependency vulnerabilities."
                                .to_string(),
                        ),
                        ..Default::default()
                    }]
                } else {
                    findings
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "bandit".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "bandit".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "BANDIT_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run bandit: {}", e),
                    reproduce_cmd: Some(format!("{bandit_cmd} -r . 2>&1")),
                    suggestion: Some(
                        "Install: `pip install bandit` or `uv add --dev bandit`."
                            .to_string(),
                    ),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

fn parse_bandit_json(output: &str) -> Vec<Finding> {
    // bandit JSON: {"results": [{"test_id": "B105", "issue_severity": "LOW", "issue_confidence": "MEDIUM", "issue_text": "...", "filename": "...", "line_number": 1}]}
    for line in output.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(results) = json.get("results").and_then(|r| r.as_array()) {
                return results.iter().filter_map(bandit_item_to_finding).collect();
            }
        }
    }
    // Try parsing the whole output as JSON (not line-by-line)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        if let Some(results) = json.get("results").and_then(|r| r.as_array()) {
            return results.iter().filter_map(bandit_item_to_finding).collect();
        }
    }
    vec![]
}

fn bandit_item_to_finding(item: &serde_json::Value) -> Option<Finding> {
    let severity_str = item.get("issue_severity").and_then(|s| s.as_str()).unwrap_or("LOW");
    let severity = match severity_str.to_uppercase().as_str() {
        "HIGH" => Severity::High,
        "MEDIUM" => Severity::Medium,
        _ => Severity::Low,
    };

    let code = item.get("test_id").and_then(|c| c.as_str()).unwrap_or("BANDIT").to_string();
    let message = item.get("issue_text").and_then(|m| m.as_str()).unwrap_or("Security issue").to_string();
    let filename = item.get("filename").and_then(|f| f.as_str()).unwrap_or("");
    let line = item.get("line_number").and_then(|l| l.as_u64()).unwrap_or(0);

    Some(Finding {
        severity,
        code,
        message,
        location: if filename.is_empty() { None } else { Some(format!("{}:{}", filename, line)) },
        reproduce_cmd: Some(format!("bandit -r . 2>&1 | grep -A 5 '{}'", filename)),
        suggestion: item.get("more_info")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;

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

    #[test]
    fn name_is_bandit() { assert_eq!(BanditRunner::default().name(), "bandit"); }

    #[test]
    fn layer_is_hostile() { assert!(matches!(BanditRunner::default().layer(), Layer::Hostile)); }

    #[test]
    fn not_available_for_rust() {
        let r = BanditRunner { proc: Arc::new(MockProcessRunner::passing("")) };
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn available_when_bandit_present() {
        let r = BanditRunner { proc: Arc::new(MockProcessRunner::passing("bandit 1.7.5")) };
        assert!(r.is_available(&py_info()));
    }

    #[test]
    fn clean_project_returns_pass_with_info_finding() {
        let json = r#"{"results": [], "metrics": {}}"#;
        let r = BanditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "BANDIT_PASSED"));
    }

    #[test]
    fn high_severity_issue_returns_partial() {
        let json = r#"{"results": [{"test_id": "B201", "issue_severity": "HIGH", "issue_confidence": "HIGH", "issue_text": "Use of assert detected.", "filename": "app.py", "line_number": 5}]}"#;
        let r = BanditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.findings[0].severity, Severity::High);
    }

    #[test]
    fn parses_location_from_result() {
        let json = r#"{"results": [{"test_id": "B105", "issue_severity": "MEDIUM", "issue_text": "Hardcoded password", "filename": "config.py", "line_number": 12}]}"#;
        let r = BanditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&py_info()).unwrap();
        assert!(result.findings[0].location.as_ref().unwrap().contains("config.py:12"));
    }

    #[test]
    fn execution_failure_returns_critical_finding() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
            }
        }
        let r = BanditRunner { proc: Arc::new(BrokenProc) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }
}
