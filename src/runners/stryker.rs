use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use super::fastcheck::detect_package_manager;

pub struct StrykerRunner {
    pub mutation_threshold: f64,
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for StrykerRunner {
    fn default() -> Self {
        Self { mutation_threshold: 95.0, proc: Arc::new(OsProcessRunner) }
    }
}

impl StrykerRunner {
    pub fn with_threshold(mutation_threshold: f64) -> Self {
        Self { mutation_threshold, ..Default::default() }
    }
}

impl TestRunner for StrykerRunner {
    fn name(&self) -> &'static str {
        "stryker"
    }

    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn skip_message(&self) -> &'static str {
        "Stryker not configured — add `@stryker-mutator/core` and a stryker.config.mjs to enable mutation testing (target: ≥95% mutation score)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::TypeScript {
            return false;
        }
        let root = Path::new(&project.root);
        let has_config = root.join("stryker.conf.json").exists()
            || root.join("stryker.config.js").exists()
            || root.join("stryker.config.mjs").exists()
            || root.join("stryker.config.cjs").exists();

        let has_dep = {
            let pkg = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
            pkg.contains("@stryker-mutator")
        };

        has_config || has_dep
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);
        let pm = detect_package_manager(root);

        match self.proc.run("npx", &["stryker", "run", "--reporters", "json,clear-text"], root) {
            Ok(out) => {
                // Try to parse the JSON mutation report
                let report_path = root.join("reports").join("mutation").join("mutation.json");
                let mutation_score = if report_path.exists() {
                    parse_stryker_report(&report_path)
                } else {
                    parse_stryker_text_output(&out.combined())
                };

                let threshold = self.mutation_threshold;
                let status = match mutation_score {
                    Some(score) if score >= threshold => LayerStatus::Pass,
                    Some(_) => LayerStatus::Partial,
                    None if out.success => LayerStatus::Partial,
                    None => LayerStatus::Fail,
                };

                let findings = build_stryker_findings(mutation_score, self.mutation_threshold, pm);

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "stryker".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        tests_run: 0,
                        passed: 0,
                        failed: 0,
                        coverage: None,
                        mutation_score,
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "structural".to_string(),
                runner: "stryker".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "STRYKER_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run stryker: {}", e),
                    reproduce_cmd: Some("npx stryker run".to_string()),
                    suggestion: Some(format!(
                        "Install stryker: `{} add -D @stryker-mutator/core @stryker-mutator/vitest-runner`",
                        pm
                    )),
                    ..Default::default()
                }],
                metrics: LayerMetrics {
                    tests_run: 0,
                    passed: 0,
                    failed: 1,
                    coverage: None,
                    mutation_score: None,
                },
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

fn parse_stryker_report(path: &Path) -> Option<f64> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let files = json.get("files")?.as_object()?;

    let mut killed = 0u64;
    let mut survived = 0u64;
    let mut no_coverage = 0u64;

    for (_file, file_data) in files {
        let mutants = file_data.get("mutants")?.as_array()?;
        for mutant in mutants {
            match mutant.get("status").and_then(|s| s.as_str()) {
                Some("Killed") | Some("Timeout") => killed += 1,
                Some("Survived") => survived += 1,
                Some("NoCoverage") => no_coverage += 1,
                _ => {}
            }
        }
    }

    let total = killed + survived + no_coverage;
    if total == 0 {
        return None;
    }
    Some(killed as f64 / total as f64 * 100.0)
}

fn parse_stryker_text_output(output: &str) -> Option<f64> {
    // Look for "Mutation score: 87.50%"
    for line in output.lines() {
        if line.contains("Mutation score") && line.contains('%') {
            if let Some(pct) = line.split('%').next() {
                if let Some(num) = pct.split_whitespace().last() {
                    return num.parse().ok();
                }
            }
        }
        // Alternative: "87.50 (34/39)"
        if line.contains("(") && line.contains("/") && line.contains(")") {
            if let Some(score_str) = line.split_whitespace().next() {
                if let Ok(score) = score_str.parse::<f64>() {
                    if score <= 100.0 {
                        return Some(score);
                    }
                }
            }
        }
    }
    None
}

