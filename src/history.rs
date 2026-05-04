/// Local metric-snapshot history for trend/regression detection.
///
/// Each completed `barzel run` that produces at least one coverage or
/// mutation-score measurement appends a compact entry to `.barzel/history/`.
/// Full reports remain in `.barzel/reports/`; history entries are small and
/// fast to scan for future regression warnings.
use crate::report::{BarzelReport, LayerStatus, ReportStatus};
use chrono::{DateTime, Utc};
use std::path::Path;

/// One metric-bearing layer inside a history entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct HistoryLayerMetric {
    pub runner: String,
    pub status: LayerStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
}

/// Snapshot of metric data from one project (or workspace member) for one run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub report_id: String,
    pub timestamp: DateTime<Utc>,
    pub project: String,
    /// Relative path within a workspace, absent for single-project runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_path: Option<String>,
    pub language: String,
    pub status: ReportStatus,
    /// Only layers with non-null mutation_score or coverage.
    pub layers: Vec<HistoryLayerMetric>,
}

/// Build history entries from a completed report.
///
/// For single projects: at most one entry.
/// For workspaces: one entry per member that has metric layers.
/// Entries with no metric layers are excluded.
pub fn entries_from_report(report: &BarzelReport) -> Vec<HistoryEntry> {
    if report.workspace_members.is_empty() {
        // Single-project path
        let layers = metric_layers(&report.layers);
        if layers.is_empty() {
            return vec![];
        }
        vec![HistoryEntry {
            report_id: report.id.clone(),
            timestamp: report.timestamp,
            project: report.project.package_name.clone()
                .unwrap_or_else(|| report.project.language.to_string()),
            package_path: None,
            language: report.project.language.to_string(),
            status: report.status,
            layers,
        }]
    } else {
        // Workspace path: use per-member layers, not aggregate
        report.workspace_members.iter().filter_map(|member| {
            let layers = metric_layers(&member.layers);
            if layers.is_empty() { return None; }
            Some(HistoryEntry {
                report_id: report.id.clone(),
                timestamp: report.timestamp,
                project: report.project.package_name.clone()
                    .unwrap_or_else(|| "workspace".to_string()),
                package_path: Some(member.package_path.clone()),
                language: member.language.clone(),
                status: member.status,
                layers,
            })
        }).collect()
    }
}

/// Extract layers that carry at least one non-null metric.
fn metric_layers(layers: &[crate::report::LayerResult]) -> Vec<HistoryLayerMetric> {
    layers.iter().filter_map(|l| {
        let ms = l.metrics.mutation_score;
        let cov = l.metrics.coverage;
        if ms.is_none() && cov.is_none() { return None; }
        Some(HistoryLayerMetric {
            runner: l.runner.clone(),
            status: l.status,
            mutation_score: ms,
            coverage: cov,
        })
    }).collect()
}

/// Persist a history entry under `{project_root}/.barzel/history/`.
///
/// Filename: `{sanitize(report_id)}-{sanitize(suffix)}-{hash8(raw_suffix)}.json`
/// where suffix is `package_path` (workspace members) or `language` (single project).
/// Using the full sanitized report_id prevents prefix collisions across runs;
/// hash8 of the raw suffix keeps paths that sanitize identically (e.g. "apps/web"
/// vs "apps_web") in separate files.
///
/// Best-effort: errors are returned so callers can log them without failing.
pub fn save_entry(entry: &HistoryEntry, project_root: &Path) -> std::io::Result<()> {
    let history_dir = project_root.join(".barzel").join("history");
    std::fs::create_dir_all(&history_dir)?;

    // Use the full report_id (sanitized) so two IDs sharing the same 8-char prefix
    // never collide. Append a short stable hash of the raw suffix so workspace members
    // that sanitize to the same string (e.g. "apps/web" vs "apps_web") remain distinct.
    let raw_suffix = entry.package_path.as_deref()
        .unwrap_or(&entry.language);
    let safe_suffix = sanitize(raw_suffix);
    let suffix_hash = hash8(raw_suffix);
    let safe_id = sanitize(&entry.report_id);
    let filename = format!("{}-{}-{}.json", safe_id, safe_suffix, suffix_hash);

    let content = serde_json::to_string_pretty(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(history_dir.join(filename), content)
}

/// Persist all metric-bearing entries from a report.
/// Best-effort: a failure is returned as `Err` so the caller can warn on stderr.
pub fn save_from_report(report: &BarzelReport, project_root: &Path) -> std::io::Result<()> {
    for entry in entries_from_report(report) {
        save_entry(&entry, project_root)?;
    }
    Ok(())
}

/// Load all valid history entries under `{project_root}/.barzel/history/`,
/// sorted by timestamp ascending. Invalid JSON files are silently skipped.
// Used by the follow-up trend/regression detection PR; suppression is intentional.
#[allow(dead_code)]
pub fn load_history_entries(project_root: &Path) -> Vec<HistoryEntry> {
    let history_dir = project_root.join(".barzel").join("history");
    if !history_dir.exists() {
        return vec![];
    }

    let mut entries: Vec<HistoryEntry> = std::fs::read_dir(&history_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| {
            let content = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&content).ok()
        })
        .collect();

    entries.sort_by_key(|e| e.timestamp);
    entries
}

