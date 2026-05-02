use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        // Update summary counts
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

        // Determine overall status
        if self.summary.critical > 0 {
            self.status = ReportStatus::Fail;
        } else if self.summary.high > 0 || self.summary.medium > 0 {
            self.status = ReportStatus::Partial;
        }

        self.layers.push(layer);
    }

    pub fn save(&self, base_dir: &std::path::Path) -> crate::error::Result<std::path::PathBuf> {
        let reports_dir = base_dir.join(".barzel").join("reports");
        std::fs::create_dir_all(&reports_dir)?;

        let filename = format!("report-{}.json", self.timestamp.format("%Y%m%d-%H%M%S"));
        let path = reports_dir.join(filename);

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;

        Ok(path)
    }
}
