use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct PlaywrightRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for PlaywrightRunner {
    fn default() -> Self {
        Self {
            proc: Arc::new(OsProcessRunner),
        }
    }
}

impl TestRunner for PlaywrightRunner {
    fn name(&self) -> &'static str {
        "playwright"
    }
    fn layer(&self) -> Layer {
        Layer::Operational
    }

    fn skip_message(&self) -> &'static str {
        "playwright not found — run `npx playwright install` and add `@playwright/test` to dev-dependencies"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if !project.frameworks.is_nextjs && project.language != crate::detect::Language::TypeScript
        {
            return false;
        }
        let root = Path::new(&project.root);
        // playwright.config.ts or playwright.config.js must exist
        root.join("playwright.config.ts").exists() || root.join("playwright.config.js").exists()
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        // Prefer local npx playwright
        match self.proc.run("npx", &["playwright", "test", "--reporter=json"], root) {
            Ok(out) => {
                let combined = out.combined();
                let (passed, failed) = parse_playwright_output(&combined);
                let total = passed + failed;

                let status = if out.success { LayerStatus::Pass } else { LayerStatus::Fail };

                let findings = if out.success {
                    vec![]
                } else {
                    vec![Finding {
                        severity: Severity::High,
                        code: "PLAYWRIGHT_FAILURE".to_string(),
                        message: format!(
                            "{} E2E test(s) failed out of {}",
                            failed, total
                        ),
                        reproduce_cmd: Some("npx playwright test --reporter=list 2>&1 | head -80".to_string()),
                        suggestion: Some(
                            "Run `npx playwright test --headed` to see failures interactively. \
                             Check for broken routes, auth flows, or UI regressions."
                                .to_string(),
                        ),
                        ..Default::default()
                    }]
                };

                Ok(LayerResult {
                    name: "operational".to_string(),
                    runner: "playwright".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: total,
                        passed,
                        failed,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "operational".to_string(),
                runner: "playwright".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "PLAYWRIGHT_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run playwright: {}", e),
                    reproduce_cmd: Some("npx playwright test 2>&1".to_string()),
                    suggestion: Some(
                        "Install: `npm install -D @playwright/test && npx playwright install`. \
                         Ensure the dev server is running or configure `webServer` in playwright.config.ts."
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

pub fn parse_playwright_output(output: &str) -> (u64, u64) {
    // JSON reporter: {"suites":[...],"stats":{"expected":5,"unexpected":2,...}}
    // Try each line since stderr may be mixed in
    for line in output.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(stats) = json.get("stats") {
                let passed = stats.get("expected").and_then(|v| v.as_u64()).unwrap_or(0);
                let failed = stats
                    .get("unexpected")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                return (passed, failed);
            }
        }
    }
    // Fallback: text output "X passed (Xs)"
    for line in output.lines().rev() {
        let l = line.trim();
        if l.contains(" passed") {
            let passed = l
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let failed = extract_playwright_failed(l);
            return (passed, failed);
        }
    }
    (0, 0)
}

fn extract_playwright_failed(line: &str) -> u64 {
    if let Some(idx) = line.find(" failed") {
        let before = line[..idx].trim_end();
        if let Some(tok) = before.split_whitespace().last() {
            return tok.parse().unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn nextjs_info(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: ProjectFrameworks {
                is_nextjs: true,
                has_ai_deps: false,
                ai_frameworks: vec![],
            },
            workspace_root: None,
        }
    }

    #[test]
    fn name_is_playwright() {
        assert_eq!(PlaywrightRunner::default().name(), "playwright");
    }

    #[test]
    fn layer_is_operational() {
        assert!(matches!(
            PlaywrightRunner::default().layer(),
            Layer::Operational
        ));
    }

    #[test]
    fn not_available_without_config_file() {
        let dir = tempdir().unwrap();
        let info = nextjs_info(&dir.path().to_string_lossy());
        assert!(!PlaywrightRunner::default().is_available(&info));
    }

    #[test]
    fn available_with_ts_config() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("playwright.config.ts"),
            b"export default {};",
        )
        .unwrap();
        let info = nextjs_info(&dir.path().to_string_lossy());
        assert!(PlaywrightRunner::default().is_available(&info));
    }

    #[test]
    fn available_with_js_config() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("playwright.config.js"),
            b"module.exports = {};",
        )
        .unwrap();
        let info = nextjs_info(&dir.path().to_string_lossy());
        assert!(PlaywrightRunner::default().is_available(&info));
    }

    #[test]
    fn not_available_for_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("playwright.config.ts"), b"").unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        assert!(!PlaywrightRunner::default().is_available(&info));
    }

    #[test]
    fn run_pass_returns_pass() {
        let json = r#"{"stats":{"expected":10,"unexpected":0}}"#;
        let r = PlaywrightRunner {
            proc: Arc::new(MockProcessRunner::passing(json)),
        };
        let result = r.run(&nextjs_info("/tmp")).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert_eq!(result.metrics.passed, 10);
        assert_eq!(result.metrics.failed, 0);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_fail_returns_fail_with_finding() {
        let json = r#"{"stats":{"expected":8,"unexpected":2}}"#;
        let r = PlaywrightRunner {
            proc: Arc::new(MockProcessRunner::failing(json)),
        };
        let result = r.run(&nextjs_info("/tmp")).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.failed, 2);
        assert!(result.findings[0].message.contains("2 E2E"));
    }

    #[test]
    fn parse_json_stats() {
        let json = r#"{"stats":{"expected":5,"unexpected":3}}"#;
        let (p, f) = parse_playwright_output(json);
        assert_eq!(p, 5);
        assert_eq!(f, 3);
    }

    #[test]
    fn parse_text_fallback() {
        let (p, _f) = parse_playwright_output("10 passed (4s)");
        assert_eq!(p, 10);
    }
}
