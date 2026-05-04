/// Local metric-snapshot history for trend/regression detection.
///
/// Each completed `barzel run` that produces at least one coverage or
/// mutation-score measurement appends a compact entry to `.barzel/history/`.
/// Full reports remain in `.barzel/reports/`; history entries are small and
/// fast to scan for future regression warnings.
use crate::config::HistoryConfig;
use crate::report::{BarzelReport, Finding, LayerResult, LayerStatus, ReportStatus, Severity};
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

/// Compare the current report against prior history and inject `COVERAGE_REGRESSION`
/// or `MUTATION_SCORE_REGRESSION` findings for any drops that exceed the configured
/// tolerances. Findings are injected directly into the matching `LayerResult`.
///
/// Must be called **before** `save_from_report` so the current run is not compared
/// against itself.
pub fn annotate_metric_regressions(
    report: &mut BarzelReport,
    project_root: &Path,
    cfg: &HistoryConfig,
) {
    if !cfg.enabled { return; }

    let history = load_history_entries(project_root);
    if history.is_empty() { return; }

    let cfg = cfg.normalized();

    let mut any_injected = false;

    if report.workspace_members.is_empty() {
        // Single-project path
        let language = report.project.language.to_string();

        for layer in &mut report.layers {
            let injected = regression_findings_for_layer(
                layer, &history, None, &language, &cfg,
            );
            if !injected.is_empty() {
                layer.findings.extend(injected);
                if matches!(layer.status, LayerStatus::Pass) {
                    layer.status = LayerStatus::Partial;
                }
                any_injected = true;
            }
        }
    } else {
        // Workspace path: inject into member layers, then update member summary/status,
        // then rebuild aggregate layers so they are consistent before emit.
        for member in &mut report.workspace_members {
            let package_path = member.package_path.clone();
            let language = member.language.clone();
            let mut member_injected = false;

            for layer in &mut member.layers {
                let injected = regression_findings_for_layer(
                    layer, &history, Some(&package_path), &language, &cfg,
                );
                if !injected.is_empty() {
                    layer.findings.extend(injected);
                    if matches!(layer.status, LayerStatus::Pass) {
                        layer.status = LayerStatus::Partial;
                    }
                    member_injected = true;
                    any_injected = true;
                }
            }

            // Recompute per-member summary and status after injection.
            if member_injected {
                let mut s = crate::report::Summary {
                    total_findings: 0, critical: 0, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Pass,
                };
                for layer in &member.layers {
                    for f in &layer.findings {
                        s.total_findings += 1;
                        match f.severity {
                            Severity::Critical => s.critical += 1,
                            Severity::High     => s.high += 1,
                            Severity::Medium   => s.medium += 1,
                            Severity::Low      => s.low += 1,
                            Severity::Info     => {}
                        }
                    }
                }
                member.status = if s.critical > 0 {
                    ReportStatus::Fail
                } else if s.high > 0 || s.medium > 0 {
                    ReportStatus::Partial
                } else {
                    ReportStatus::Pass
                };
                s.overall_status = member.status;
                member.summary = s;
            }
        }

        // Rebuild aggregate layers from updated members and recompute aggregate summary.
        if any_injected {
            report.layers = report.workspace_members.iter()
                .flat_map(|m| m.layers.clone())
                .collect();
        }
    }

    if any_injected {
        report.recompute_summary();
    }
}

