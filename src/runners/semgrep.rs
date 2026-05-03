use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::process::Command;
use std::time::Instant;

pub struct SemgrepRunner;

impl TestRunner for SemgrepRunner {
    fn name(&self) -> &'static str {
        "semgrep"
    }

    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn skip_message(&self) -> &'static str {
        "semgrep not installed — run `pip install semgrep` to enable SAST security scanning"
    }

    fn is_available(&self, _project: &ProjectInfo) -> bool {
        Command::new("semgrep")
            .args(["--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = std::path::Path::new(&project.root);

        let output = Command::new("semgrep")
            .args(["--json", "--quiet", "--config", "auto", "."])
            .current_dir(project_root)
            .output();

        match output {
            Ok(result) => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                let findings = parse_semgrep_json(&output_str);

                let status = if findings
                    .iter()
                    .any(|f| matches!(f.severity, Severity::Critical | Severity::High))
                {
                    LayerStatus::Fail
                } else if !findings.is_empty() {
                    LayerStatus::Partial
                } else {
                    LayerStatus::Pass
                };

                Ok(LayerResult {
                    name: "hostile".to_string(),
                    runner: "semgrep".to_string(),
                    status,
                    findings,
                    metrics: LayerMetrics::default(),
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(e) => Ok(LayerResult {
                name: "hostile".to_string(),
                runner: "semgrep".to_string(),
                status: LayerStatus::Fail,
                findings: vec![Finding {
                    severity: Severity::Critical,
                    code: "SAST_EXECUTION_FAILED".to_string(),
                    message: format!("Failed to run semgrep: {}", e),
                    reproduce_cmd: Some("semgrep --config=auto . 2>&1".to_string()),
                    suggestion: Some("Install semgrep: `pip install semgrep`".to_string()),
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

fn parse_semgrep_json(output: &str) -> Vec<Finding> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(output) else {
        return vec![];
    };

    let Some(results) = json.get("results").and_then(|r| r.as_array()) else {
        return vec![];
    };

    results
        .iter()
        .map(|item| {
            let severity = item
                .get("extra")
                .and_then(|e| e.get("severity"))
                .and_then(|s| s.as_str())
                .map(map_semgrep_severity)
                .unwrap_or(Severity::Medium);

            let message = item
                .get("extra")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Security issue detected")
                .to_string();

            let check_id = item
                .get("check_id")
                .and_then(|c| c.as_str())
                .unwrap_or("SAST_FINDING")
                .to_string();

            let path = item.get("path").and_then(|p| p.as_str());
            let line = item
                .get("start")
                .and_then(|s| s.get("line"))
                .and_then(|l| l.as_u64())
                .unwrap_or(0);

            let location = path.map(|p| format!("{}:{}", p, line));

            let reproduce_cmd = path.map(|p| {
                format!("semgrep --config={} --include {} .", check_id, p)
            });

            Finding {
                severity,
                code: check_id,
                message,
                location,
                reproduce_cmd,
                suggestion: Some(
                    "Review the flagged code. See semgrep.dev for rule details and remediation."
                        .to_string(),
                ),
            }
        })
        .collect()
}

fn map_semgrep_severity(s: &str) -> Severity {
    match s.to_uppercase().as_str() {
        "ERROR" => Severity::Critical,
        "WARNING" => Severity::High,
        "INFO" => Severity::Info,
        _ => Severity::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_json_returns_no_findings() {
        assert!(parse_semgrep_json("").is_empty());
        assert!(parse_semgrep_json("{}").is_empty());
        assert!(parse_semgrep_json(r#"{"results":[]}"#).is_empty());
    }

    #[test]
    fn parses_single_finding() {
        let json = r#"{
            "results": [{
                "check_id": "rules.sqli",
                "path": "src/db.rs",
                "start": {"line": 42, "col": 1},
                "extra": {
                    "severity": "ERROR",
                    "message": "SQL injection risk"
                }
            }]
        }"#;
        let findings = parse_semgrep_json(json);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Critical));
        assert_eq!(findings[0].code, "rules.sqli");
        assert!(findings[0].location.as_deref().unwrap().contains("src/db.rs"));
        assert!(findings[0].location.as_deref().unwrap().contains("42"));
    }

    #[test]
    fn maps_severity_correctly() {
        assert!(matches!(map_semgrep_severity("ERROR"),   Severity::Critical));
        assert!(matches!(map_semgrep_severity("WARNING"), Severity::High));
        assert!(matches!(map_semgrep_severity("INFO"),    Severity::Info));
        assert!(matches!(map_semgrep_severity("unknown"), Severity::Medium));
    }

    #[test]
    fn severity_mapping_is_case_insensitive() {
        assert!(matches!(map_semgrep_severity("error"),   Severity::Critical));
        assert!(matches!(map_semgrep_severity("warning"), Severity::High));
        assert!(matches!(map_semgrep_severity("info"),    Severity::Info));
    }

    proptest! {
        #[test]
        fn parse_semgrep_json_never_panics(s in ".*") {
            let _ = parse_semgrep_json(&s);
        }

        #[test]
        fn map_severity_never_panics(s in ".*") {
            let _ = map_semgrep_severity(&s);
        }
    }
}
