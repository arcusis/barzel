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
}

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
        }
    }

    pub fn add_layer(&mut self, layer: LayerResult) {
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

    pub fn save(&self, base_dir: &Path) -> crate::error::Result<std::path::PathBuf> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        std::fs::create_dir_all(&reports_dir)?;

        let filename = format!("report-{}.json", self.timestamp.format("%Y%m%d-%H%M%S"));
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

        let mut entries: Vec<_> = std::fs::read_dir(&reports_dir)?
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|x| x == "json")
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        match entries.last() {
            None => Ok(None),
            Some(entry) => {
                let content = std::fs::read_to_string(entry.path())?;
                let report: Self = serde_json::from_str(&content)?;
                Ok(Some(report))
            }
        }
    }

    pub fn load_by_id(base_dir: &Path, id: &str) -> crate::error::Result<Option<Self>> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        if !reports_dir.exists() {
            return Ok(None);
        }

        for entry in std::fs::read_dir(&reports_dir)?.flatten() {
            let path = entry.path();
            if path.extension().map(|x| x == "json").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(report) = serde_json::from_str::<Self>(&content) {
                        if report.id.starts_with(id) {
                            return Ok(Some(report));
                        }
                    }
                }
            }
        }

        Ok(None)
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
