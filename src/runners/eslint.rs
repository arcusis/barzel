/// ESLint runner for TypeScript/JavaScript projects.
/// Runs eslint as SAST — catches real code quality issues, security patterns,
/// and AI-generated code antipatterns (console.log, any types, unsafe patterns).
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct EslintRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for EslintRunner {
    fn default() -> Self {
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for EslintRunner {
    fn name(&self) -> &'static str {
        "eslint"
    }
    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn skip_message(&self) -> &'static str {
        "eslint not found — install: `npm install -D eslint` and add an eslint config to catch security and quality issues"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        let root = Path::new(&project.root);
        root.join("node_modules")
            .join(".bin")
            .join("eslint")
            .exists()
            && has_eslint_config(root)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);
        let eslint = root.join("node_modules").join(".bin").join("eslint");
        let eslint_str = eslint.to_string_lossy().to_string();

        match self.proc.run(
            &eslint_str,
            &[
                ".",
                "--format=json",
                "--ext=.ts,.tsx,.js,.jsx",
                "--max-warnings=0",
            ],
            root,
        ) {
            Ok(out) => {
                let combined = out.combined();
                let findings = parse_eslint_json(&combined);

                let status = if out.success {
                    LayerStatus::Pass
                } else if findings
                    .iter()
                    .any(|f| matches!(f.severity, Severity::High | Severity::Critical))
                {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                let findings = if findings.is_empty() {
                    vec![Finding {
                        severity: Severity::Info,
                        code: "ESLINT_PASSED".to_string(),
                        message: "No ESLint issues found".to_string(),
                        ..Default::default()
                    }]
                } else {
                    findings
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "eslint".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "eslint".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "ESLINT_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run eslint: {}", e),
                    reproduce_cmd: Some(format!("{eslint_str} . 2>&1 | head -30")),
                    suggestion: Some(
                        "Install: `npm install -D eslint` and add .eslintrc".to_string(),
                    ),
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

fn has_eslint_config(root: &Path) -> bool {
    for name in &[
        ".eslintrc",
        ".eslintrc.js",
        ".eslintrc.cjs",
        ".eslintrc.json",
        ".eslintrc.yaml",
        ".eslintrc.yml",
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.ts",
    ] {
        if root.join(name).exists() {
            return true;
        }
    }
    // Check package.json for eslintConfig key
    if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
        if pkg.contains("\"eslintConfig\"") {
            return true;
        }
    }
    false
}

pub fn parse_eslint_json(output: &str) -> Vec<Finding> {
    // ESLint JSON format: array of file results, each with messages[]
    // Try line-by-line first (stderr may be mixed in)
    for line in output.lines() {
        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(results) = arr.as_array() {
                return results.iter().flat_map(eslint_file_to_findings).collect();
            }
        }
    }
    if let Ok(arr) = serde_json::from_str::<serde_json::Value>(output) {
        if let Some(results) = arr.as_array() {
            return results.iter().flat_map(eslint_file_to_findings).collect();
        }
    }
    vec![]
}

fn eslint_file_to_findings(file_result: &serde_json::Value) -> Vec<Finding> {
    let filepath = file_result
        .get("filePath")
        .and_then(|f| f.as_str())
        .unwrap_or("");
    let messages = match file_result.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return vec![],
    };

    messages
        .iter()
        .map(|msg| {
            let severity_num = msg.get("severity").and_then(|s| s.as_u64()).unwrap_or(1);
            let severity = if severity_num == 2 {
                Severity::High
            } else {
                Severity::Medium
            };

            let rule = msg
                .get("ruleId")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown");
            let message = msg
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("lint error");
            let line = msg.get("line").and_then(|l| l.as_u64()).unwrap_or(0);

            Finding {
                severity,
                code: format!("ESLINT_{}", rule.replace(['/', '-'], "_").to_uppercase()),
                message: format!("[{}] {}", rule, message),
                location: if filepath.is_empty() {
                    None
                } else {
                    Some(format!("{}:{}", filepath, line))
                },
                reproduce_cmd: Some(format!("./node_modules/.bin/eslint {} 2>&1", filepath)),
                suggestion: Some(format!(
                    "Fix rule `{}`. Run `eslint --fix` for auto-fixable issues.",
                    rule
                )),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn ts_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: ProjectFrameworks {
                is_nextjs: true,
                ..Default::default()
            },
            workspace_root: None,
        }
    }

    fn setup_eslint_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/.bin/eslint"),
            b"#!/bin/sh\necho hi",
        )
        .unwrap();
        std::fs::write(dir.path().join(".eslintrc.json"), b"{}").unwrap();
        dir
    }

    #[test]
    fn name_is_eslint() {
        assert_eq!(EslintRunner::default().name(), "eslint");
    }

    #[test]
    fn layer_is_hostile() {
        assert!(matches!(EslintRunner::default().layer(), Layer::Hostile));
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
        assert!(!EslintRunner::default().is_available(&i));
    }

    #[test]
    fn not_available_without_config() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/eslint"), b"").unwrap();
        assert!(!EslintRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_config_and_binary() {
        let dir = setup_eslint_dir();
        assert!(EslintRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_eslint_config_js() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/eslint"), b"").unwrap();
        std::fs::write(dir.path().join("eslint.config.js"), b"").unwrap();
        assert!(EslintRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn clean_run_returns_pass_with_info_finding() {
        let dir = setup_eslint_dir();
        let json =
            r#"[{"filePath":"/app/src/index.ts","messages":[],"errorCount":0,"warningCount":0}]"#;
        let r = EslintRunner {
            proc: Arc::new(MockProcessRunner::passing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.iter().any(|f| f.code == "ESLINT_PASSED"));
    }

    #[test]
    fn run_with_errors_returns_partial() {
        let dir = setup_eslint_dir();
        let json = r#"[{"filePath":"/app/src/index.ts","messages":[{"ruleId":"no-console","severity":2,"message":"Unexpected console statement.","line":5}],"errorCount":1,"warningCount":0}]"#;
        let r = EslintRunner {
            proc: Arc::new(MockProcessRunner::failing(json)),
        };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert!(!result.findings.is_empty());
        assert!(result.findings[0].message.contains("no-console"));
    }

    #[test]
    fn parse_extracts_rule_and_location() {
        let json = r#"[{"filePath":"/src/app.ts","messages":[{"ruleId":"@typescript-eslint/no-explicit-any","severity":2,"message":"Unexpected any.","line":10}]}]"#;
        let findings = parse_eslint_json(json);
        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .location
            .as_ref()
            .unwrap()
            .contains("/src/app.ts:10"));
        assert!(findings[0].code.contains("NO_EXPLICIT_ANY"));
    }

    #[test]
    fn parse_empty_array_returns_no_findings() {
        let findings = parse_eslint_json(r#"[]"#);
        assert!(findings.is_empty());
    }
}
