/// npm audit runner — scans Node.js/TypeScript dependencies for known CVEs.
/// Runs on TypeScript projects with a package-lock.json or pnpm-lock.yaml.
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct NpmAuditRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for NpmAuditRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for NpmAuditRunner {
    fn name(&self) -> &'static str { "npm-audit" }
    fn layer(&self) -> Layer { Layer::Hostile }

    fn skip_message(&self) -> &'static str {
        "npm-audit requires a lockfile — run `npm install` or `pnpm install` to generate one"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript { return false; }
        let root = Path::new(&project.root);
        // Needs a lockfile to operate
        root.join("package-lock.json").exists()
            || root.join("pnpm-lock.yaml").exists()
            || root.join("yarn.lock").exists()
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let (cmd, args): (&str, &[&str]) = if root.join("pnpm-lock.yaml").exists() {
            ("pnpm", &["audit", "--json"])
        } else if root.join("yarn.lock").exists() {
            ("yarn", &["audit", "--json"])
        } else {
            ("npm", &["audit", "--json"])
        };

        match self.proc.run(cmd, args, root) {
            Ok(out) => {
                let combined = out.combined();
                let findings = parse_npm_audit_json(&combined);

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
                        code: "NPM_AUDIT_PASSED".to_string(),
                        message: "No known vulnerabilities in npm dependencies".to_string(),
                        suggestion: Some("Keep dependencies updated: `npx npm-check-updates -u`".to_string()),
                        ..Default::default()
                    }]
                } else {
                    findings
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "npm-audit".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "npm-audit".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "NPM_AUDIT_FAILED".to_string(),
                    message: format!("Failed to run npm audit: {}", e),
                    reproduce_cmd: Some(format!("{cmd} audit 2>&1")),
                    suggestion: Some("Ensure npm/pnpm is installed and `npm install` has been run.".to_string()),
                    ..Default::default()
                }],
                metrics: LayerMetrics { failed: 1, ..Default::default() },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

pub fn parse_npm_audit_json(output: &str) -> Vec<Finding> {
    // Try line-by-line first (mixed stderr)
    for line in output.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(findings) = extract_npm_findings(&json) {
                return findings;
            }
        }
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        return extract_npm_findings(&json).unwrap_or_default();
    }
    vec![]
}

fn extract_npm_findings(json: &serde_json::Value) -> Option<Vec<Finding>> {
    // npm audit JSON v2: {"vulnerabilities": {"pkg": {"severity": "high", "via": [...]}}}
    let vulns = json.get("vulnerabilities")?.as_object()?;
    let findings: Vec<Finding> = vulns.iter().map(|(pkg, vuln)| {
        let severity_str = vuln.get("severity").and_then(|s| s.as_str()).unwrap_or("low");
        let severity = match severity_str {
            "critical" => Severity::Critical,
            "high" => Severity::High,
            "moderate" | "medium" => Severity::Medium,
            _ => Severity::Low,
        };

        let fix_available = vuln.get("fixAvailable")
            .and_then(|f| f.as_bool())
            .unwrap_or(false);

        Finding {
            severity,
            code: format!("NPM_VULN_{}", pkg.to_uppercase().replace('-', "_")),
            message: format!("Vulnerability in `{}` ({})", pkg, severity_str),
            reproduce_cmd: Some(format!("npm audit --json 2>&1 | jq '.vulnerabilities.\"{}\"'", pkg)),
            suggestion: Some(if fix_available {
                "Run `npm audit fix` to auto-fix. Review breaking changes first.".to_string()
            } else {
                "No automatic fix available. Review and replace the dependency.".to_string()
            }),
            ..Default::default()
        }
    }).collect();
    Some(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn ts_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    fn setup_npm_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        dir
    }

    #[test]
    fn name_is_npm_audit() { assert_eq!(NpmAuditRunner::default().name(), "npm-audit"); }

    #[test]
    fn layer_is_hostile() { assert!(matches!(NpmAuditRunner::default().layer(), Layer::Hostile)); }

    #[test]
    fn not_available_for_rust() {
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!NpmAuditRunner::default().is_available(&i));
    }

    #[test]
    fn not_available_without_lockfile() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), b"{}").unwrap();
        assert!(!NpmAuditRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_package_lock() {
        let dir = setup_npm_dir();
        assert!(NpmAuditRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_pnpm_lock() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"lockfileVersion: '6.0'").unwrap();
        assert!(NpmAuditRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn clean_project_returns_pass() {
        let dir = setup_npm_dir();
        let json = r#"{"vulnerabilities":{},"metadata":{"vulnerabilities":{"total":0}}}"#;
        let r = NpmAuditRunner { proc: Arc::new(MockProcessRunner::passing(json)) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "NPM_AUDIT_PASSED"));
    }

    #[test]
    fn critical_vulnerability_returns_fail() {
        let dir = setup_npm_dir();
        let json = r#"{"vulnerabilities":{"lodash":{"severity":"critical","fixAvailable":true}}}"#;
        let r = NpmAuditRunner { proc: Arc::new(MockProcessRunner::failing(json)) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert!(result.findings[0].reproduce_cmd.is_some());
    }

    #[test]
    fn high_severity_returns_partial() {
        let dir = setup_npm_dir();
        let json = r#"{"vulnerabilities":{"axios":{"severity":"high","fixAvailable":false}}}"#;
        let r = NpmAuditRunner { proc: Arc::new(MockProcessRunner::failing(json)) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
    }

    #[test]
    fn parse_extracts_vulnerability_severity() {
        let json = r#"{"vulnerabilities":{"express":{"severity":"moderate","fixAvailable":true}}}"#;
        let findings = parse_npm_audit_json(json);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Medium));
        assert!(findings[0].suggestion.as_ref().unwrap().contains("audit fix"));
    }
}
