/// Report comparison for regression detection.
///
/// Finding identity is `(severity, code, location, message)` — codes alone repeat
/// across files, so location disambiguates the same issue class at different sites.
/// Skipped is treated as neutral in both directions: transitioning to Skipped is
/// never a regression (runner became non-applicable) and never an improvement
/// (a missing runner is not progress). Transitioning from Skipped is similarly
/// ignored to avoid false improvements when a runner starts running again.
use crate::report::{BarzelReport, LayerStatus, ReportStatus, Severity};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Status ordering for worsening/improving comparisons. Higher = worse.
fn status_rank(s: LayerStatus) -> u8 {
    match s {
        LayerStatus::Pass => 0,
        LayerStatus::Skipped => 1, // conservatively between Pass and Partial
        LayerStatus::Partial => 2,
        LayerStatus::Fail => 3,
    }
}

fn status_worsened(from: LayerStatus, to: LayerStatus) -> bool {
    // Skipped is neutral in both directions. If a runner transitions from Skipped
    // to Fail, NewFinding regressions will surface any Critical/High issues found —
    // a StatusWorsened on top would be contradictory and redundant.
    if from == LayerStatus::Skipped || to == LayerStatus::Skipped { return false; }
    status_rank(to) > status_rank(from)
}

fn status_improved(from: LayerStatus, to: LayerStatus) -> bool {
    // Skipped is neutral in both directions
    if from == LayerStatus::Skipped || to == LayerStatus::Skipped { return false; }
    status_rank(to) < status_rank(from)
}

/// Stable composite identity for a finding within a layer.
#[derive(PartialEq, Eq, Hash, Clone)]
struct FindingKey {
    severity_str: String, // serialized to avoid Severity not implementing Hash
    code: String,
    location: Option<String>,
    message: String,
}

