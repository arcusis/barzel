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
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for NpmAuditRunner {
    fn name(&self) -> &'static str {
        "npm-audit"
    }
    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn skip_message(&self) -> &'static str {
        "npm-audit requires a lockfile — run `npm install` or `pnpm install` to generate one"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        // Check package root first; fall back to workspace root so members without
        // their own lockfile (common in pnpm/npm workspaces) are still detected.
        let has_lockfile = |dir: &Path| {
            dir.join("package-lock.json").exists()
                || dir.join("pnpm-lock.yaml").exists()
                || dir.join("yarn.lock").exists()
        };
        has_lockfile(Path::new(&project.root))
            || project
                .workspace_root
                .as_deref()
                .is_some_and(|ws| has_lockfile(Path::new(ws)))
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        // Use the directory that contains the lockfile as the working dir.
        // For workspace members this is typically the workspace root.
        let pkg_root = Path::new(&project.root);
        let root = if pkg_root.join("pnpm-lock.yaml").exists()
            || pkg_root.join("package-lock.json").exists()
            || pkg_root.join("yarn.lock").exists()
        {
            pkg_root
        } else if let Some(ws) = project.workspace_root.as_deref() {
            Path::new(ws)
        } else {
            pkg_root
        };

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
                let mut findings = parse_npm_audit_json(&combined);

                // Unconditionally overwrite reproduce_cmd with the actual package
                // manager and cwd. Parsed findings carry a default npm-based command;
                // this corrects it for pnpm/yarn and for workspace-root cwd.
                for f in &mut findings {
                    f.reproduce_cmd = Some(audit_reproduce_cmd(cmd, root, &f.code));
                }

                let status = if findings
                    .iter()
                    .any(|f| matches!(f.severity, Severity::Critical))
                {
                    LayerStatus::Fail
                } else if findings
                    .iter()
                    .any(|f| matches!(f.severity, Severity::High | Severity::Medium))
                {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                let findings = if findings.is_empty() {
                    vec![Finding {
                        severity: Severity::Info,
                        code: "NPM_AUDIT_PASSED".to_string(),
                        message: "No known vulnerabilities in npm dependencies".to_string(),
                        suggestion: Some(
                            "Keep dependencies updated: `npx npm-check-updates -u`".to_string(),
                        ),
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
                    message: format!("Failed to run {} audit: {}", cmd, e),
                    reproduce_cmd: Some(format!(
                        "cd {} && {} audit --json 2>&1",
                        shell_quote(root),
                        cmd
                    )),
                    suggestion: Some(format!(
                        "Ensure {} is installed and `{} install` has been run.",
                        cmd, cmd
                    )),
                    ..Default::default()
                }],
                metrics: LayerMetrics {
                    failed: 1,
                    ..Default::default()
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

/// Single-quote a path for POSIX shell. Wraps in single quotes and escapes
/// any embedded single quotes so paths with spaces are runnable verbatim.
fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build a reproduce command that names the actual package manager and working directory.
fn audit_reproduce_cmd(cmd: &str, cwd: &Path, finding_code: &str) -> String {
    let quoted = shell_quote(cwd);
    // Extract the package name from codes like NPM_VULN_LODASH → lodash
    let pkg = finding_code
        .strip_prefix("NPM_VULN_")
        .map(|s| s.to_lowercase().replace('_', "-"))
        .unwrap_or_default();
    if pkg.is_empty() {
        format!("cd {quoted} && {cmd} audit --json 2>&1")
    } else {
        format!("cd {quoted} && {cmd} audit --json 2>&1 | jq '.vulnerabilities.\"{pkg}\"'")
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
    let findings: Vec<Finding> = vulns
        .iter()
        .map(|(pkg, vuln)| {
            let severity_str = vuln
                .get("severity")
                .and_then(|s| s.as_str())
                .unwrap_or("low");
            let severity = match severity_str {
                "critical" => Severity::Critical,
                "high" => Severity::High,
                "moderate" | "medium" => Severity::Medium,
                _ => Severity::Low,
            };

            let fix_available = vuln
                .get("fixAvailable")
                .and_then(|f| f.as_bool())
                .unwrap_or(false);

            Finding {
                severity,
                code: format!("NPM_VULN_{}", pkg.to_uppercase().replace('-', "_")),
                message: format!("Vulnerability in `{}` ({})", pkg, severity_str),
                // Default uses npm; run() overwrites with the selected package manager and cwd.
                reproduce_cmd: Some(format!(
                    "npm audit --json 2>&1 | jq '.vulnerabilities.\"{pkg}\"'"
                )),
                suggestion: Some(if fix_available {
                    "Run `npm audit fix` to auto-fix. Review breaking changes first.".to_string()
                } else {
                    "No automatic fix available. Review and replace the dependency.".to_string()
                }),
                ..Default::default()
            }
        })
        .collect();
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
    fn name_is_npm_audit() {
        assert_eq!(NpmAuditRunner::default().name(), "npm-audit");
    }

    #[test]
    fn layer_is_hostile() {
        assert!(matches!(NpmAuditRunner::default().layer(), Layer::Hostile));
    }

    #[test]
    fn not_available_for_rust() {
        let i = ProjectInfo {
            language: Language::Rust,
            root: "/tmp".to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
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
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::passing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "NPM_AUDIT_PASSED"));
    }

    #[test]
    fn critical_vulnerability_returns_fail() {
        let dir = setup_npm_dir();
        let json = r#"{"vulnerabilities":{"lodash":{"severity":"critical","fixAvailable":true}}}"#;
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::failing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert!(result.findings[0].reproduce_cmd.is_some());
    }

    #[test]
    fn high_severity_returns_partial() {
        let dir = setup_npm_dir();
        let json = r#"{"vulnerabilities":{"axios":{"severity":"high","fixAvailable":false}}}"#;
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::failing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
    }

    #[test]
    fn available_with_workspace_root_pnpm_lock() {
        let ws_dir = tempdir().unwrap();
        std::fs::write(
            ws_dir.path().join("pnpm-lock.yaml"),
            b"lockfileVersion: '6.0'",
        )
        .unwrap();
        let pkg_dir = tempdir().unwrap();
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: pkg_dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: Default::default(),
            workspace_root: Some(ws_dir.path().to_string_lossy().to_string()),
        };
        assert!(
            NpmAuditRunner::default().is_available(&info),
            "should be available when lockfile is at workspace root"
        );
    }

    #[test]
    fn not_available_without_lockfile_even_with_workspace_root() {
        let ws_dir = tempdir().unwrap();
        let pkg_dir = tempdir().unwrap();
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: pkg_dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: Default::default(),
            workspace_root: Some(ws_dir.path().to_string_lossy().to_string()),
        };
        assert!(
            !NpmAuditRunner::default().is_available(&info),
            "should not be available when neither package root nor workspace root has a lockfile"
        );
    }

    /// Recording subprocess runner — captures the last call for assertion.
    struct RecordingRunner {
        response: crate::process::ProcessOutput,
        last_call: std::sync::Mutex<Option<(String, Vec<String>, std::path::PathBuf)>>,
    }

    impl RecordingRunner {
        fn passing(stdout: &str) -> Self {
            Self {
                response: crate::process::ProcessOutput {
                    success: true,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
                last_call: std::sync::Mutex::new(None),
            }
        }
        fn last(&self) -> (String, Vec<String>, std::path::PathBuf) {
            self.last_call
                .lock()
                .unwrap()
                .clone()
                .expect("no call recorded")
        }
    }

    impl crate::process::SubprocessRunner for RecordingRunner {
        fn run(
            &self,
            cmd: &str,
            args: &[&str],
            cwd: &std::path::Path,
        ) -> std::io::Result<crate::process::ProcessOutput> {
            *self.last_call.lock().unwrap() = Some((
                cmd.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(self.response.clone())
        }
    }

    #[test]
    fn run_uses_workspace_root_as_cwd_and_selects_pnpm() {
        let ws_dir = tempdir().unwrap();
        std::fs::write(
            ws_dir.path().join("pnpm-lock.yaml"),
            b"lockfileVersion: '6.0'",
        )
        .unwrap();
        let pkg_dir = tempdir().unwrap();
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: pkg_dir.path().to_string_lossy().to_string(),
            has_tests: false,
            package_name: Some("sub".to_string()),
            frameworks: Default::default(),
            workspace_root: Some(ws_dir.path().to_string_lossy().to_string()),
        };
        let recorder = Arc::new(RecordingRunner::passing(r#"{"vulnerabilities":{}}"#));
        let r = NpmAuditRunner {
            proc: Arc::clone(&recorder) as Arc<dyn crate::process::SubprocessRunner>,
        };
        let result = r.run(&info).unwrap();

        assert!(matches!(result.status, LayerStatus::Pass));
        let (cmd, args, cwd) = recorder.last();
        assert_eq!(
            cmd, "pnpm",
            "must select pnpm when pnpm-lock.yaml is at workspace root"
        );
        assert_eq!(args, &["audit", "--json"]);
        assert_eq!(
            cwd,
            ws_dir.path(),
            "cwd must be workspace root where pnpm-lock.yaml lives"
        );
    }

    #[test]
    fn reproduce_cmd_uses_selected_package_manager_and_cwd() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"lockfileVersion: '6.0'").unwrap();
        let json = r#"{"vulnerabilities":{"lodash":{"severity":"high","fixAvailable":false}}}"#;
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::passing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        let rc = result.findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert!(
            rc.contains("pnpm"),
            "reproduce_cmd must name pnpm, got: {rc}"
        );
        assert!(
            rc.contains(&dir.path().to_string_lossy().as_ref()),
            "reproduce_cmd must include cwd"
        );
    }

    #[test]
    fn spawn_error_finding_names_selected_cmd() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::spawn_error("not found")),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        let finding = &result.findings[0];
        assert!(
            finding.message.contains("yarn"),
            "error message must name yarn"
        );
        let rc = finding.reproduce_cmd.as_deref().unwrap_or("");
        assert!(
            rc.contains("yarn"),
            "reproduce_cmd must name yarn on spawn error"
        );
    }

    #[test]
    fn parse_extracts_vulnerability_severity() {
        let json = r#"{"vulnerabilities":{"express":{"severity":"moderate","fixAvailable":true}}}"#;
        let findings = parse_npm_audit_json(json);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Medium));
        assert!(findings[0]
            .suggestion
            .as_ref()
            .unwrap()
            .contains("audit fix"));
    }

    #[test]
    fn parser_findings_always_have_reproduce_cmd() {
        let json = r#"{"vulnerabilities":{"lodash":{"severity":"high","fixAvailable":false},"axios":{"severity":"critical","fixAvailable":true}}}"#;
        let findings = parse_npm_audit_json(json);
        assert!(!findings.is_empty());
        for f in &findings {
            assert!(
                f.reproduce_cmd.is_some(),
                "parser finding '{}' must have reproduce_cmd",
                f.code
            );
        }
    }

    #[test]
    fn reproduce_cmd_shell_quotes_path_with_spaces() {
        let base = tempdir().unwrap();
        let spaced = base.path().join("my project");
        std::fs::create_dir_all(&spaced).unwrap();
        std::fs::write(spaced.join("package-lock.json"), b"{}").unwrap();
        let json = r#"{"vulnerabilities":{"lodash":{"severity":"high","fixAvailable":false}}}"#;
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::passing(json)),
        };
        let result = r.run(&ts_info(&spaced.to_string_lossy())).unwrap();
        let rc = result.findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert!(
            rc.contains("'"),
            "path with spaces must be single-quoted in reproduce_cmd, got: {rc}"
        );
        assert!(
            !rc.contains("my project "),
            "unquoted path must not appear in reproduce_cmd"
        );
    }

    #[test]
    fn spawn_error_reproduce_cmd_quotes_path_with_spaces() {
        let base = tempdir().unwrap();
        let spaced = base.path().join("my workspace");
        std::fs::create_dir_all(&spaced).unwrap();
        std::fs::write(spaced.join("yarn.lock"), b"").unwrap();
        let r = NpmAuditRunner {
            proc: Arc::new(MockProcessRunner::spawn_error("not found")),
        };
        let result = r.run(&ts_info(&spaced.to_string_lossy())).unwrap();
        let rc = result.findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert!(
            rc.contains("'"),
            "spawn-error reproduce_cmd must shell-quote path with spaces, got: {rc}"
        );
    }
}
