/// pip-audit runner — scans Python dependencies for known CVEs.
/// Uses pip-audit (PyPA) which checks against the OSV database.
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct PipAuditRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for PipAuditRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for PipAuditRunner {
    fn name(&self) -> &'static str { "pip-audit" }
    fn layer(&self) -> Layer { Layer::Hostile }

    fn skip_message(&self) -> &'static str {
        "pip-audit not installed — run `pip install pip-audit` to scan Python dependencies for CVEs"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Python { return false; }
        let root = Path::new(&project.root);
        let local = root.join(".venv").join("bin").join("pip-audit");
        if local.exists() { return true; }
        self.proc.is_available("pip-audit", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let local = root.join(".venv").join("bin").join("pip-audit");
        let cmd = if local.exists() {
            local.to_string_lossy().to_string()
        } else {
            "pip-audit".to_string()
        };

        match self.proc.run(&cmd, &["--format=json", "--progress-spinner=off"], root) {
            Ok(out) => {
                let combined = out.combined();
                let findings = parse_pip_audit_json(&combined);

                let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical | Severity::High)) {
                    LayerStatus::Fail
                } else if findings.iter().any(|f| matches!(f.severity, Severity::Medium)) {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                let findings = if findings.is_empty() {
                    vec![Finding {
                        severity: Severity::Info,
                        code: "PIP_AUDIT_PASSED".to_string(),
                        message: "No known vulnerabilities in Python dependencies".to_string(),
                        suggestion: Some("Keep dependencies updated: `pip list --outdated`".to_string()),
                        ..Default::default()
                    }]
                } else {
                    findings
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "pip-audit".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "pip-audit".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "PIP_AUDIT_FAILED".to_string(),
                    message: format!("Failed to run pip-audit: {}", e),
                    reproduce_cmd: Some(format!("{cmd} --format=json 2>&1")),
                    suggestion: Some("Install: `pip install pip-audit` or `uv add --dev pip-audit`".to_string()),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_pip_audit_json(output: &str) -> Vec<Finding> {
    // pip-audit JSON: {"dependencies":[{"name":"requests","version":"2.25.0","vulns":[{"id":"PYSEC-2023-X","fix_versions":["2.31.0"],"description":"..."}]}]}
    for line in output.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(findings) = extract_pip_findings(&json) {
                if !findings.is_empty() { return findings; }
            }
        }
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        return extract_pip_findings(&json).unwrap_or_default();
    }
    vec![]
}

fn extract_pip_findings(json: &serde_json::Value) -> Option<Vec<Finding>> {
    let deps = json.get("dependencies")?.as_array()?;
    let findings: Vec<Finding> = deps.iter().flat_map(|dep| {
        let pkg = dep.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
        let version = dep.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        let vulns = dep.get("vulns").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        vulns.into_iter().map(move |vuln| {
            let id = vuln.get("id").and_then(|i| i.as_str()).unwrap_or("UNKNOWN");
            let desc = vuln.get("description").and_then(|d| d.as_str()).unwrap_or("Vulnerability detected");
            let fix_versions: Vec<&str> = vuln.get("fix_versions")
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            // Treat all pip-audit findings as High unless we can determine otherwise
            let severity = Severity::High;

            Finding {
                severity,
                code: format!("CVE_{}", id.replace('-', "_")),
                message: format!("{} {}@{}: {}", id, pkg, version, desc),
                reproduce_cmd: Some(format!("pip-audit --format=json 2>&1 | jq '.dependencies[] | select(.name==\"{pkg}\")'").to_string()),
                suggestion: Some(if fix_versions.is_empty() {
                    format!("No fix available yet for `{}`. Monitor for updates.", pkg)
                } else {
                    format!("Upgrade `{}` to {} or later: `pip install {}=={}`", pkg, fix_versions[0], pkg, fix_versions[0])
                }),
                ..Default::default()
            }
        })
    }).collect();
    Some(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;

    fn py_info() -> ProjectInfo {
        ProjectInfo { language: Language::Python, root: "/tmp".to_string(), has_tests: true, package_name: None, frameworks: Default::default() }
    }

    #[test]
    fn name_is_pip_audit() { assert_eq!(PipAuditRunner::default().name(), "pip-audit"); }

    #[test]
    fn layer_is_hostile() { assert!(matches!(PipAuditRunner::default().layer(), Layer::Hostile)); }

    #[test]
    fn not_available_for_rust() {
        let r = PipAuditRunner { proc: Arc::new(MockProcessRunner::passing("")) };
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default() };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn available_when_pip_audit_installed() {
        let r = PipAuditRunner { proc: Arc::new(MockProcessRunner::passing("pip-audit 2.4.0")) };
        assert!(r.is_available(&py_info()));
    }

    #[test]
    fn clean_project_returns_pass() {
        let json = r#"{"dependencies":[]}"#;
        let r = PipAuditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "PIP_AUDIT_PASSED"));
    }

    #[test]
    fn vulnerability_found_returns_fail() {
        let json = r#"{"dependencies":[{"name":"requests","version":"2.25.0","vulns":[{"id":"PYSEC-2023-74","fix_versions":["2.31.0"],"description":"Unverified HTTPS requests"}]}]}"#;
        let r = PipAuditRunner { proc: Arc::new(MockProcessRunner::failing(json)) };
        let result = r.run(&py_info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert!(result.findings[0].message.contains("requests"));
        assert!(result.findings[0].suggestion.as_ref().unwrap().contains("2.31.0"));
    }

    #[test]
    fn parse_extracts_package_and_fix_version() {
        let json = r#"{"dependencies":[{"name":"flask","version":"1.0.0","vulns":[{"id":"CVE-2023-1","fix_versions":["2.0.0"],"description":"XSS vulnerability"}]}]}"#;
        let findings = parse_pip_audit_json(json);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("flask@1.0.0"));
        assert!(findings[0].suggestion.as_ref().unwrap().contains("2.0.0"));
    }

    #[test]
    fn no_vulns_package_is_clean() {
        let json = r#"{"dependencies":[{"name":"requests","version":"2.31.0","vulns":[]}]}"#;
        let findings = parse_pip_audit_json(json);
        assert!(findings.is_empty());
    }
}
