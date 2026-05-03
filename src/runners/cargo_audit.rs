/// cargo audit runner — scans Rust dependencies for known CVEs via RustSec.
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct CargoAuditRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for CargoAuditRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for CargoAuditRunner {
    fn name(&self) -> &'static str { "cargo-audit" }
    fn layer(&self) -> Layer { Layer::Hostile }

    fn skip_message(&self) -> &'static str {
        "cargo-audit not installed — run `cargo install cargo-audit` to scan Rust dependencies for CVEs"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust { return false; }
        let root = Path::new(&project.root);
        // Needs Cargo.lock to operate
        if !root.join("Cargo.lock").exists() { return false; }
        self.proc.is_available("cargo", &["audit", "--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        match self.proc.run("cargo", &["audit", "--json"], root) {
            Ok(out) => {
                let combined = out.combined();
                let findings = parse_cargo_audit_json(&combined);

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
                        code: "CARGO_AUDIT_PASSED".to_string(),
                        message: "No known vulnerabilities in Rust dependencies".to_string(),
                        suggestion: Some("Keep dependencies updated: `cargo update` and review with `cargo outdated`".to_string()),
                        ..Default::default()
                    }]
                } else {
                    findings
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "cargo-audit".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "cargo-audit".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "CARGO_AUDIT_FAILED".to_string(),
                    message: format!("Failed to run cargo audit: {}", e),
                    reproduce_cmd: Some("cargo audit 2>&1".to_string()),
                    suggestion: Some("Install: `cargo install cargo-audit`".to_string()),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_cargo_audit_json(output: &str) -> Vec<Finding> {
    // cargo audit JSON: {"vulnerabilities":{"list":[{"advisory":{"id":"RUSTSEC-2023-X","title":"...","severity":"high"},"package":{"name":"...","version":"..."}}]}}
    for line in output.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(findings) = extract_cargo_findings(&json) {
                return findings;
            }
        }
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        return extract_cargo_findings(&json).unwrap_or_default();
    }
    vec![]
}

fn extract_cargo_findings(json: &serde_json::Value) -> Option<Vec<Finding>> {
    let list = json
        .get("vulnerabilities")
        .and_then(|v| v.get("list"))
        .and_then(|l| l.as_array())?;

    let findings: Vec<Finding> = list.iter().filter_map(|item| {
        let advisory = item.get("advisory")?;
        let pkg = item.get("package")?;

        let id = advisory.get("id").and_then(|i| i.as_str()).unwrap_or("UNKNOWN");
        let title = advisory.get("title").and_then(|t| t.as_str()).unwrap_or("Vulnerability");
        let severity_str = advisory.get("severity").and_then(|s| s.as_str()).unwrap_or("medium");
        let severity = match severity_str {
            "critical" => Severity::Critical,
            "high" => Severity::High,
            "medium" | "moderate" => Severity::Medium,
            _ => Severity::Low,
        };

        let pkg_name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
        let pkg_version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        let patched = advisory.get("patched_versions")
            .and_then(|p| p.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str());

        Some(Finding {
            severity,
            code: format!("RUSTSEC_{}", id.replace('-', "_")),
            message: format!("{}: {} ({}@{})", id, title, pkg_name, pkg_version),
            reproduce_cmd: Some(format!("cargo audit 2>&1 | grep -A 10 '{id}'")),
            suggestion: Some(match patched {
                Some(v) => format!("Upgrade `{}` to {} in Cargo.toml", pkg_name, v),
                None => format!("No patch available for `{}` yet. Consider replacing or pinning to a safe version.", pkg_name),
            }),
            ..Default::default()
        })
    }).collect();

    Some(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn rust_info(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::Rust, root: root.to_string(), has_tests: true, package_name: None, frameworks: Default::default() }
    }

    #[test]
    fn name_is_cargo_audit() { assert_eq!(CargoAuditRunner::default().name(), "cargo-audit"); }

    #[test]
    fn layer_is_hostile() { assert!(matches!(CargoAuditRunner::default().layer(), Layer::Hostile)); }

    #[test]
    fn not_available_for_typescript() {
        let r = CargoAuditRunner { proc: Arc::new(MockProcessRunner::passing("")) };
        let i = ProjectInfo { language: Language::TypeScript, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default() };
        assert!(!r.is_available(&i));
    }

    #[test]
    fn not_available_without_cargo_lock() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        // No Cargo.lock
        let r = CargoAuditRunner { proc: Arc::new(MockProcessRunner::passing("cargo-audit 0.18")) };
        assert!(!r.is_available(&rust_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_cargo_lock() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), b"").unwrap();
        let r = CargoAuditRunner { proc: Arc::new(MockProcessRunner::passing("cargo-audit 0.18")) };
        assert!(r.is_available(&rust_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn clean_project_returns_pass() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), b"").unwrap();
        let json = r#"{"vulnerabilities":{"list":[],"count":0},"warnings":{}}"#;
        let r = CargoAuditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&rust_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "CARGO_AUDIT_PASSED"));
    }

    #[test]
    fn vulnerability_found_returns_fail() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), b"").unwrap();
        let json = r#"{"vulnerabilities":{"list":[{"advisory":{"id":"RUSTSEC-2023-0001","title":"Unsound use of transmute","severity":"high","patched_versions":[">=1.2.0"]},"package":{"name":"mylib","version":"1.0.0"}}],"count":1}}"#;
        let r = CargoAuditRunner { proc: Arc::new(MockProcessRunner::failing(json)) };
        let result = r.run(&rust_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert!(result.findings[0].message.contains("RUSTSEC-2023-0001"));
        assert!(result.findings[0].suggestion.as_ref().unwrap().contains("1.2.0"));
    }

    #[test]
    fn parse_extracts_severity_and_fix() {
        let json = r#"{"vulnerabilities":{"list":[{"advisory":{"id":"RUSTSEC-2023-0042","title":"Buffer overflow","severity":"critical","patched_versions":[">=2.0.0"]},"package":{"name":"unsafe-lib","version":"1.0.0"}}]}}"#;
        let findings = parse_cargo_audit_json(json);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert!(findings[0].message.contains("unsafe-lib@1.0.0"));
        assert!(findings[0].suggestion.as_ref().unwrap().contains("2.0.0"));
    }
}
