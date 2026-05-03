use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

pub struct MutantsRunner {
    pub mutation_threshold: f64,
}

impl Default for MutantsRunner {
    fn default() -> Self {
        Self { mutation_threshold: 95.0 }
    }
}

impl TestRunner for MutantsRunner {
    fn name(&self) -> &'static str {
        "cargo-mutants"
    }

    fn layer(&self) -> Layer {
        Layer::Structural
    }

    fn skip_message(&self) -> &'static str {
        "cargo-mutants not installed — run `cargo install cargo-mutants` to enable mutation testing (target: ≥95% mutation score)"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        if project.language != crate::detect::Language::Rust {
            return false;
        }
        Command::new("cargo")
            .args(["mutants", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        let output = Command::new("cargo")
            .args(["mutants", "--timeout", "30", "--no-shuffle"])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                let mutation_score = parse_mutation_score(&output_str);

                let threshold = self.mutation_threshold;
                let status = match mutation_score {
                    Some(score) if score >= threshold => LayerStatus::Pass,
                    Some(_) => LayerStatus::Partial,
                    None => LayerStatus::Partial,
                };

                let findings = match mutation_score {
                    Some(score) if score < threshold => vec![Finding {
                        severity: Severity::High,
                        code: "LOW_MUTATION_SCORE".to_string(),
                        message: format!(
                            "Mutation score is {:.1}% (target ≥{:.0}%) — {:.1}% of mutants survived",
                            score, threshold,
                            100.0 - score
                        ),
                        reproduce_cmd: Some("cargo mutants 2>&1 | tail -20".to_string()),
                        suggestion: Some(
                            "Check mutants.out/missed.txt for surviving mutants. \
                             Add test cases that specifically exercise boundary conditions."
                                .to_string(),
                        ),
                        ..Default::default()
                    }],
                    Some(_) => vec![],
                    None => vec![Finding {
                        severity: Severity::Info,
                        code: "MUTATION_RUN_COMPLETE".to_string(),
                        message: "Mutation testing completed — check mutants.out/ for details"
                            .to_string(),
                        reproduce_cmd: Some("cargo mutants 2>&1".to_string()),
                        ..Default::default()
                    }],
                };

                Ok(LayerResult {
                    name: "structural".to_string(),
                    runner: "cargo-mutants".to_string(),
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
                runner: "cargo-mutants".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "MUTATION_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run cargo-mutants: {}", e),
                    reproduce_cmd: Some("cargo mutants 2>&1".to_string()),
                    suggestion: Some("Install cargo-mutants: `cargo install cargo-mutants`".to_string()),
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

fn parse_mutation_score(output: &str) -> Option<f64> {
    for line in output.lines() {
        // cargo-mutants format: "N mutants tested in Xs: M missed, K caught[, U unviable]"
        if line.contains("mutants tested") && line.contains("caught") {
            let caught = count_before_label(line, "caught");
            let missed = count_before_label(line, "missed");
            let total = caught + missed;
            if total > 0 {
                return Some(caught as f64 / total as f64 * 100.0);
            }
        }
        // Legacy / alternative: "mutation score: X%"
        if line.contains("mutation score") && line.contains('%') {
            if let Some(pct) = line.split('%').next() {
                if let Some(n) = pct.split_whitespace().last() {
                    if let Ok(s) = n.parse::<f64>() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

/// Extract the integer token that immediately precedes `label` in `line`.
fn count_before_label(line: &str, label: &str) -> u64 {
    if let Some(idx) = line.find(label) {
        let before = line[..idx].trim_end();
        if let Some(tok) = before.split_whitespace().last() {
            return tok.trim_end_matches(',').parse().unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use proptest::prelude::*;

    fn info(lang: Language) -> ProjectInfo {
        ProjectInfo { language: lang, root: "/tmp".to_string(), has_tests: false, package_name: None }
    }

    #[test]
    fn name_is_cargo_mutants() {
        assert_eq!(MutantsRunner::default().name(), "cargo-mutants");
    }

    #[test]
    fn layer_is_structural() {
        assert!(matches!(MutantsRunner::default().layer(), crate::plugin::Layer::Structural));
    }

    #[test]
    fn skip_message_nonempty_and_mentions_cargo_mutants() {
        let msg = MutantsRunner::default().skip_message();
        assert!(!msg.is_empty());
        assert!(msg.contains("cargo-mutants") || msg.contains("cargo install"));
    }

    #[test]
    fn not_available_for_typescript() {
        assert!(!MutantsRunner::default().is_available(&info(Language::TypeScript)));
    }

    #[test]
    fn not_available_for_go() {
        assert!(!MutantsRunner::default().is_available(&info(Language::Go)));
    }

    #[test]
    fn parses_new_format_with_unviable() {
        let line = "461 mutants tested in 8m: 395 missed, 37 caught, 29 unviable";
        let score = parse_mutation_score(line).unwrap();
        // 37 / (37 + 395) = 8.56...
        assert!((score - 37.0 / 432.0 * 100.0).abs() < 0.01);
    }

    #[test]
    fn parses_new_format_without_unviable() {
        let line = "17 mutants tested in 23s: 11 missed, 6 caught";
        let score = parse_mutation_score(line).unwrap();
        assert!((score - 6.0 / 17.0 * 100.0).abs() < 0.01);
    }

    #[test]
    fn parses_perfect_score() {
        let line = "10 mutants tested in 5s: 0 missed, 10 caught";
        let score = parse_mutation_score(line).unwrap();
        assert!((score - 100.0).abs() < 0.01);
    }

    #[test]
    fn parses_zero_score() {
        let line = "10 mutants tested in 5s: 10 missed, 0 caught";
        let score = parse_mutation_score(line).unwrap();
        assert!((score - 0.0).abs() < 0.01);
    }

    #[test]
    fn returns_none_for_empty_input() {
        assert!(parse_mutation_score("").is_none());
        assert!(parse_mutation_score("no relevant output here").is_none());
    }

    #[test]
    fn parses_legacy_percent_format() {
        let line = "mutation score: 87.50%";
        let score = parse_mutation_score(line).unwrap();
        assert!((score - 87.5).abs() < 0.01);
    }

    proptest! {
        #[test]
        fn parse_mutation_score_never_panics(s in ".*") {
            let _ = parse_mutation_score(&s);
        }

        #[test]
        fn count_before_label_never_panics(line in ".*", label in "[a-z]+") {
            let _ = count_before_label(&line, &label);
        }

        #[test]
        fn score_from_caught_missed_is_in_range(
            caught in 0u64..1000u64,
            missed in 0u64..1000u64,
        ) {
            let line = format!(
                "{} mutants tested in 1s: {} missed, {} caught",
                caught + missed,
                missed,
                caught,
            );
            if caught + missed > 0 {
                let score = parse_mutation_score(&line).unwrap();
                prop_assert!(score >= 0.0 && score <= 100.0);
                let expected = caught as f64 / (caught + missed) as f64 * 100.0;
                prop_assert!((score - expected).abs() < 0.01);
            }
        }
    }
}
