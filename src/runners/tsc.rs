/// TypeScript type checker runner.
/// Runs `tsc --noEmit` to catch type errors without emitting files.
/// Type errors in AI-generated code are a leading source of runtime failures.
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct TscRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for TscRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

impl TestRunner for TscRunner {
    fn name(&self) -> &'static str { "tsc" }
    fn layer(&self) -> Layer { Layer::Logic }

    fn skip_message(&self) -> &'static str {
        "tsc not found — install TypeScript: `npm install -D typescript` and add a tsconfig.json"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript { return false; }
        let root = Path::new(&project.root);
        // tsc must be in node_modules and tsconfig must exist
        root.join("node_modules").join(".bin").join("tsc").exists()
            && (root.join("tsconfig.json").exists() || root.join("tsconfig.app.json").exists())
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let tsc = root.join("node_modules").join(".bin").join("tsc");
        let tsc_str = tsc.to_string_lossy().to_string();

        match self.proc.run(&tsc_str, &["--noEmit", "--pretty", "false"], root) {
            Ok(out) => {
                let combined = out.combined();
                let errors = parse_tsc_errors(&combined);
                let error_count = errors.len() as u64;

                let status = if out.success { LayerStatus::Pass } else { LayerStatus::Fail };

                let findings = if out.success {
                    vec![]
                } else {
                    errors.into_iter().take(10).map(|(loc, msg)| Finding {
                        severity: Severity::High,
                        code: "TYPE_ERROR".to_string(),
                        message: msg,
                        location: Some(loc),
                        reproduce_cmd: Some(format!("{tsc_str} --noEmit 2>&1 | head -40")),
                        suggestion: Some(
                            "Fix TypeScript type errors. AI-generated code frequently introduces \
                             type mismatches — run tsc to verify before deployment."
                                .to_string(),
                        ),
                    }).chain(if error_count > 10 {
                        vec![Finding {
                            severity: Severity::Info,
                            code: "TYPE_ERRORS_TRUNCATED".to_string(),
                            message: format!("{} type error(s) total — showing first 10", error_count),
                            reproduce_cmd: Some(format!("{tsc_str} --noEmit 2>&1")),
                            ..Default::default()
                        }]
                    } else {
                        vec![]
                    }).collect()
                };

                Ok(LayerResult {
                    name: "logic".to_string(),
                    runner: "tsc".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        failed: error_count,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "logic".to_string(),
                runner: "tsc".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "TSC_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run tsc: {}", e),
                    reproduce_cmd: Some("npx tsc --noEmit 2>&1".to_string()),
                    suggestion: Some(
                        "Install TypeScript: `npm install -D typescript` and ensure tsconfig.json exists."
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

/// Parse tsc output into (location, message) pairs.
/// tsc format: "src/foo.ts(12,5): error TS2345: Argument of type..."
pub fn parse_tsc_errors(output: &str) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        // tsc error line: "path/file.ts(line,col): error TSxxxx: message"
        if t.contains("): error TS") {
            if let Some(paren) = t.find('(') {
                if let Some(close) = t.find("):") {
                    let location = t[..close + 1].to_string();
                    let rest = &t[close + 2..].trim();
                    let message = rest.trim_start_matches("error ").to_string();
                    if !message.is_empty() {
                        errors.push((location, message));
                    }
                    let _ = paren; // used indirectly via close
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn ts_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: ProjectFrameworks { is_nextjs: true, ..Default::default() },
            workspace_root: None,
        }
    }

    fn setup_tsc_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/tsc"), b"#!/bin/sh\necho hi").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), b"{}").unwrap();
        dir
    }

    #[test]
    fn name_is_tsc() { assert_eq!(TscRunner::default().name(), "tsc"); }

    #[test]
    fn layer_is_logic() { assert!(matches!(TscRunner::default().layer(), Layer::Logic)); }

    #[test]
    fn not_available_for_rust() {
        let i = ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None };
        assert!(!TscRunner::default().is_available(&i));
    }

    #[test]
    fn not_available_without_tsconfig() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        std::fs::write(dir.path().join("node_modules/.bin/tsc"), b"").unwrap();
        // no tsconfig.json
        assert!(!TscRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn not_available_without_tsc_binary() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), b"{}").unwrap();
        // no node_modules/.bin/tsc
        assert!(!TscRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn available_with_both() {
        let dir = setup_tsc_dir();
        assert!(TscRunner::default().is_available(&ts_info(&dir.path().to_string_lossy())));
    }

    #[test]
    fn run_pass_returns_pass() {
        let dir = setup_tsc_dir();
        let r = TscRunner { proc: Arc::new(MockProcessRunner::passing("")) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_fail_parses_type_errors() {
        let dir = setup_tsc_dir();
        let output = "src/app.ts(12,5): error TS2345: Argument of type 'string' is not assignable to parameter of type 'number'.";
        let r = TscRunner { proc: Arc::new(MockProcessRunner::failing(output)) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert!(!result.findings.is_empty());
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].location.as_ref().unwrap().contains("src/app.ts(12,5)"));
        assert!(result.findings[0].message.contains("TS2345"));
    }

    #[test]
    fn run_truncates_to_10_errors() {
        let dir = setup_tsc_dir();
        let lines: String = (1..=15).map(|i| {
            format!("src/f.ts({i},1): error TS2322: Type error {i}.\n")
        }).collect();
        let r = TscRunner { proc: Arc::new(MockProcessRunner::failing(&lines)) };
        let result = r.run(&ts_info(&dir.path().to_string_lossy())).unwrap();
        let type_errors: Vec<_> = result.findings.iter().filter(|f| f.code == "TYPE_ERROR").collect();
        assert_eq!(type_errors.len(), 10);
        let truncated = result.findings.iter().any(|f| f.code == "TYPE_ERRORS_TRUNCATED");
        assert!(truncated);
    }

    #[test]
    fn parse_tsc_error_line() {
        let errors = parse_tsc_errors("src/app.ts(12,5): error TS2345: Argument of type 'string' is not assignable.");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "src/app.ts(12,5)");
        assert!(errors[0].1.contains("TS2345"));
    }

    #[test]
    fn parse_ignores_non_error_lines() {
        let errors = parse_tsc_errors("Found 2 errors in 1 file.\nDone in 1.5s");
        assert!(errors.is_empty());
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".*") { let _ = parse_tsc_errors(&s); }
    }
}