/// Return all regression findings for `layer` (one per exceeded metric threshold).
/// Returns an empty vec when no regressions are found.
/// Matching key: `(package_path, language, runner)` — `project` is metadata only and
/// is excluded so package-name renames do not break the baseline.
fn regression_findings_for_layer(
    layer: &LayerResult,
    history: &[HistoryEntry],
    package_path: Option<&str>,
    language: &str,
    cfg: &HistoryConfig,
) -> Vec<Finding> {
    let ms = layer.metrics.mutation_score;
    let cov = layer.metrics.coverage;
    if ms.is_none() && cov.is_none() { return vec![]; }

    // History is sorted ascending; scan in reverse so the first match is the most recent.
    // Matching key: (package_path, language, runner) — project name is metadata only
    // and is intentionally excluded so renames do not break the baseline.
    let prior_layer = match history.iter().rev()
        .filter(|e| {
            e.package_path.as_deref() == package_path
                && e.language == language
        })
        .find_map(|e| e.layers.iter().find(|l| l.runner == layer.runner))
    {
        Some(p) => p,
        None => return vec![],
    };

    let mut findings = vec![];
    let reproduce = best_reproduce_cmd(layer, "barzel run --json");

    // Coverage regression
    if let (Some(current), Some(prior)) = (cov, prior_layer.coverage) {
        let drop = prior - current;
        if drop > cfg.coverage_regression_tolerance {
            findings.push(Finding {
                severity: Severity::Medium,
                code: "COVERAGE_REGRESSION".to_string(),
                message: format!(
                    "Coverage dropped from {:.1}% to {:.1}% (−{:.1} pp, runner: {})",
                    prior * 100.0, current * 100.0, drop * 100.0, layer.runner
                ),
                location: None,
                reproduce_cmd: Some(reproduce.clone()),
                suggestion: Some(
                    "Inspect recently changed code for untested branches and rerun the test suite \
                     with coverage reporting enabled."
                    .to_string(),
                ),
            });
        }
    }

    // Mutation score regression
    if let (Some(current), Some(prior)) = (ms, prior_layer.mutation_score) {
        let drop = prior - current;
        if drop > cfg.mutation_regression_tolerance {
            findings.push(Finding {
                severity: Severity::Medium,
                code: "MUTATION_SCORE_REGRESSION".to_string(),
                message: format!(
                    "Mutation score dropped from {:.1}% to {:.1}% (−{:.1} pp, runner: {})",
                    prior * 100.0, current * 100.0, drop * 100.0, layer.runner
                ),
                location: None,
                reproduce_cmd: Some(reproduce),
                suggestion: Some(
                    "Inspect recently changed code for surviving mutants and add targeted tests. \
                     Rerun the mutation runner to confirm improvement."
                    .to_string(),
                ),
            });
        }
    }

    findings
}