fn build_stryker_findings(mutation_score: Option<f64>, threshold: f64, _pm: &str) -> Vec<Finding> {
    match mutation_score {
        Some(score) if score >= threshold => vec![],

        Some(score) => vec![Finding {
            severity: Severity::High,
            code: "LOW_MUTATION_SCORE".to_string(),
            message: format!(
                "Mutation score is {:.1}% (target ≥{:.0}%) — {:.1}% of mutants survived",
                score, threshold, 100.0 - score
            ),
            reproduce_cmd: Some("npx stryker run".to_string()),
            suggestion: Some(
                "Open reports/mutation/mutation.html to see surviving mutants. \
                 Add test cases that specifically target the uncovered conditions."
                    .to_string(),
            ),
            ..Default::default()
        }],
        None => vec![Finding {
            severity: Severity::Info,
            code: "MUTATION_NO_SCORE".to_string(),
            message: "Stryker ran but could not determine mutation score. Check reports/mutation/."
                .to_string(),
            reproduce_cmd: Some("npx stryker run && open reports/mutation/mutation.html".to_string()),
            suggestion: None,
            ..Default::default()
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn ts_project(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::TypeScript,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("my-app".to_string()),
            frameworks: Default::default(),
        }
    }

    fn runner_with(mock: MockProcessRunner) -> StrykerRunner {
        StrykerRunner { proc: Arc::new(mock), ..Default::default() }
    }

    // ── runner metadata ───────────────────────────────────────────────────────

    #[test]
    fn name_is_stryker() {
        assert_eq!(StrykerRunner::default().name(), "stryker");
    }

    #[test]
    fn layer_is_structural() {
        assert!(matches!(StrykerRunner::default().layer(), crate::plugin::Layer::Structural));
    }

    #[test]
    fn skip_message_is_nonempty() {
        assert!(!StrykerRunner::default().skip_message().is_empty());
    }

    #[test]
    fn not_available_for_non_typescript() {
        let dir = tempdir().unwrap();
        let info = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
        };
        assert!(!StrykerRunner::default().is_available(&info));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn available_when_stryker_dep_present() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"devDependencies":{"@stryker-mutator/core":"7.0"}}"#,
        ).unwrap();
        let info = ProjectInfo { language: Language::TypeScript, root: dir.path().to_string_lossy().to_string(), has_tests: true, package_name: None, frameworks: Default::default() };
        assert!(StrykerRunner::default().is_available(&info));
    }

    #[test]
    fn available_when_stryker_config_present() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("stryker.config.mjs"), b"export default {}").unwrap();
        let info = ProjectInfo { language: Language::TypeScript, root: dir.path().to_string_lossy().to_string(), has_tests: true, package_name: None, frameworks: Default::default() };
        assert!(StrykerRunner::default().is_available(&info));
    }

    // ── run() with mock ───────────────────────────────────────────────────────

    #[test]
    fn run_returns_pass_when_score_meets_threshold() {
        let stdout = "Mutation score: 97.00%";
        let result = runner_with(MockProcessRunner::passing(stdout)).run(&ts_project("/tmp")).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_returns_partial_when_score_below_threshold() {
        let stdout = "Mutation score: 60.00%";
        let result = runner_with(MockProcessRunner::passing(stdout)).run(&ts_project("/tmp")).unwrap();
        assert!(matches!(result.status, LayerStatus::Partial));
        assert_eq!(result.findings[0].severity, Severity::High);
    }

    #[test]
    fn run_returns_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "npx not found"))
            }
        }
        let runner = StrykerRunner { proc: Arc::new(BrokenProc), ..Default::default() };
        let result = runner.run(&ts_project("/tmp")).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    // ── build_stryker_findings ────────────────────────────────────────────────

    #[test]
    fn empty_findings_when_score_meets_threshold() {
        assert!(build_stryker_findings(Some(95.0), 95.0, "pnpm").is_empty());
        assert!(build_stryker_findings(Some(100.0), 95.0, "pnpm").is_empty());
    }

    #[test]
    fn high_severity_when_below_threshold() {
        let findings = build_stryker_findings(Some(60.0), 95.0, "pnpm");
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::High));
        assert!(findings[0].message.contains("60.0%"));
        assert!(findings[0].message.contains("95"));
    }

    #[test]
    fn info_finding_when_no_score() {
        let findings = build_stryker_findings(None, 95.0, "pnpm");
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Info));
    }

    #[test]
    fn boundary_exactly_at_threshold_passes() {
        assert!(build_stryker_findings(Some(80.0), 80.0, "npm").is_empty());
    }

    // ── parse_stryker_text_output ─────────────────────────────────────────────

    #[test]
    fn parses_stryker_mutation_score_line() {
        let output = "Mutation score: 87.50%";
        let score = parse_stryker_text_output(output).unwrap();
        assert!((score - 87.5).abs() < 0.01);
    }

    #[test]
    fn returns_none_for_empty_output() {
        assert!(parse_stryker_text_output("").is_none());
    }

    // ── parse_stryker_report (JSON) ───────────────────────────────────────────

    #[test]
    fn parses_json_mutation_report() {
        let dir = tempdir().unwrap();
        let report_dir = dir.path().join("reports").join("mutation");
        std::fs::create_dir_all(&report_dir).unwrap();

        let json = r#"{
            "schemaVersion": "1",
            "files": {
                "src/foo.ts": {
                    "mutants": [
                        {"status": "Killed"},
                        {"status": "Killed"},
                        {"status": "Survived"},
                        {"status": "NoCoverage"}
                    ]
                }
            }
        }"#;
        let report_path = report_dir.join("mutation.json");
        std::fs::write(&report_path, json).unwrap();

        let score = parse_stryker_report(&report_path).unwrap();
        // killed=2, survived=1, no_coverage=1, total=4 → 2/4 = 50%
        assert!((score - 50.0).abs() < 0.01);
    }

    #[test]
    fn perfect_score_when_all_killed() {
        let dir = tempdir().unwrap();
        let json = r#"{"files":{"f.ts":{"mutants":[{"status":"Killed"},{"status":"Killed"}]}}}"#;
        let path = dir.path().join("r.json");
        std::fs::write(&path, json).unwrap();
        let score = parse_stryker_report(&path).unwrap();
        assert!((score - 100.0).abs() < 0.01);
    }

    proptest! {
        #[test]
        fn build_stryker_findings_never_panics(
            score in proptest::option::of(0.0f64..100.0f64),
            threshold in 0.0f64..100.0f64,
        ) {
            let _ = build_stryker_findings(score, threshold, "npm");
        }

        #[test]
        fn parse_stryker_text_never_panics(s in ".*") {
            let _ = parse_stryker_text_output(&s);
        }
    }
}
