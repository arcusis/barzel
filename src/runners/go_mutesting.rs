use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub struct GoMutestingRunner {
    pub mutation_threshold: f64,
}

impl Default for GoMutestingRunner {
    fn default() -> Self {
        Self { mutation_threshold: 95.0 }
    }
}

impl TestRunner for GoMutestingRunner {
    fn name(&self) -> &'static str {
        "go-mutesting"
    }

    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn skip_message(&self) -> &'static str {
        "go-mutesting not installed — run `go install github.com/zimmski/go-mutesting/cmd/go-mutesting@latest` \
         to enable mutation testing (target: ≥95% mutation score)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Go {
            return false;
        }
        // go-mutesting prints usage and exits non-zero with no args — that's fine, just check it's on PATH
        Command::new("go-mutesting")
            .arg("--help")
            .output()
            .is_ok()
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let output = Command::new("go-mutesting")
            .args(["./..."])
            .current_dir(root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                let combined = format!("{}\n{}", stdout, stderr);

                let mutation_score = parse_go_mutesting_score(&combined);
                let threshold = self.mutation_threshold;

                let status = match mutation_score {
                    Some(score) if score >= threshold => LayerStatus::Pass,
                    Some(_) => LayerStatus::Partial,
                    None if result.status.success() => LayerStatus::Partial,
                    None => LayerStatus::Fail,
                };

                let findings = build_findings(mutation_score, threshold);

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "go-mutesting".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics {
                        mutation_score,
                        ..Default::default()
                    },
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "structural".to_string(),
                runner: "go-mutesting".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "GO_MUTESTING_FAILED".to_string(),
                    message: format!("Failed to run go-mutesting: {}", e),
                    reproduce_cmd: Some("go-mutesting ./... 2>&1".to_string()),
                    suggestion: Some(
                        "Install: `go install github.com/zimmski/go-mutesting/cmd/go-mutesting@latest`"
                            .to_string(),
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

fn parse_go_mutesting_score(output: &str) -> Option<f64> {
    // "The mutation score is 0.8421 (16 of 19 mutants killed)"
    for line in output.lines() {
        if line.contains("mutation score is") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(idx) = parts.iter().position(|&w| w == "is") {
                if let Some(raw) = parts.get(idx + 1) {
                    // Strip trailing '(' if present
                    let clean = raw.trim_end_matches('(');
                    if let Ok(score) = clean.parse::<f64>() {
                        // go-mutesting reports 0.0–1.0
                        return Some(score * 100.0);
                    }
                }
            }
        }
        // Fallback: "PASS: 16/19 (84.21%)"
        if line.contains('%') && (line.contains("PASS") || line.contains("score")) {
            if let Some(pct_str) = line.split('%').next().and_then(|s| s.split_whitespace().last()) {
                if let Ok(score) = pct_str.parse::<f64>() {
                    if score <= 100.0 {
                        return Some(score);
                    }
                }
            }
        }
    }
    None
}

fn build_findings(mutation_score: Option<f64>, threshold: f64) -> Vec<Finding> {
    match mutation_score {
        Some(score) if score >= threshold => vec![],
        Some(score) => vec![Finding {
            severity: Severity::High,
            code: "LOW_MUTATION_SCORE".to_string(),
            message: format!(
                "Mutation score is {:.1}% (target ≥{:.0}%) — {:.1}% of mutants survived",
                score, threshold, 100.0 - score
            ),
            reproduce_cmd: Some("go-mutesting ./... 2>&1 | grep FAIL".to_string()),
            suggestion: Some(
                "Add test cases that exercise boundary conditions. \
                 Look for survived mutants (operators changed without test failures)."
                    .to_string(),
            ),
            ..Default::default()
        }],
        None => vec![Finding {
            severity: Severity::Info,
            code: "MUTATION_NO_SCORE".to_string(),
            message: "go-mutesting ran but could not determine mutation score".to_string(),
            reproduce_cmd: Some("go-mutesting ./... 2>&1".to_string()),
            ..Default::default()
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_standard_score_line() {
        // go-mutesting reports 0.0–1.0
        let output = "The mutation score is 0.8421 (16 of 19 mutants killed)";
        let score = parse_go_mutesting_score(output).unwrap();
        assert!((score - 84.21).abs() < 0.1);
    }

    #[test]
    fn parses_perfect_score() {
        let output = "The mutation score is 1.0000 (19 of 19 mutants killed)";
        let score = parse_go_mutesting_score(output).unwrap();
        assert!((score - 100.0).abs() < 0.01);
    }

    #[test]
    fn returns_none_for_no_match() {
        assert!(parse_go_mutesting_score("").is_none());
        assert!(parse_go_mutesting_score("no relevant output").is_none());
    }

    #[test]
    fn build_findings_empty_when_above_threshold() {
        let findings = build_findings(Some(97.0), 95.0);
        assert!(findings.is_empty());
    }

    #[test]
    fn build_findings_high_severity_when_below_threshold() {
        let findings = build_findings(Some(60.0), 95.0);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::High));
        assert!(findings[0].message.contains("60.0%"));
    }

    #[test]
    fn build_findings_info_when_no_score() {
        let findings = build_findings(None, 95.0);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Info));
    }

    proptest! {
        #[test]
        fn parse_go_mutesting_score_never_panics(s in ".*") {
            let _ = parse_go_mutesting_score(&s);
        }

        #[test]
        fn score_in_range_when_found(
            killed in 0u64..100u64,
            total in 1u64..100u64,
        ) {
            let ratio = killed.min(total) as f64 / total as f64;
            let output = format!("The mutation score is {:.4} ({} of {} mutants killed)", ratio, killed.min(total), total);
            if let Some(score) = parse_go_mutesting_score(&output) {
                prop_assert!(score >= 0.0 && score <= 100.0);
            }
        }
    }
}