impl FindingKey {
    fn from(f: &crate::report::Finding) -> Self {
        Self {
            severity_str: format!("{:?}", f.severity),
            code: f.code.clone(),
            location: f.location.clone(),
            message: f.message.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Regressed,
    Improved,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryDelta {
    /// Positive = more findings in head; negative = resolved.
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub total: i64,
}

/// A finding that appeared or disappeared between baseline and head.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingChange {
    pub code: String,
    pub severity: Severity,
    pub location: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerChange {
    pub layer: String,
    pub runner: String,
    /// Status in baseline (None if layer did not exist in baseline).
    pub status_from: Option<LayerStatus>,
    /// Status in head (None if layer was removed).
    pub status_to: Option<LayerStatus>,
    /// Findings present in head but not in baseline for this layer.
    pub new_findings: Vec<FindingChange>,
    /// Findings present in baseline but not in head (resolved).
    pub resolved_findings: Vec<FindingChange>,
    /// Coverage change (head − baseline). None when either report lacks coverage.
    pub coverage_delta: Option<f64>,
    /// Mutation score change (head − baseline). None when either lacks mutation score.
    pub mutation_score_delta: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Regression {
    StatusWorsened {
        layer: String,
        runner: String,
        from: LayerStatus,
        to: LayerStatus,
    },
    NewFinding {
        layer: String,
        runner: String,
        code: String,
        severity: Severity,
        message: String,
        location: Option<String>,
    },
    CoverageDrop {
        layer: String,
        runner: String,
        from: f64,
        to: f64,
        delta: f64,
    },
    MutationScoreDrop {
        layer: String,
        runner: String,
        from: f64,
        to: f64,
        delta: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Improvement {
    StatusImproved {
        layer: String,
        runner: String,
        from: LayerStatus,
        to: LayerStatus,
    },
    FindingResolved {
        layer: String,
        runner: String,
        code: String,
        severity: Severity,
        location: Option<String>,
    },
    CoverageImproved {
        layer: String,
        runner: String,
        from: f64,
        to: f64,
        delta: f64,
    },
    MutationScoreImproved {
        layer: String,
        runner: String,
        from: f64,
        to: f64,
        delta: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportComparison {
    pub baseline_id: String,
    pub head_id: String,
    pub baseline_timestamp: DateTime<Utc>,
    pub head_timestamp: DateTime<Utc>,
    pub baseline_status: ReportStatus,
    pub head_status: ReportStatus,
    pub summary_delta: SummaryDelta,
    pub layer_changes: Vec<LayerChange>,
    /// Sorted by severity priority, then layer, runner, code, location.
    pub regressions: Vec<Regression>,
    pub improvements: Vec<Improvement>,
    pub verdict: Verdict,
}

/// Compare `baseline` (older) against `head` (newer).
pub fn compare_reports(baseline: &BarzelReport, head: &BarzelReport) -> ReportComparison {
    let summary_delta = SummaryDelta {
        critical: head.summary.critical as i64 - baseline.summary.critical as i64,
        high: head.summary.high as i64 - baseline.summary.high as i64,
        medium: head.summary.medium as i64 - baseline.summary.medium as i64,
        low: head.summary.low as i64 - baseline.summary.low as i64,
        total: head.summary.total_findings as i64 - baseline.summary.total_findings as i64,
    };

    let mut regressions: Vec<Regression> = Vec::new();
    let mut improvements: Vec<Improvement> = Vec::new();
    let mut layer_changes: Vec<LayerChange> = Vec::new();

    // Ordered union of (layer_name, runner) pairs from both reports
    let mut layer_keys: Vec<(String, String)> = Vec::new();
    for l in &baseline.layers { layer_keys.push((l.name.clone(), l.runner.clone())); }
    for l in &head.layers {
        let key = (l.name.clone(), l.runner.clone());
        if !layer_keys.contains(&key) { layer_keys.push(key); }
    }

    for (layer_name, runner) in &layer_keys {
        let base_layer = baseline.layers.iter().find(|l| &l.name == layer_name && &l.runner == runner);
        let head_layer = head.layers.iter().find(|l| &l.name == layer_name && &l.runner == runner);

        let status_from = base_layer.map(|l| l.status);
        let status_to = head_layer.map(|l| l.status);

        // Finding sets keyed by (severity, code, location, message)
        let base_keys: HashSet<FindingKey> = base_layer
            .map(|l| l.findings.iter().map(FindingKey::from).collect())
            .unwrap_or_default();
        let head_keys: HashSet<FindingKey> = head_layer
            .map(|l| l.findings.iter().map(FindingKey::from).collect())
            .unwrap_or_default();

        let new_keys: Vec<&FindingKey> = head_keys.difference(&base_keys).collect();
        let resolved_keys: Vec<&FindingKey> = base_keys.difference(&head_keys).collect();

        // Build FindingChange lists — look up original Finding for full context
        let mut new_findings: Vec<FindingChange> = new_keys.iter().map(|k| {
            let f = head_layer.and_then(|l| l.findings.iter().find(|f| FindingKey::from(f) == **k));
            FindingChange {
                code: k.code.clone(),
                severity: f.map(|f| f.severity).unwrap_or(Severity::Info),
                location: k.location.clone(),
                message: k.message.clone(),
            }
        }).collect();
        let mut resolved_findings: Vec<FindingChange> = resolved_keys.iter().map(|k| {
            let f = base_layer.and_then(|l| l.findings.iter().find(|f| FindingKey::from(f) == **k));
            FindingChange {
                code: k.code.clone(),
                severity: f.map(|f| f.severity).unwrap_or(Severity::Info),
                location: k.location.clone(),
                message: k.message.clone(),
            }
        }).collect();

        // Sort for determinism
        sort_finding_changes(&mut new_findings);
        sort_finding_changes(&mut resolved_findings);

        // Coverage and mutation score
        let coverage_delta = delta_opt(
            base_layer.and_then(|l| l.metrics.coverage),
            head_layer.and_then(|l| l.metrics.coverage),
        );
        let mutation_score_delta = delta_opt(
            base_layer.and_then(|l| l.metrics.mutation_score),
            head_layer.and_then(|l| l.metrics.mutation_score),
        );

        // Status regressions / improvements
        if let (Some(from), Some(to)) = (status_from, status_to) {
            if status_worsened(from, to) {
                regressions.push(Regression::StatusWorsened { layer: layer_name.clone(), runner: runner.clone(), from, to });
            } else if status_improved(from, to) {
                improvements.push(Improvement::StatusImproved { layer: layer_name.clone(), runner: runner.clone(), from, to });
            }
        }

        // New Critical/High findings → regressions
        for fc in &new_findings {
            if matches!(fc.severity, Severity::Critical | Severity::High) {
                regressions.push(Regression::NewFinding {
                    layer: layer_name.clone(),
                    runner: runner.clone(),
                    code: fc.code.clone(),
                    severity: fc.severity,
                    message: fc.message.clone(),
                    location: fc.location.clone(),
                });
            }
        }

        // Resolved findings → improvements
        for fc in &resolved_findings {
            improvements.push(Improvement::FindingResolved {
                layer: layer_name.clone(),
                runner: runner.clone(),
                code: fc.code.clone(),
                severity: fc.severity,
                location: fc.location.clone(),
            });
        }

        // Coverage
        if let Some(delta) = coverage_delta {
            let from = base_layer.and_then(|l| l.metrics.coverage).unwrap_or(0.0);
            let to = head_layer.and_then(|l| l.metrics.coverage).unwrap_or(0.0);
            if delta < -0.1 {
                regressions.push(Regression::CoverageDrop { layer: layer_name.clone(), runner: runner.clone(), from, to, delta });
            } else if delta > 0.1 {
                improvements.push(Improvement::CoverageImproved { layer: layer_name.clone(), runner: runner.clone(), from, to, delta });
            }
        }

        // Mutation score
        if let Some(delta) = mutation_score_delta {
            let from = base_layer.and_then(|l| l.metrics.mutation_score).unwrap_or(0.0);
            let to = head_layer.and_then(|l| l.metrics.mutation_score).unwrap_or(0.0);
            if delta < -0.1 {
                regressions.push(Regression::MutationScoreDrop { layer: layer_name.clone(), runner: runner.clone(), from, to, delta });
            } else if delta > 0.1 {
                improvements.push(Improvement::MutationScoreImproved { layer: layer_name.clone(), runner: runner.clone(), from, to, delta });
            }
        }

        layer_changes.push(LayerChange {
            layer: layer_name.clone(),
            runner: runner.clone(),
            status_from,
            status_to,
            new_findings,
            resolved_findings,
            coverage_delta,
            mutation_score_delta,
        });
    }

    // Sort regressions: severity priority, then layer, runner, code, location
    regressions.sort_by_key(regression_sort_key);

    let verdict = if !regressions.is_empty() {
        Verdict::Regressed
    } else if !improvements.is_empty() {
        Verdict::Improved
    } else {
        Verdict::Unchanged
    };

    ReportComparison {
        baseline_id: baseline.id.clone(),
        head_id: head.id.clone(),
        baseline_timestamp: baseline.timestamp,
        head_timestamp: head.timestamp,
        baseline_status: baseline.status,
        head_status: head.status,
        summary_delta,
        layer_changes,
        regressions,
        improvements,
        verdict,
    }
}

fn delta_opt(base: Option<f64>, head: Option<f64>) -> Option<f64> {
    match (base, head) { (Some(b), Some(h)) => Some(h - b), _ => None }
}

fn severity_priority(s: Severity) -> u8 {
    match s { Severity::Critical => 0, Severity::High => 1, Severity::Medium => 2, Severity::Low => 3, Severity::Info => 4 }
}

fn sort_finding_changes(v: &mut [FindingChange]) {
    v.sort_by_key(|f| (severity_priority(f.severity), f.code.clone(), f.location.clone().unwrap_or_default()));
}

fn regression_sort_key(r: &Regression) -> (u8, String, String, String, String) {
    match r {
        Regression::NewFinding { severity, layer, runner, code, location, .. } =>
            (severity_priority(*severity), layer.clone(), runner.clone(), code.clone(), location.clone().unwrap_or_default()),
        Regression::StatusWorsened { layer, runner, .. } =>
            (0, layer.clone(), runner.clone(), String::new(), String::new()),
        Regression::CoverageDrop { layer, runner, .. } =>
            (1, layer.clone(), runner.clone(), "coverage".to_string(), String::new()),
        Regression::MutationScoreDrop { layer, runner, .. } =>
            (1, layer.clone(), runner.clone(), "mutation_score".to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};

    fn project() -> ProjectInfo {
        ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    fn layer(name: &str, runner: &str, status: LayerStatus, findings: Vec<Finding>) -> LayerResult {
        LayerResult { name: name.to_string(), runner: runner.to_string(), status, findings, metrics: LayerMetrics::default(), duration_ms: 0 }
    }

    fn layer_with_metrics(name: &str, runner: &str, status: LayerStatus, coverage: Option<f64>, mutation_score: Option<f64>) -> LayerResult {
        LayerResult {
            name: name.to_string(), runner: runner.to_string(), status, findings: vec![],
            metrics: LayerMetrics { coverage, mutation_score, ..Default::default() },
            duration_ms: 0,
        }
    }

    fn finding(code: &str, severity: Severity, location: &str) -> Finding {
        Finding {
            severity, code: code.to_string(), message: format!("{} issue at {}", code, location),
            location: Some(location.to_string()),
            reproduce_cmd: Some(format!("grep {} {}", code, location)),
            suggestion: None,
        }
    }

    fn make_report(layers: Vec<LayerResult>) -> BarzelReport {
        let mut r = BarzelReport::new(project());
        for l in layers { r.add_layer(l); }
        r
    }

    // ── verdict ───────────────────────────────────────────────────────────────

    #[test]
    fn identical_reports_verdict_unchanged() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        assert_eq!(compare_reports(&baseline, &head).verdict, Verdict::Unchanged);
    }

    // ── finding identity uses location ────────────────────────────────────────

    #[test]
    fn same_code_different_location_are_two_findings() {
        // Two instances of the same code at different file locations must be treated independently.
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("SQL_INJECTION", Severity::Critical, "src/a.py:10"),
        ])]);
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("SQL_INJECTION", Severity::Critical, "src/b.py:42"), // different file
        ])]);
        let cmp = compare_reports(&baseline, &head);
        // Old location resolved, new location appeared → both regression and improvement
        assert!(cmp.regressions.iter().any(|r| matches!(
            r, Regression::NewFinding { location, .. } if location.as_deref() == Some("src/b.py:42")
        )));
        assert!(cmp.improvements.iter().any(|i| matches!(
            i, Improvement::FindingResolved { location, .. } if location.as_deref() == Some("src/a.py:10")
        )));
    }

    #[test]
    fn same_code_same_location_is_unchanged() {
        let f = finding("EVAL_LLM_OUTPUT", Severity::Critical, "agent.py:5");
        let baseline = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Fail, vec![f.clone()])]);
        let head = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Fail, vec![f])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().all(|r| !matches!(r, Regression::NewFinding { .. })));
        assert!(cmp.improvements.iter().all(|i| !matches!(i, Improvement::FindingResolved { .. })));
    }

    // ── regressions ───────────────────────────────────────────────────────────

    #[test]
    fn new_critical_finding_is_regression() {
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("SQL_INJECTION", Severity::Critical, "src/app.py:10"),
        ])]);
        let cmp = compare_reports(&baseline, &head);
        assert_eq!(cmp.verdict, Verdict::Regressed);
        assert!(cmp.regressions.iter().any(|r| matches!(
            r, Regression::NewFinding { code, severity: Severity::Critical, .. } if code == "SQL_INJECTION"
        )));
    }

    #[test]
    fn new_high_finding_is_regression() {
        let baseline = make_report(vec![layer("hostile", "semgrep", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("hostile", "semgrep", LayerStatus::Partial, vec![
            finding("HARDCODED_SECRET", Severity::High, "config.py:3"),
        ])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().any(|r| matches!(r, Regression::NewFinding { severity: Severity::High, .. })));
    }

    #[test]
    fn new_medium_finding_not_in_regressions() {
        let baseline = make_report(vec![layer("hostile", "semgrep", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("hostile", "semgrep", LayerStatus::Partial, vec![
            finding("MISSING_VALIDATION", Severity::Medium, "api.py:15"),
        ])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.regressions.iter().any(|r| matches!(r, Regression::NewFinding { .. })));
        // But visible in layer_changes
        let lc = cmp.layer_changes.iter().find(|l| l.layer == "hostile").unwrap();
        assert!(lc.new_findings.iter().any(|f| f.code == "MISSING_VALIDATION"));
    }

    #[test]
    fn status_worsened_pass_to_fail_is_regression() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Fail, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().any(|r| matches!(
            r, Regression::StatusWorsened { from: LayerStatus::Pass, to: LayerStatus::Fail, .. }
        )));
    }

    #[test]
    fn status_worsened_pass_to_partial_is_regression() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Partial, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().any(|r| matches!(r, Regression::StatusWorsened { .. })));
    }

    #[test]
    fn skipped_replacing_pass_is_not_regression() {
        // Conservative: Skipped might mean runner was not applicable (e.g. no AI deps)
        let baseline = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Skipped, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.regressions.iter().any(|r| matches!(r, Regression::StatusWorsened { .. })),
            "Skipped replacing Pass must not be flagged as regression");
    }

    #[test]
    fn skipped_replacing_fail_is_not_regression() {
        let baseline = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Fail, vec![])]);
        let head = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Skipped, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.regressions.iter().any(|r| matches!(r, Regression::StatusWorsened { .. })),
            "Skipped must never be considered worse than Fail");
    }

    #[test]
    fn skipped_to_fail_is_not_status_regression() {
        // A runner transitioning from Skipped to Fail should not produce StatusWorsened —
        // NewFinding regressions will surface any Critical/High issues instead.
        let baseline = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Skipped, vec![])]);
        let head = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Fail, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.regressions.iter().any(|r| matches!(r, Regression::StatusWorsened { .. })),
            "Skipped → Fail must not produce StatusWorsened");
    }

    #[test]
    fn fail_to_skipped_is_not_improvement() {
        // Skipped means the runner became non-applicable, not that the problem was fixed.
        let baseline = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Fail, vec![])]);
        let head = make_report(vec![layer("hostile", "ai-sec", LayerStatus::Skipped, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.improvements.iter().any(|i| matches!(i, Improvement::StatusImproved { .. })),
            "Fail → Skipped must not be counted as an improvement");
    }

    #[test]
    fn partial_to_skipped_is_not_improvement() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Partial, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Skipped, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(!cmp.improvements.iter().any(|i| matches!(i, Improvement::StatusImproved { .. })),
            "Partial → Skipped must not be counted as an improvement");
    }

    #[test]
    fn coverage_drop_is_regression() {
        let baseline = make_report(vec![layer_with_metrics("logic", "pytest", LayerStatus::Pass, Some(90.0), None)]);
        let head = make_report(vec![layer_with_metrics("logic", "pytest", LayerStatus::Pass, Some(70.0), None)]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().any(|r| matches!(
            r, Regression::CoverageDrop { from, to, .. }
            if (*from - 90.0).abs() < 0.01 && (*to - 70.0).abs() < 0.01
        )));
    }

    #[test]
    fn mutation_score_drop_is_regression() {
        let baseline = make_report(vec![layer_with_metrics("structural", "mutmut", LayerStatus::Pass, None, Some(95.0))]);
        let head = make_report(vec![layer_with_metrics("structural", "mutmut", LayerStatus::Partial, None, Some(70.0))]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.regressions.iter().any(|r| matches!(r, Regression::MutationScoreDrop { .. })));
    }

    // ── improvements ─────────────────────────────────────────────────────────

    #[test]
    fn resolved_critical_finding_is_improvement() {
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("SQL_INJECTION", Severity::Critical, "src/app.py:10"),
        ])]);
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Pass, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert_eq!(cmp.verdict, Verdict::Improved);
        assert!(cmp.improvements.iter().any(|i| matches!(
            i, Improvement::FindingResolved { code, .. } if code == "SQL_INJECTION"
        )));
    }

    #[test]
    fn status_improved_fail_to_pass_is_improvement() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Fail, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        assert!(cmp.improvements.iter().any(|i| matches!(
            i, Improvement::StatusImproved { from: LayerStatus::Fail, to: LayerStatus::Pass, .. }
        )));
    }

    #[test]
    fn coverage_increase_is_improvement() {
        let baseline = make_report(vec![layer_with_metrics("logic", "pytest", LayerStatus::Pass, Some(60.0), None)]);
        let head = make_report(vec![layer_with_metrics("logic", "pytest", LayerStatus::Pass, Some(85.0), None)]);
        assert!(compare_reports(&baseline, &head).improvements.iter().any(|i| matches!(i, Improvement::CoverageImproved { .. })));
    }

    // ── summary delta ─────────────────────────────────────────────────────────

    #[test]
    fn summary_delta_counts_correctly() {
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("A", Severity::Critical, "x.py:1"),
            finding("B", Severity::High, "x.py:2"),
        ])]);
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Partial, vec![
            finding("B", Severity::High, "x.py:2"),
            finding("C", Severity::Medium, "x.py:3"),
        ])]);
        let cmp = compare_reports(&baseline, &head);
        assert_eq!(cmp.summary_delta.critical, -1);
        assert_eq!(cmp.summary_delta.high, 0);
        assert_eq!(cmp.summary_delta.medium, 1);
        assert_eq!(cmp.summary_delta.total, 0);
    }

    // ── deterministic output ──────────────────────────────────────────────────

    #[test]
    fn regressions_sorted_critical_before_high() {
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("HIGH_ISSUE", Severity::High, "a.py:1"),
            finding("CRITICAL_ISSUE", Severity::Critical, "b.py:2"),
        ])]);
        let cmp = compare_reports(&baseline, &head);
        let new_findings: Vec<&Regression> = cmp.regressions.iter()
            .filter(|r| matches!(r, Regression::NewFinding { .. }))
            .collect();
        assert_eq!(new_findings.len(), 2);
        // Critical must come before High
        let first_sev = if let Regression::NewFinding { severity, .. } = new_findings[0] { *severity } else { Severity::Info };
        assert!(matches!(first_sev, Severity::Critical));
    }

    // ── serialization ─────────────────────────────────────────────────────────

    #[test]
    fn comparison_serializes_and_deserializes() {
        let baseline = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let head = make_report(vec![layer("logic", "pytest", LayerStatus::Pass, vec![])]);
        let cmp = compare_reports(&baseline, &head);
        let json = serde_json::to_string(&cmp).unwrap();
        let restored: ReportComparison = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.verdict, cmp.verdict);
        assert_eq!(restored.baseline_id, cmp.baseline_id);
    }

    #[test]
    fn comparison_from_saved_reports() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let baseline = make_report(vec![layer("hostile", "bandit", LayerStatus::Fail, vec![
            finding("SQL_INJECTION", Severity::Critical, "src/app.py:10"),
        ])]);
        baseline.save(dir.path()).unwrap();
        let head = make_report(vec![layer("hostile", "bandit", LayerStatus::Pass, vec![])]);
        head.save(dir.path()).unwrap();
        let lb = BarzelReport::load_by_id(dir.path(), &baseline.id).unwrap().unwrap();
        let lh = BarzelReport::load_by_id(dir.path(), &head.id).unwrap().unwrap();
        let cmp = compare_reports(&lb, &lh);
        assert_eq!(cmp.verdict, Verdict::Improved);
    }
}