/// Replace non-alphanumeric characters (except hyphens) with underscores.
/// Mirrors the helper in cache.rs to keep filenames safe across platforms.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

/// Stable 8-hex-char hash of a string, used as a tiebreaker in filenames
/// when two raw suffixes sanitize to the same safe string.
fn hash8(s: &str) -> String {
    // FNV-1a 64-bit — dependency-free, deterministic, good distribution for short strings.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h & 0xffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo, ProjectFrameworks};
    use crate::report::{
        BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, ReportStatus,
        Severity, Summary, WorkspaceMemberReport,
    };
    use tempfile::tempdir;

    // ── fixtures ──────────────────────────────────────────────────────────────

    fn project(language: Language) -> ProjectInfo {
        ProjectInfo {
            language,
            root: "/tmp/proj".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    fn layer_with_metrics(runner: &str, mutation_score: Option<f64>, coverage: Option<f64>) -> LayerResult {
        LayerResult {
            name: "logic".to_string(),
            runner: runner.to_string(),
            status: LayerStatus::Pass,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "PASS".to_string(),
                message: "ok".to_string(),
                location: None,
                reproduce_cmd: Some("pytest".to_string()),
                suggestion: None,
            }],
            metrics: LayerMetrics { mutation_score, coverage, ..Default::default() },
            duration_ms: 100,
        }
    }

    fn layer_no_metrics(runner: &str) -> LayerResult {
        LayerResult {
            name: "hostile".to_string(),
            runner: runner.to_string(),
            status: LayerStatus::Pass,
            findings: vec![],
            metrics: LayerMetrics::default(),
            duration_ms: 50,
        }
    }

    fn single_report_with_coverage() -> BarzelReport {
        let mut r = BarzelReport::new(project(Language::Python));
        r.add_layer(layer_with_metrics("pytest", None, Some(0.91)));
        r.add_layer(layer_no_metrics("bandit"));
        r
    }

    fn single_report_no_metrics() -> BarzelReport {
        let mut r = BarzelReport::new(project(Language::Rust));
        r.add_layer(layer_no_metrics("semgrep"));
        r.add_layer(layer_no_metrics("cargo audit"));
        r
    }

    fn workspace_report() -> BarzelReport {
        let ws_project = ProjectInfo {
            language: Language::Unknown,
            root: "/tmp/ws".to_string(),
            has_tests: true,
            package_name: Some("my-workspace".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut r = BarzelReport::new(ws_project);

        r.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "crates/api".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Pass,
                layers: vec![layer_with_metrics("cargo mutants", Some(0.84), None)],
                summary: Summary { total_findings: 0, critical: 0, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Pass },
            },
            WorkspaceMemberReport {
                package_path: "apps/web".to_string(),
                language: "typescript".to_string(),
                status: ReportStatus::Partial,
                layers: vec![
                    layer_with_metrics("stryker", Some(0.72), None),
                    layer_no_metrics("eslint"),
                ],
                summary: Summary { total_findings: 1, critical: 0, high: 1, medium: 0, low: 0,
                    overall_status: ReportStatus::Partial },
            },
            WorkspaceMemberReport {
                package_path: "libs/shared".to_string(),
                language: "typescript".to_string(),
                status: ReportStatus::Pass,
                // No metric layers — this member must be excluded
                layers: vec![layer_no_metrics("tsc")],
                summary: Summary { total_findings: 0, critical: 0, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Pass },
            },
        ];
        r
    }

    // ── serialization ─────────────────────────────────────────────────────────

    #[test]
    fn history_entry_round_trips() {
        let entry = HistoryEntry {
            report_id: "abc12345-dead-beef-0000-000000000000".to_string(),
            timestamp: chrono::Utc::now(),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.91),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn workspace_entry_round_trips_with_package_path() {
        let entry = HistoryEntry {
            report_id: "ff112233".to_string(),
            timestamp: chrono::Utc::now(),
            project: "my-workspace".to_string(),
            package_path: Some("crates/api".to_string()),
            language: "rust".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "cargo mutants".to_string(),
                status: LayerStatus::Pass,
                mutation_score: Some(0.84),
                coverage: None,
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry.package_path, back.package_path);
        assert_eq!(back.layers[0].mutation_score, Some(0.84));
    }

    // ── entries_from_report ───────────────────────────────────────────────────

    #[test]
    fn single_project_creates_one_entry_with_coverage() {
        let report = single_report_with_coverage();
        let entries = entries_from_report(&report);

        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.report_id, report.id);
        assert_eq!(e.language, "python");
        assert!(e.package_path.is_none());
        assert_eq!(e.layers.len(), 1, "only the pytest layer has metrics");
        assert_eq!(e.layers[0].runner, "pytest");
        assert_eq!(e.layers[0].coverage, Some(0.91));
        assert!(e.layers[0].mutation_score.is_none());
    }

    #[test]
    fn single_project_with_no_metrics_produces_no_entries() {
        let report = single_report_no_metrics();
        let entries = entries_from_report(&report);
        assert!(entries.is_empty(), "zero-metric report must produce no history entries");
    }

    #[test]
    fn workspace_produces_one_entry_per_metric_bearing_member() {
        let report = workspace_report();
        let entries = entries_from_report(&report);

        assert_eq!(entries.len(), 2, "two of three members have metric layers");

        let api = entries.iter().find(|e| e.package_path.as_deref() == Some("crates/api"))
            .expect("crates/api entry must be present");
        assert_eq!(api.layers[0].runner, "cargo mutants");
        assert_eq!(api.layers[0].mutation_score, Some(0.84));
        assert_eq!(api.language, "rust");

        let web = entries.iter().find(|e| e.package_path.as_deref() == Some("apps/web"))
            .expect("apps/web entry must be present");
        assert_eq!(web.layers.len(), 1, "only stryker has metrics in apps/web");
        assert_eq!(web.layers[0].mutation_score, Some(0.72));
        assert_eq!(web.status, ReportStatus::Partial);

        assert!(entries.iter().all(|e| e.report_id == report.id),
            "all entries must reference the same report_id");
    }

    #[test]
    fn workspace_member_with_no_metrics_is_excluded() {
        let report = workspace_report();
        let entries = entries_from_report(&report);
        assert!(
            entries.iter().all(|e| e.package_path.as_deref() != Some("libs/shared")),
            "libs/shared has no metric layers and must not appear in history"
        );
    }

    #[test]
    fn skipped_report_produces_no_entries() {
        // A diff-skip report has a runner="diff" Info finding and no metrics
        let mut r = BarzelReport::new(project(Language::Rust));
        r.add_layer(LayerResult {
            name: "skipped".to_string(),
            runner: "diff".to_string(),
            status: LayerStatus::Skipped,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "NO_CHANGES_SINCE_REV".to_string(),
                message: "skipped".to_string(),
                location: None,
                reproduce_cmd: Some("git diff".to_string()),
                suggestion: None,
            }],
            metrics: LayerMetrics::default(),
            duration_ms: 0,
        });
        let entries = entries_from_report(&r);
        assert!(entries.is_empty(), "diff-skip report must produce no history entries");
    }

    // ── save / load ───────────────────────────────────────────────────────────

    #[test]
    fn save_entry_writes_under_barzel_history() {
        let dir = tempdir().unwrap();
        let report = single_report_with_coverage();
        let entries = entries_from_report(&report);
        assert_eq!(entries.len(), 1);

        save_entry(&entries[0], dir.path()).unwrap();

        let history_dir = dir.path().join(".barzel").join("history");
        assert!(history_dir.exists(), ".barzel/history/ must be created");
        let files: Vec<_> = std::fs::read_dir(&history_dir).unwrap()
            .flatten().collect();
        assert_eq!(files.len(), 1, "one file per entry");
        let name = files[0].file_name().to_string_lossy().to_string();
        assert!(name.ends_with(".json"), "file must have .json extension");
        assert!(name.contains(&report.id[..8]), "filename must contain report id prefix");
    }

    #[test]
    fn workspace_entries_get_unique_filenames() {
        let dir = tempdir().unwrap();
        let report = workspace_report();
        save_from_report(&report, dir.path()).unwrap();

        let history_dir = dir.path().join(".barzel").join("history");
        let files: Vec<_> = std::fs::read_dir(&history_dir).unwrap()
            .flatten().map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(files.len(), 2, "two metric-bearing members → two files");
        // filenames must be distinct
        let unique: std::collections::HashSet<_> = files.iter().collect();
        assert_eq!(unique.len(), 2, "workspace member filenames must be unique");
    }

    #[test]
    fn load_skips_invalid_json_and_returns_valid_entries() {
        let dir = tempdir().unwrap();
        let history_dir = dir.path().join(".barzel").join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        // Write one valid entry
        let report = single_report_with_coverage();
        let entries = entries_from_report(&report);
        save_entry(&entries[0], dir.path()).unwrap();

        // Write corrupt JSON alongside it
        std::fs::write(history_dir.join("corrupt.json"), b"not json {{").unwrap();

        let loaded = load_history_entries(dir.path());
        assert_eq!(loaded.len(), 1, "corrupt file must be skipped; one valid entry returned");
    }

    #[test]
    fn load_returns_entries_sorted_by_timestamp_ascending() {
        let dir = tempdir().unwrap();

        // Build two entries with deliberate timestamp ordering
        let mut early = HistoryEntry {
            report_id: "aaaa0001".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(2),
            project: "proj".to_string(),
            package_path: None,
            language: "rust".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "cargo mutants".to_string(),
                status: LayerStatus::Pass,
                mutation_score: Some(0.80),
                coverage: None,
            }],
        };
        let mut late = early.clone();
        late.report_id = "bbbb0002".to_string();
        late.timestamp = chrono::Utc::now();
        early.report_id = "aaaa0001".to_string(); // restore after clone

        // Write late first, then early — load must still sort ascending
        let history_dir = dir.path().join(".barzel").join("history");
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(history_dir.join("late.json"),  serde_json::to_string(&late).unwrap()).unwrap();
        std::fs::write(history_dir.join("early.json"), serde_json::to_string(&early).unwrap()).unwrap();

        let loaded = load_history_entries(dir.path());
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].timestamp <= loaded[1].timestamp, "entries must be sorted ascending by timestamp");
        assert_eq!(loaded[0].report_id, "aaaa0001", "earlier entry must be first");
    }

    #[test]
    fn same_id_prefix_different_full_ids_produce_separate_files() {
        // Two report IDs sharing the same first 8 chars must not overwrite each other.
        let dir = tempdir().unwrap();
        let history_dir = dir.path().join(".barzel").join("history");

        let base_layer = HistoryLayerMetric {
            runner: "pytest".to_string(),
            status: LayerStatus::Pass,
            mutation_score: None,
            coverage: Some(0.91),
        };
        let first = HistoryEntry {
            report_id: "abcd1234-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(),
            timestamp: chrono::Utc::now(),
            project: "proj".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![base_layer.clone()],
        };
        let mut second = first.clone();
        // Same 8-char prefix "abcd1234", different full id
        second.report_id = "abcd1234-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string();

        save_entry(&first,  dir.path()).unwrap();
        save_entry(&second, dir.path()).unwrap();

        let files: Vec<_> = std::fs::read_dir(&history_dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 2,
            "two entries with IDs sharing the same 8-char prefix must produce two distinct files");
    }

    #[test]
    fn workspace_members_sanitizing_to_same_suffix_produce_separate_files() {
        // "apps/web" and "apps_web" both sanitize to "apps_web" — they must not collide.
        let dir = tempdir().unwrap();
        let history_dir = dir.path().join(".barzel").join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        let base_entry = HistoryEntry {
            report_id: "rep12345-0000-0000-0000-000000000000".to_string(),
            timestamp: chrono::Utc::now(),
            project: "ws".to_string(),
            package_path: Some("apps/web".to_string()),
            language: "typescript".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "stryker".to_string(),
                status: LayerStatus::Pass,
                mutation_score: Some(0.80),
                coverage: None,
            }],
        };
        let mut slash_entry = base_entry.clone();
        let mut underscore_entry = base_entry.clone();
        slash_entry.package_path = Some("apps/web".to_string());
        underscore_entry.package_path = Some("apps_web".to_string());

        save_entry(&slash_entry, dir.path()).unwrap();
        save_entry(&underscore_entry, dir.path()).unwrap();

        let files: Vec<_> = std::fs::read_dir(&history_dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 2,
            "apps/web and apps_web sanitize to the same string but must produce distinct files");
    }

    #[test]
    fn save_failure_is_returned_as_err_not_panic() {
        // Write to a path where the parent is a file (cannot create dir)
        let dir = tempdir().unwrap();
        let blocker = dir.path().join(".barzel");
        std::fs::write(&blocker, b"I am a file, not a directory").unwrap();

        let report = single_report_with_coverage();
        let entries = entries_from_report(&report);
        let result = save_entry(&entries[0], dir.path());
        assert!(result.is_err(), "save_entry must return Err when history dir cannot be created");
    }
}
