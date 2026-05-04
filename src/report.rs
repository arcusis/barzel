use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

use crate::detect::ProjectInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarzelReport {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub project: ProjectInfo,
    pub layers: Vec<LayerResult>,
    pub summary: Summary,
    pub status: ReportStatus,
    /// Minimum severity from config that triggers a non-zero exit: critical | high | medium | any
    #[serde(default = "default_fail_on")]
    pub fail_on: String,
    /// Per-package reports for workspace/monorepo projects (empty for single-project)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_members: Vec<WorkspaceMemberReport>,
    /// Git revision passed to --since (set whenever --since was used, even on fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_since: Option<String>,
    /// Number of changed files detected in diff mode (absent when lookup failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_changed_files: Option<usize>,
    /// Reason diff mode fell back to full run (absent when diff succeeded or not requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_fallback_reason: Option<String>,
}

/// Package-level report nested inside a workspace aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMemberReport {
    pub package_path: String,
    pub language: String,
    pub status: ReportStatus,
    pub layers: Vec<LayerResult>,
    pub summary: Summary,
}

fn default_fail_on() -> String { "high".to_string() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerResult {
    pub name: String,
    pub runner: String,
    pub status: LayerStatus,
    pub findings: Vec<Finding>,
    pub metrics: LayerMetrics,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub location: Option<String>,
    /// Exact shell command an AI agent can run to reproduce this finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduce_cmd: Option<String>,
    /// Actionable fix suggestion for an AI agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayerMetrics {
    pub tests_run: u64,
    pub passed: u64,
    pub failed: u64,
    pub coverage: Option<f64>,
    pub mutation_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub overall_status: ReportStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Pass,
    Partial,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerStatus {
    Pass,
    Partial,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Default for Finding {
    fn default() -> Self {
        Self {
            severity: Severity::Info,
            code: String::new(),
            message: String::new(),
            location: None,
            reproduce_cmd: None,
            suggestion: None,
        }
    }
}

const NAMED_LAYERS: &[&str] = &["logic", "structural", "hostile", "operational"];

fn reproduce_cmd_fallback(layer_name: &str) -> String {
    if NAMED_LAYERS.contains(&layer_name) {
        format!("barzel run --layer {} --json", layer_name)
    } else {
        "barzel run --json".to_string()
    }
}

impl BarzelReport {
    pub fn new(project: ProjectInfo) -> Self {
        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now();

        Self {
            id,
            timestamp,
            project,
            layers: Vec::new(),
            summary: Summary {
                total_findings: 0,
                critical: 0,
                high: 0,
                medium: 0,
                low: 0,
                overall_status: ReportStatus::Pass,
            },
            status: ReportStatus::Pass,
            fail_on: "high".to_string(),
            workspace_members: Vec::new(),
            diff_since: None,
            diff_changed_files: None,
            diff_fallback_reason: None,
        }
    }

    pub fn add_layer(&mut self, mut layer: LayerResult) {
        // Every finding must carry a reproduce_cmd so agents can always re-run a finding.
        let fallback = reproduce_cmd_fallback(&layer.name);
        for finding in &mut layer.findings {
            if finding.reproduce_cmd.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                finding.reproduce_cmd = Some(fallback.clone());
            }
        }

        for finding in &layer.findings {
            self.summary.total_findings += 1;
            match finding.severity {
                Severity::Critical => self.summary.critical += 1,
                Severity::High => self.summary.high += 1,
                Severity::Medium => self.summary.medium += 1,
                Severity::Low => self.summary.low += 1,
                Severity::Info => {}
            }
        }

        if self.summary.critical > 0 {
            self.status = ReportStatus::Fail;
        } else if self.summary.high > 0 || self.summary.medium > 0 {
            self.status = ReportStatus::Partial;
        }

        self.summary.overall_status = self.status;
        self.layers.push(layer);
    }

    /// Recompute summary counts and overall status from the current layers.
    /// Call after injecting findings post-run (e.g. coverage threshold enforcement).
    pub fn recompute_summary(&mut self) {
        let mut s = Summary {
            total_findings: 0,
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            overall_status: ReportStatus::Pass,
        };
        for layer in &self.layers {
            for finding in &layer.findings {
                s.total_findings += 1;
                match finding.severity {
                    Severity::Critical => s.critical += 1,
                    Severity::High => s.high += 1,
                    Severity::Medium => s.medium += 1,
                    Severity::Low => s.low += 1,
                    Severity::Info => {}
                }
            }
        }
        let status = if s.critical > 0 {
            ReportStatus::Fail
        } else if s.high > 0 || s.medium > 0 {
            ReportStatus::Partial
        } else {
            ReportStatus::Pass
        };
        s.overall_status = status;
        self.summary = s;
        self.status = status;
    }

    pub fn save(&self, base_dir: &Path) -> crate::error::Result<std::path::PathBuf> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        std::fs::create_dir_all(&reports_dir)?;

        let id_prefix = self.id.get(..8).unwrap_or(&self.id);
        let filename = format!("report-{}-{}.json", self.timestamp.format("%Y%m%d-%H%M%S"), id_prefix);
        let path = reports_dir.join(filename);

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;

        Ok(path)
    }

    pub fn load_latest(base_dir: &Path) -> crate::error::Result<Option<Self>> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        if !reports_dir.exists() {
            return Ok(None);
        }

        // Deserialize every report and pick the one with the greatest timestamp.
        // Filename ordering is unreliable when two reports share the same
        // filename-second (same-second saves now have different ID suffixes).
        // Full DateTime has sub-second precision, so this is always correct.
        // Path is used as a deterministic tie-breaker in the pathological case
        // where two reports have identical timestamps.
        let mut best: Option<(Self, std::path::PathBuf)> = None;

        for entry in std::fs::read_dir(&reports_dir)?.flatten() {
            let path = entry.path();
            if !path.extension().map(|x| x == "json").unwrap_or(false) { continue; }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(report) = serde_json::from_str::<Self>(&content) {
                    let is_newer = best.as_ref().is_none_or(|(prev, prev_path)| {
                        report.timestamp > prev.timestamp
                            || (report.timestamp == prev.timestamp && path > *prev_path)
                    });
                    if is_newer {
                        best = Some((report, path));
                    }
                }
            }
        }

        Ok(best.map(|(report, _)| report))
    }

    /// Load a report by exact full ID or by unambiguous prefix.
    ///
    /// Resolution rules:
    /// - Exact full-ID match wins immediately, even if other reports share the prefix.
    /// - No matches -> `Ok(None)`.
    /// - Exactly one prefix match -> `Ok(Some(report))`.
    /// - More than one prefix match -> `Err` with an "ambiguous" message listing the
    ///   matching full IDs (sorted) so the caller can prompt for a longer prefix.
    pub fn load_by_id(base_dir: &Path, id: &str) -> crate::error::Result<Option<Self>> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        if !reports_dir.exists() {
            return Ok(None);
        }

        let mut prefix_matches: Vec<(String, Self)> = Vec::new();

        for entry in std::fs::read_dir(&reports_dir)?.flatten() {
            let path = entry.path();
            if !path.extension().map(|x| x == "json").unwrap_or(false) { continue; }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(report) = serde_json::from_str::<Self>(&content) {
                    if report.id == id {
                        // Exact full-ID match wins immediately; no ambiguity possible
                        return Ok(Some(report));
                    }
                    if report.id.starts_with(id) {
                        prefix_matches.push((report.id.clone(), report));
                    }
                }
            }
        }

        match prefix_matches.len() {
            0 => Ok(None),
            1 => Ok(Some(prefix_matches.remove(0).1)),
            _ => {
                let mut ids: Vec<String> = prefix_matches.into_iter().map(|(id, _)| id).collect();
                ids.sort();
                Err(crate::error::BarzelError::Detection(format!(
                    "ambiguous report id prefix '{}': {} reports match ({}). Use a longer prefix.",
                    id,
                    ids.len(),
                    ids.join(", ")
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use proptest::prelude::*;

    fn dummy_project() -> ProjectInfo {
        ProjectInfo {
            language: Language::Rust,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("test".to_string()),
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    fn layer_with_findings(findings: Vec<Finding>) -> LayerResult {
        LayerResult {
            name: "logic".to_string(),
            runner: "proptest".to_string(),
            status: LayerStatus::Pass,
            findings,
            metrics: LayerMetrics {
                tests_run: 0,
                passed: 0,
                failed: 0,
                coverage: None,
                mutation_score: None,
            },
            duration_ms: 0,
        }
    }

    fn finding(severity: Severity) -> Finding {
        Finding {
            severity,
            code: "TEST".to_string(),
            message: "test finding".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn add_layer_increments_total_findings() {
        let mut report = BarzelReport::new(dummy_project());
        assert_eq!(report.summary.total_findings, 0);

        report.add_layer(layer_with_findings(vec![finding(Severity::Info)]));
        assert_eq!(report.summary.total_findings, 1);

        report.add_layer(layer_with_findings(vec![
            finding(Severity::High),
            finding(Severity::Medium),
        ]));
        assert_eq!(report.summary.total_findings, 3);
    }

    #[test]
    fn critical_finding_makes_report_fail() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Critical)]));
        assert_eq!(report.status, ReportStatus::Fail);
    }

    #[test]
    fn info_findings_do_not_degrade_status() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![
            finding(Severity::Info),
            finding(Severity::Info),
        ]));
        assert_eq!(report.status, ReportStatus::Pass);
    }

    // ── Display implementations ───────────────────────────────────────────────

    #[test]
    fn report_status_display() {
        assert_eq!(ReportStatus::Pass.to_string(), "pass");
        assert_eq!(ReportStatus::Partial.to_string(), "partial");
        assert_eq!(ReportStatus::Fail.to_string(), "fail");
    }

    #[test]
    fn layer_status_display() {
        assert_eq!(LayerStatus::Pass.to_string(), "PASS");
        assert_eq!(LayerStatus::Fail.to_string(), "FAIL");
        assert_eq!(LayerStatus::Partial.to_string(), "PARTIAL");
        assert_eq!(LayerStatus::Skipped.to_string(), "SKIPPED");
    }

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Critical.to_string(), "CRITICAL");
        assert_eq!(Severity::High.to_string(), "HIGH");
        assert_eq!(Severity::Medium.to_string(), "MEDIUM");
        assert_eq!(Severity::Low.to_string(), "LOW");
        assert_eq!(Severity::Info.to_string(), "INFO");
    }

    // ── add_layer boundary conditions ─────────────────────────────────────────

    #[test]
    fn high_finding_makes_report_partial() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::High)]));
        assert_eq!(report.status, ReportStatus::Partial);
    }

    #[test]
    fn medium_finding_makes_report_partial() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Medium)]));
        assert_eq!(report.status, ReportStatus::Partial);
    }

    #[test]
    fn low_finding_does_not_change_status_but_increments_count() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Low)]));
        assert_eq!(report.status, ReportStatus::Pass);
        assert_eq!(report.summary.low, 1);  // catches low += 1 → *= 1 mutation
    }

    #[test]
    fn medium_finding_increments_medium_count() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Medium)]));
        assert_eq!(report.summary.medium, 1);
    }

    #[test]
    fn critical_overrides_partial() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::High)]));
        assert_eq!(report.status, ReportStatus::Partial);
        report.add_layer(layer_with_findings(vec![finding(Severity::Critical)]));
        assert_eq!(report.status, ReportStatus::Fail);
    }

    #[test]
    fn multiple_info_findings_count_in_total() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![
            finding(Severity::Info),
            finding(Severity::Info),
            finding(Severity::Info),
        ]));
        assert_eq!(report.summary.total_findings, 3);
        assert_eq!(report.status, ReportStatus::Pass); // info doesn't degrade
    }

    #[test]
    fn summary_overall_status_matches_report_status() {
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Critical)]));
        assert_eq!(report.summary.overall_status, report.status);
    }

    // ── reproduce_cmd invariant ───────────────────────────────────────────────

    fn layer_for_reproduce_test(layer_name: &str, findings: Vec<Finding>) -> LayerResult {
        LayerResult {
            name: layer_name.to_string(),
            runner: "test-runner".to_string(),
            status: LayerStatus::Pass,
            findings,
            metrics: LayerMetrics::default(),
            duration_ms: 0,
        }
    }

    #[test]
    fn add_layer_fills_missing_reproduce_cmd() {
        let mut report = BarzelReport::new(dummy_project());
        let f = Finding {
            severity: Severity::High,
            code: "MISSING".to_string(),
            message: "no reproduce_cmd set".to_string(),
            reproduce_cmd: None,
            ..Default::default()
        };
        report.add_layer(layer_for_reproduce_test("logic", vec![f]));
        let cmd = report.layers[0].findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert!(!cmd.is_empty(), "reproduce_cmd must be filled when None");
        assert!(cmd.contains("--layer logic"), "fallback must name the layer");
    }

    #[test]
    fn add_layer_fills_blank_reproduce_cmd() {
        let mut report = BarzelReport::new(dummy_project());
        let f = Finding {
            severity: Severity::Medium,
            code: "BLANK".to_string(),
            message: "blank reproduce_cmd".to_string(),
            reproduce_cmd: Some("   ".to_string()),
            ..Default::default()
        };
        report.add_layer(layer_for_reproduce_test("structural", vec![f]));
        let cmd = report.layers[0].findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert!(!cmd.trim().is_empty(), "reproduce_cmd must be filled when blank");
        assert!(cmd.contains("--layer structural"));
    }

    #[test]
    fn add_layer_preserves_existing_reproduce_cmd() {
        let mut report = BarzelReport::new(dummy_project());
        let f = Finding {
            severity: Severity::Low,
            code: "EXISTING".to_string(),
            message: "has reproduce_cmd already".to_string(),
            reproduce_cmd: Some("cargo test --test my_test".to_string()),
            ..Default::default()
        };
        report.add_layer(layer_for_reproduce_test("logic", vec![f]));
        let cmd = report.layers[0].findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert_eq!(cmd, "cargo test --test my_test", "existing reproduce_cmd must not be overwritten");
    }

    #[test]
    fn add_layer_unknown_layer_name_uses_generic_fallback() {
        let mut report = BarzelReport::new(dummy_project());
        let f = Finding {
            severity: Severity::Info,
            code: "UNKNOWN_LAYER".to_string(),
            message: "layer name not in the known set".to_string(),
            reproduce_cmd: None,
            ..Default::default()
        };
        report.add_layer(layer_for_reproduce_test("custom-layer", vec![f]));
        let cmd = report.layers[0].findings[0].reproduce_cmd.as_deref().unwrap_or("");
        assert_eq!(cmd, "barzel run --json", "unknown layer must fall back to generic command");
    }

    // ── save / load ───────────────────────────────────────────────────────────

    #[test]
    fn save_creates_file_in_reports_dir() {
        let dir = tempfile::tempdir().unwrap();
        let report = BarzelReport::new(dummy_project());
        let path = report.save(dir.path()).unwrap();
        assert!(path.exists());
        assert!(path.extension().map(|e| e == "json").unwrap_or(false));
    }

    #[test]
    fn load_latest_returns_saved_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = BarzelReport::new(dummy_project());
        report.add_layer(layer_with_findings(vec![finding(Severity::Info)]));
        let _ = report.save(dir.path()).unwrap();

        let loaded = BarzelReport::load_latest(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.id, report.id);
        assert_eq!(loaded.summary.total_findings, 1);
    }

    #[test]
    fn load_latest_returns_none_when_no_reports() {
        let dir = tempfile::tempdir().unwrap();
        let result = BarzelReport::load_latest(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_latest_uses_timestamp_not_filename_for_same_second() {
        // Two reports with the same filename-second but different subsecond timestamps.
        // load_latest must return the one with the greater DateTime, not the one
        // that sorts later by filename.
        use chrono::{Duration, TimeZone};
        let dir = tempfile::tempdir().unwrap();

        let mut older = BarzelReport::new(dummy_project());
        let mut newer = BarzelReport::new(dummy_project());

        // Use a fixed timestamp with nanos=0 so base_time + 500ms is deterministically
        // in the same filename-second regardless of when the test runs.
        let base_time = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();
        older.timestamp = base_time;
        newer.timestamp = base_time + Duration::milliseconds(500);

        // Force older to sort later by filename (higher ID prefix) to expose the bug.
        older.id = "zzzzzzzz-0000-0000-0000-000000000000".to_string();
        newer.id = "00000000-0000-0000-0000-000000000000".to_string();

        older.save(dir.path()).unwrap();
        newer.save(dir.path()).unwrap();

        let latest = BarzelReport::load_latest(dir.path()).unwrap().unwrap();
        assert_eq!(
            latest.id, newer.id,
            "load_latest must return the report with the greater timestamp, not the later filename"
        );
    }

    #[test]
    fn load_by_id_finds_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = BarzelReport::new(dummy_project());
        let id_prefix = &report.id[..8];
        let _ = report.save(dir.path()).unwrap();

        let loaded = BarzelReport::load_by_id(dir.path(), id_prefix).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, report.id);
    }

    /// Build and save two reports whose IDs share a given 8-char prefix but differ in the rest.
    /// Returns (dir, id_of_report_1, id_of_report_2).
    fn two_reports_sharing_prefix(prefix: &str) -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut r1 = BarzelReport::new(dummy_project());
        let mut r2 = BarzelReport::new(dummy_project());
        // Override IDs to share a prefix but differ after it.
        r1.id = format!("{}-aaaa-aaaa-aaaa-aaaaaaaaaaaa", prefix);
        r2.id = format!("{}-bbbb-bbbb-bbbb-bbbbbbbbbbbb", prefix);
        // Give them different timestamps so filenames are distinct.
        r2.timestamp = r1.timestamp + chrono::Duration::seconds(1);
        let id1 = r1.id.clone();
        let id2 = r2.id.clone();
        r1.save(dir.path()).unwrap();
        r2.save(dir.path()).unwrap();
        (dir, id1, id2)
    }

    #[test]
    fn load_by_id_ambiguous_prefix_returns_err() {
        let prefix = "abcdef01";
        let (dir, id1, id2) = two_reports_sharing_prefix(prefix);

        let result = BarzelReport::load_by_id(dir.path(), prefix);
        assert!(result.is_err(), "ambiguous prefix must return Err, not Ok");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ambiguous"), "error must contain 'ambiguous': {msg}");
        assert!(msg.contains(prefix), "error must name the prefix: {msg}");
        assert!(msg.contains(&id1), "error must list first matching id: {msg}");
        assert!(msg.contains(&id2), "error must list second matching id: {msg}");
    }

    #[test]
    fn load_by_id_exact_full_id_wins_over_ambiguous_prefix() {
        let prefix = "abcdef01";
        let (dir, id1, _id2) = two_reports_sharing_prefix(prefix);

        // Using the full ID of report 1 must return report 1 exactly; no ambiguity.
        let loaded = BarzelReport::load_by_id(dir.path(), &id1)
            .expect("exact full id must not return Err")
            .expect("exact full id must return Some");
        assert_eq!(loaded.id, id1, "must return the exact-matched report");
    }

    #[test]
    fn load_by_id_longer_unique_prefix_resolves_unambiguously() {
        let prefix = "abcdef01";
        let (dir, _id1, id2) = two_reports_sharing_prefix(prefix);

        // id2 = "abcdef01-bbbb-..."; "abcdef01-b" is unique to it.
        let unique_prefix = &id2[..10]; // "abcdef01-b"
        let loaded = BarzelReport::load_by_id(dir.path(), unique_prefix)
            .expect("unique longer prefix must not return Err")
            .expect("unique longer prefix must return Some");
        assert_eq!(loaded.id, id2, "must return the report whose id matches the longer prefix");
    }

    #[test]
    fn load_by_id_returns_none_for_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let report = BarzelReport::new(dummy_project());
        let _ = report.save(dir.path()).unwrap();
        let result = BarzelReport::load_by_id(dir.path(), "00000000").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn same_second_reports_get_distinct_filenames() {
        // Force identical timestamps so the test deterministically exercises the collision case.
        let dir = tempfile::tempdir().unwrap();
        let r1 = BarzelReport::new(dummy_project());
        let mut r2 = BarzelReport::new(dummy_project());
        r2.timestamp = r1.timestamp; // same second, different IDs
        let p1 = r1.save(dir.path()).unwrap();
        let p2 = r2.save(dir.path()).unwrap();
        assert_ne!(p1, p2, "two reports with the same timestamp must not share a filename");
        // Both are loadable by ID
        assert!(BarzelReport::load_by_id(dir.path(), &r1.id).unwrap().is_some());
        assert!(BarzelReport::load_by_id(dir.path(), &r2.id).unwrap().is_some());
    }

    #[test]
    fn save_does_not_panic_on_short_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = BarzelReport::new(dummy_project());
        report.id = "ab".to_string(); // shorter than 8 chars
        let path = report.save(dir.path()).unwrap();
        assert!(path.exists());
        // Filename contains the full short id, not a slice
        assert!(path.to_string_lossy().contains("ab"));
    }

    proptest! {
        #[test]
        fn total_findings_equals_sum_of_layer_findings(
            counts in prop::collection::vec(0usize..10usize, 1..5),
        ) {
            let mut report = BarzelReport::new(dummy_project());
            let total: usize = counts.iter().sum();

            for count in &counts {
                let findings = (0..*count).map(|_| finding(Severity::Info)).collect();
                report.add_layer(layer_with_findings(findings));
            }

            prop_assert_eq!(report.summary.total_findings, total);
        }

        #[test]
        fn total_findings_never_decreases(
            count_a in 0usize..10usize,
            count_b in 0usize..10usize,
        ) {
            let mut report = BarzelReport::new(dummy_project());

            let before = report.summary.total_findings;
            let findings_a = (0..count_a).map(|_| finding(Severity::Info)).collect();
            report.add_layer(layer_with_findings(findings_a));
            prop_assert!(report.summary.total_findings >= before);

            let before = report.summary.total_findings;
            let findings_b = (0..count_b).map(|_| finding(Severity::Info)).collect();
            report.add_layer(layer_with_findings(findings_b));
            prop_assert!(report.summary.total_findings >= before);
        }

        #[test]
        fn severity_counts_add_up_to_total(finding_count in 0usize..20usize) {
            let mut report = BarzelReport::new(dummy_project());
            // Use High severity so it counts in the summary
            let findings = (0..finding_count).map(|_| finding(Severity::High)).collect();
            report.add_layer(layer_with_findings(findings));

            let counted = report.summary.critical
                + report.summary.high
                + report.summary.medium
                + report.summary.low;
            // Info findings are not counted in severity buckets but ARE in total
            prop_assert_eq!(counted, report.summary.total_findings);
        }
    }
}

impl std::fmt::Display for ReportStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportStatus::Pass => write!(f, "pass"),
            ReportStatus::Partial => write!(f, "partial"),
            ReportStatus::Fail => write!(f, "fail"),
        }
    }
}

impl std::fmt::Display for LayerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerStatus::Pass => write!(f, "PASS"),
            LayerStatus::Partial => write!(f, "PARTIAL"),
            LayerStatus::Fail => write!(f, "FAIL"),
            LayerStatus::Skipped => write!(f, "SKIPPED"),
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "CRITICAL"),
            Severity::High => write!(f, "HIGH"),
            Severity::Medium => write!(f, "MEDIUM"),
            Severity::Low => write!(f, "LOW"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}