/// Return the reproduce_cmd from the first non-regression finding in the layer,
/// falling back to `fallback` if none exists.
fn best_reproduce_cmd(layer: &LayerResult, fallback: &str) -> String {
    layer.findings.iter()
        .filter(|f| !matches!(f.code.as_str(), "COVERAGE_REGRESSION" | "MUTATION_SCORE_REGRESSION"))
        .find_map(|f| f.reproduce_cmd.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// Load all valid history entries under `{project_root}/.barzel/history/`,
/// sorted by timestamp ascending. Invalid JSON files are silently skipped.
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

    // ── annotate_metric_regressions ───────────────────────────────────────────

    fn default_history_cfg() -> HistoryConfig {
        HistoryConfig::default()
    }

    /// Write a prior entry to `dir`'s history directory and return a current
    /// BarzelReport whose layer metric differs from the prior by `delta` (negative = drop).
    fn setup_single_regression(
        dir: &tempfile::TempDir,
        runner: &str,
        prior_coverage: Option<f64>,
        prior_mutation: Option<f64>,
        current_coverage: Option<f64>,
        current_mutation: Option<f64>,
    ) -> BarzelReport {
        let prior = HistoryEntry {
            report_id: "prior001-0000-0000-0000-000000000000".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: runner.to_string(),
                status: LayerStatus::Pass,
                mutation_score: prior_mutation,
                coverage: prior_coverage,
            }],
        };
        save_entry(&prior, dir.path()).unwrap();

        let reproduce = format!("{} --coverage 2>&1", runner);
        let mut report = BarzelReport::new(project(Language::Python));
        report.add_layer(LayerResult {
            name: "logic".to_string(),
            runner: runner.to_string(),
            status: LayerStatus::Pass,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "PASS".to_string(),
                message: "ok".to_string(),
                location: None,
                reproduce_cmd: Some(reproduce),
                suggestion: None,
            }],
            metrics: LayerMetrics {
                coverage: current_coverage,
                mutation_score: current_mutation,
                ..Default::default()
            },
            duration_ms: 0,
        });
        report
    }

    #[test]
    fn negative_tolerance_does_not_flag_improvement_as_regression() {
        // A negative tolerance must be clamped to 0.0, so an improvement is never flagged.
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.80), None, // prior
            Some(0.90), None, // current — improvement
        );
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: -0.05,
            mutation_regression_tolerance: 0.0,
        };
        annotate_metric_regressions(&mut report, dir.path(), &cfg);
        assert!(!report.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "negative tolerance must be clamped to 0.0; an improvement must not trigger a finding");
    }

    #[test]
    fn tolerance_above_one_suppresses_all_findings() {
        // tolerance > 1.0 clamped to 1.0; real drops can never exceed 1.0, so no finding.
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.90), None,
            Some(0.50), None, // 40 pp drop — large but < 100 pp
        );
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: 1.5,
            mutation_regression_tolerance: 0.0,
        };
        annotate_metric_regressions(&mut report, dir.path(), &cfg);
        assert!(!report.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "tolerance clamped to 1.0 must suppress all findings since drops never exceed 1.0");
    }

    #[test]
    fn exact_equality_at_tolerance_does_not_emit_finding() {
        // `drop > tolerance` is strict; equality must not emit a finding.
        // Because f64 subtraction is not exact, we derive tolerance from the same
        // subtraction the code uses: `tolerance = prior - current`. That guarantees
        // the comparison is `x > x` (false) regardless of floating-point rounding.
        let prior = 0.90_f64;
        let current = 0.85_f64;
        let exact_drop = prior - current; // whatever f64 computes for this subtraction

        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(prior), None,
            Some(current), None,
        );
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: exact_drop, // set tolerance == drop exactly
            mutation_regression_tolerance: 0.0,
        };
        annotate_metric_regressions(&mut report, dir.path(), &cfg);
        assert!(!report.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "drop exactly equal to tolerance must not emit a finding (strict > comparison)");
    }

    #[test]
    fn project_name_change_does_not_break_baseline() {
        // Matching key is (package_path, language, runner); project is metadata only.
        // A prior entry with project="old-name" must still match a current run
        // whose package_name changed to "new-name".
        let dir = tempdir().unwrap();
        let prior = HistoryEntry {
            report_id: "oldname01".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            project: "old-name".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.90),
            }],
        };
        save_entry(&prior, dir.path()).unwrap();

        let mut new_project = project(Language::Python);
        new_project.package_name = Some("new-name".to_string());
        let mut report = BarzelReport::new(new_project);
        report.add_layer(LayerResult {
            name: "logic".to_string(),
            runner: "pytest".to_string(),
            status: LayerStatus::Pass,
            findings: vec![Finding {
                severity: Severity::Info,
                code: "PASS".to_string(),
                message: "ok".to_string(),
                location: None,
                reproduce_cmd: Some("pytest".to_string()),
                suggestion: None,
            }],
            metrics: LayerMetrics { coverage: Some(0.75), ..Default::default() },
            duration_ms: 0,
        });

        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        assert!(report.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "project name change must not prevent regression detection — key is (package_path, language, runner)");
    }

    #[test]
    fn no_prior_history_produces_no_regression_findings() {
        let dir = tempdir().unwrap();
        let mut report = single_report_with_coverage();
        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());
        let codes: Vec<_> = report.layers.iter()
            .flat_map(|l| &l.findings)
            .map(|f| f.code.as_str())
            .collect();
        assert!(!codes.contains(&"COVERAGE_REGRESSION"));
        assert!(!codes.contains(&"MUTATION_SCORE_REGRESSION"));
    }

    #[test]
    fn coverage_drop_injects_finding_with_reproduce_cmd() {
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.90), None,  // prior
            Some(0.80), None,  // current — 10 pp drop
        );
        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        let finding = report.layers.iter()
            .flat_map(|l| &l.findings)
            .find(|f| f.code == "COVERAGE_REGRESSION")
            .expect("COVERAGE_REGRESSION finding must be injected");
        assert_eq!(finding.severity, Severity::Medium);
        assert!(finding.message.contains("90.0%"), "message must show prior value");
        assert!(finding.message.contains("80.0%"), "message must show current value");
        assert!(finding.reproduce_cmd.as_deref().unwrap_or("").contains("pytest"),
            "reproduce_cmd must come from the layer's existing finding");
    }

    #[test]
    fn mutation_score_drop_injects_finding_with_reproduce_cmd() {
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "cargo mutants",
            None, Some(0.85),  // prior
            None, Some(0.70),  // current — 15 pp drop
        );
        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        let finding = report.layers.iter()
            .flat_map(|l| &l.findings)
            .find(|f| f.code == "MUTATION_SCORE_REGRESSION")
            .expect("MUTATION_SCORE_REGRESSION finding must be injected");
        assert_eq!(finding.severity, Severity::Medium);
        assert!(finding.message.contains("85.0%"));
        assert!(finding.message.contains("70.0%"));
        assert!(!finding.reproduce_cmd.as_deref().unwrap_or("").is_empty(),
            "reproduce_cmd must not be empty");
    }

    #[test]
    fn equal_or_improved_metrics_produce_no_finding() {
        let dir = tempdir().unwrap();
        // Prior 0.80, current 0.85 — improvement, no finding
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.80), None,
            Some(0.85), None,
        );
        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());
        assert!(!report.layers.iter()
            .flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"));
    }

    #[test]
    fn tolerance_suppresses_small_drops() {
        let dir = tempdir().unwrap();
        // 1 pp drop, but tolerance is 2 pp — no finding
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.90), None,
            Some(0.89), None,
        );
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: 0.02,
            mutation_regression_tolerance: 0.0,
        };
        annotate_metric_regressions(&mut report, dir.path(), &cfg);
        assert!(!report.layers.iter()
            .flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"));
    }

    #[test]
    fn disabled_history_suppresses_regression_findings() {
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.90), None,
            Some(0.80), None,
        );
        let cfg = HistoryConfig { enabled: false, ..HistoryConfig::default() };
        annotate_metric_regressions(&mut report, dir.path(), &cfg);
        assert!(!report.layers.iter()
            .flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "regression findings must be suppressed when history.enabled = false");
    }

    #[test]
    fn latest_prior_entry_wins_when_multiple_exist() {
        let dir = tempdir().unwrap();
        let runner = "pytest";

        // older entry: coverage was 0.90
        let old_entry = HistoryEntry {
            report_id: "old00001".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(5),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: runner.to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.90),
            }],
        };
        // newest entry: coverage was 0.75 — current (0.80) is better, no regression vs latest
        let new_entry = HistoryEntry {
            report_id: "new00001".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::minutes(10),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Partial,
            layers: vec![HistoryLayerMetric {
                runner: runner.to_string(),
                status: LayerStatus::Partial,
                mutation_score: None,
                coverage: Some(0.75),
            }],
        };
        save_entry(&old_entry, dir.path()).unwrap();
        save_entry(&new_entry, dir.path()).unwrap();

        let mut report = setup_single_regression(
            &dir, runner,
            None, None,         // prior ignored — we wrote manually above
            Some(0.80), None,   // current 80% — above the latest prior of 75%
        );
        // Clear the auto-written prior (setup_single_regression writes its own)
        // by recreating the history dir with only our two entries.
        let history_dir = dir.path().join(".barzel").join("history");
        for f in std::fs::read_dir(&history_dir).unwrap().flatten() {
            std::fs::remove_file(f.path()).unwrap();
        }
        save_entry(&old_entry, dir.path()).unwrap();
        save_entry(&new_entry, dir.path()).unwrap();

        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());
        assert!(!report.layers.iter()
            .flat_map(|l| &l.findings)
            .any(|f| f.code == "COVERAGE_REGRESSION"),
            "latest prior is 75%, current is 80% — no regression vs latest prior");
    }

    #[test]
    fn workspace_compares_only_matching_package_and_runner() {
        let dir = tempdir().unwrap();

        // Prior for crates/api
        let api_prior = HistoryEntry {
            report_id: "apiprior-0000-0000-0000-000000000000".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            project: "my-workspace".to_string(),
            package_path: Some("crates/api".to_string()),
            language: "rust".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "cargo mutants".to_string(),
                status: LayerStatus::Pass,
                mutation_score: Some(0.85),
                coverage: None,
            }],
        };
        save_entry(&api_prior, dir.path()).unwrap();

        // Build workspace report: api dropped, web unchanged (no prior)
        let ws_project = ProjectInfo {
            language: Language::Unknown,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("my-workspace".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(ws_project);
        report.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "crates/api".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Pass,
                layers: vec![LayerResult {
                    name: "structural".to_string(),
                    runner: "cargo mutants".to_string(),
                    status: LayerStatus::Pass,
                    findings: vec![Finding {
                        severity: Severity::Info,
                        code: "PASS".to_string(),
                        message: "ok".to_string(),
                        location: None,
                        reproduce_cmd: Some("cargo mutants 2>&1".to_string()),
                        suggestion: None,
                    }],
                    metrics: LayerMetrics { mutation_score: Some(0.70), ..Default::default() },
                    duration_ms: 0,
                }],
                summary: Summary { total_findings: 0, critical: 0, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Pass },
            },
            WorkspaceMemberReport {
                package_path: "apps/web".to_string(),
                language: "typescript".to_string(),
                status: ReportStatus::Pass,
                layers: vec![LayerResult {
                    name: "structural".to_string(),
                    runner: "stryker".to_string(),
                    status: LayerStatus::Pass,
                    findings: vec![],
                    metrics: LayerMetrics { mutation_score: Some(0.80), ..Default::default() },
                    duration_ms: 0,
                }],
                summary: Summary { total_findings: 0, critical: 0, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Pass },
            },
        ];

        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        // api must have a regression finding (0.85 → 0.70)
        let api = report.workspace_members.iter()
            .find(|m| m.package_path == "crates/api").unwrap();
        assert!(api.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "MUTATION_SCORE_REGRESSION"),
            "crates/api dropped 15 pp — must have MUTATION_SCORE_REGRESSION");
        assert!(matches!(api.status, ReportStatus::Partial),
            "crates/api member.status must be Partial after regression injection");
        assert_eq!(api.summary.medium, 1,
            "crates/api member.summary.medium must be 1 after regression injection");

        // web has no prior — must not have a regression finding
        let web = report.workspace_members.iter()
            .find(|m| m.package_path == "apps/web").unwrap();
        assert!(!web.layers.iter().flat_map(|l| &l.findings)
            .any(|f| f.code == "MUTATION_SCORE_REGRESSION"),
            "apps/web has no prior history entry — must not have regression finding");
        assert!(matches!(web.status, ReportStatus::Pass),
            "apps/web member.status must remain Pass");

        // Aggregate report summary must be recomputed
        assert!(matches!(report.status, ReportStatus::Partial),
            "aggregate report status must be Partial after workspace member regression");
    }

    #[test]
    fn regression_finding_reproduce_cmd_falls_back_to_barzel_run() {
        let dir = tempdir().unwrap();
        // Layer with no existing reproduce_cmd on its findings
        let prior = HistoryEntry {
            report_id: "prior002".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.90),
            }],
        };
        save_entry(&prior, dir.path()).unwrap();

        let mut report = BarzelReport::new(project(Language::Python));
        report.add_layer(LayerResult {
            name: "logic".to_string(),
            runner: "pytest".to_string(),
            status: LayerStatus::Pass,
            findings: vec![],  // no existing findings → no reproduce_cmd to borrow
            metrics: LayerMetrics { coverage: Some(0.80), ..Default::default() },
            duration_ms: 0,
        });

        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        let finding = report.layers.iter()
            .flat_map(|l| &l.findings)
            .find(|f| f.code == "COVERAGE_REGRESSION")
            .expect("COVERAGE_REGRESSION must be injected");
        let rc = finding.reproduce_cmd.as_deref().unwrap_or("");
        assert!(!rc.trim().is_empty(), "reproduce_cmd must not be empty");
        assert!(rc.contains("barzel"), "fallback reproduce_cmd must reference barzel run");
    }

    #[test]
    fn both_coverage_and_mutation_regressions_emit_separate_findings() {
        // A layer that tracks both metrics and drops on both must emit two findings.
        let dir = tempdir().unwrap();
        let prior = HistoryEntry {
            report_id: "prior003".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            project: "myapp".to_string(),
            package_path: None,
            language: "python".to_string(),
            status: ReportStatus::Pass,
            layers: vec![HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: Some(0.85),
                coverage: Some(0.90),
            }],
        };
        save_entry(&prior, dir.path()).unwrap();

        let mut report = BarzelReport::new(project(Language::Python));
        report.add_layer(LayerResult {
            name: "logic".to_string(),
            runner: "pytest".to_string(),
            status: LayerStatus::Pass,
            findings: vec![],
            metrics: LayerMetrics {
                mutation_score: Some(0.70),  // dropped 15 pp
                coverage: Some(0.75),        // dropped 15 pp
                ..Default::default()
            },
            duration_ms: 0,
        });

        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        let codes: Vec<_> = report.layers.iter()
            .flat_map(|l| &l.findings)
            .map(|f| f.code.as_str())
            .collect();
        assert!(codes.contains(&"COVERAGE_REGRESSION"),     "must emit COVERAGE_REGRESSION");
        assert!(codes.contains(&"MUTATION_SCORE_REGRESSION"), "must emit MUTATION_SCORE_REGRESSION");
    }

    #[test]
    fn regression_findings_appear_in_action_items_and_summary_is_coherent() {
        let dir = tempdir().unwrap();
        let mut report = setup_single_regression(
            &dir, "pytest",
            Some(0.90), None,
            Some(0.70), None,
        );
        annotate_metric_regressions(&mut report, dir.path(), &default_history_cfg());

        // summary must be recomputed
        let medium_count = report.layers.iter()
            .flat_map(|l| &l.findings)
            .filter(|f| matches!(f.severity, Severity::Medium))
            .count();
        assert!(medium_count >= 1);
        assert_eq!(report.summary.medium, medium_count,
            "summary.medium must match actual medium finding count after recompute");
        assert!(matches!(report.status, ReportStatus::Partial),
            "a previously passing report with a medium finding must become Partial");
    }
}
