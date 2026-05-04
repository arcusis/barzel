/// Agent-facing stdio JSON protocol: request/response types, payload builders,
/// progress emission, and the `handle_stdio` dispatcher.
///
/// All JSON shapes emitted from this module are part of the public agent contract.
/// Do not change field names or nesting without a corresponding README/version update.
use chrono::Utc;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;
use uuid::Uuid;

use crate::report::{BarzelReport, ReportStatus, Severity};

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct StdioRequest {
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) project_path: Option<String>,
    #[serde(default)]
    pub(crate) layers: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) no_cache: bool,
    #[serde(default)]
    pub(crate) fail_fast: bool,
    #[serde(default)]
    pub(crate) request_id: Option<String>,
    #[serde(default)]
    pub(crate) since: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) compare: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) package_path: Option<String>,
    #[serde(default)]
    pub(crate) language: Option<String>,
    /// Overwrite existing .barzel.toml when running the `init` command.
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(serde::Serialize)]
pub(crate) struct StdioResponse {
    pub(crate) status: String,
    pub(crate) request_id: String,
    pub(crate) timestamp: String,
    pub(crate) version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

pub(crate) fn create_response(
    status: &str,
    request_id: Option<String>,
    data: Option<serde_json::Value>,
    error: Option<String>,
) -> StdioResponse {
    StdioResponse {
        status: status.to_string(),
        request_id: request_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        timestamp: Utc::now().to_rfc3339(),
        version: "1".to_string(),
        data,
        error,
    }
}

pub(crate) fn emit_error(request_id: Option<String>, message: String) {
    let resp = create_response("error", request_id, None, Some(message));
    println!("{}", serde_json::to_string(&resp).unwrap());
}

/// Emit a single newline-delimited progress JSON line to stdout.
/// Must be called before the final success/error response.
fn emit_progress(request_id: &Option<String>, data: serde_json::Value) {
    let line = serde_json::json!({
        "status": "progress",
        "request_id": request_id.as_deref().unwrap_or(""),
        "timestamp": Utc::now().to_rfc3339(),
        "version": "1",
        "data": data,
    });
    println!("{}", serde_json::to_string(&line).unwrap());
}

fn progress_event_data(event: crate::orchestrator::RunnerEvent<'_>, package_path: Option<&str>) -> serde_json::Value {
    use crate::orchestrator::RunnerEvent;
    match event {
        RunnerEvent::Started { runner, layer } => serde_json::json!({
            "event": "runner_started",
            "runner": runner,
            "layer": layer,
            "package_path": package_path,
            "runner_status": null,
            "duration_ms": null,
        }),
        RunnerEvent::Completed { runner, layer, status, duration_ms } => serde_json::json!({
            "event": "runner_completed",
            "runner": runner,
            "layer": layer,
            "package_path": package_path,
            "runner_status": status,
            "duration_ms": duration_ms,
        }),
    }
}

// ── Payload builders ──────────────────────────────────────────────────────────

fn severity_priority(s: &Severity) -> u8 {
    match s {
        Severity::Critical => 1,
        Severity::High => 2,
        Severity::Medium => 3,
        Severity::Low => 4,
        Severity::Info => 5,
    }
}

/// Build the structured data payload an AI agent receives after `run`.
pub(crate) fn build_run_data(report: &BarzelReport) -> serde_json::Value {
    let passed = matches!(report.status, ReportStatus::Pass);
    let is_workspace = !report.workspace_members.is_empty();

    let mut action_items: Vec<serde_json::Value> = if is_workspace {
        report.workspace_members.iter().flat_map(|member| {
            member.layers.iter().flat_map(move |layer| {
                layer.findings.iter().filter_map(move |f| {
                    if matches!(f.severity, Severity::Info) { return None; }
                    Some(serde_json::json!({
                        "priority":     severity_priority(&f.severity),
                        "package_path": member.package_path,
                        "layer":        layer.name,
                        "runner":       layer.runner,
                        "severity":     f.severity,
                        "code":         f.code,
                        "message":      f.message,
                        "location":     f.location,
                        "reproduce_cmd": f.reproduce_cmd,
                        "suggestion":   f.suggestion,
                    }))
                })
            })
        }).collect()
    } else {
        report.layers.iter().flat_map(|layer| {
            layer.findings.iter().filter_map(move |f| {
                if matches!(f.severity, Severity::Info) { return None; }
                Some(serde_json::json!({
                    "priority": severity_priority(&f.severity),
                    "layer":    layer.name,
                    "runner":   layer.runner,
                    "severity": f.severity,
                    "code":     f.code,
                    "message":  f.message,
                    "location": f.location,
                    "reproduce_cmd": f.reproduce_cmd,
                    "suggestion":    f.suggestion,
                }))
            })
        }).collect()
    };
    action_items.sort_by_key(|v| v["priority"].as_u64().unwrap_or(99));

    let layers_json = |layers: &[crate::report::LayerResult]| -> Vec<serde_json::Value> {
        layers.iter().map(|l| {
            let findings: Vec<serde_json::Value> = l.findings.iter().map(|f| serde_json::json!({
                "severity":     f.severity,
                "code":         f.code,
                "message":      f.message,
                "location":     f.location,
                "reproduce_cmd": f.reproduce_cmd,
                "suggestion":   f.suggestion,
            })).collect();
            serde_json::json!({
                "name":     l.name,
                "runner":   l.runner,
                "status":   l.status,
                "tests_run": l.metrics.tests_run,
                "passed":    l.metrics.passed,
                "failed":    l.metrics.failed,
                "mutation_score": l.metrics.mutation_score,
                "duration_ms": l.duration_ms,
                "findings": findings,
            })
        }).collect()
    };

    let workspace_json: Vec<serde_json::Value> = report.workspace_members.iter().map(|m| {
        serde_json::json!({
            "package_path":    m.package_path,
            "language":        m.language,
            "status":          m.status,
            "summary": {
                "total_findings": m.summary.total_findings,
                "critical":       m.summary.critical,
                "high":           m.summary.high,
                "medium":         m.summary.medium,
                "low":            m.summary.low,
                "overall_status": m.summary.overall_status,
            },
            "layers": layers_json(&m.layers),
        })
    }).collect();

    let mut payload = serde_json::json!({
        "passed":          passed,
        "overall_status":  report.status,
        "total_findings":  report.summary.total_findings,
        "critical":        report.summary.critical,
        "high":            report.summary.high,
        "medium":          report.summary.medium,
        "low":             report.summary.low,
        "layers":          layers_json(&report.layers),
        "action_items":    action_items,
        "report_id":       report.id,
        "timestamp":       report.timestamp,
    });

    if is_workspace {
        payload["is_workspace"] = serde_json::Value::Bool(true);
        payload["packages"] = serde_json::Value::Array(workspace_json);
    }

    if report.diff_since.is_some() {
        let active = report.diff_changed_files.is_some();
        payload["diff_mode"] = serde_json::json!({
            "active":        active,
            "since":         report.diff_since,
            "changed_files": report.diff_changed_files,
            "fallback_reason": report.diff_fallback_reason,
        });
    }

    payload
}

/// Build the `data` payload for a stdio `history` command.
pub(crate) fn build_stdio_history_payload(
    target: &str,
    limit: Option<usize>,
    package_path: Option<&str>,
    language: Option<&str>,
) -> serde_json::Value {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 200;

    let effective_limit = match limit {
        None => DEFAULT_LIMIT,
        Some(n) => n.min(MAX_LIMIT),
    };

    let mut all: Vec<crate::history::HistoryEntry> =
        crate::history::load_history_entries(std::path::Path::new(target));
    all.reverse();

    let filtered: Vec<_> = all
        .into_iter()
        .filter(|e| {
            package_path
                .map(|p| e.package_path.as_deref() == Some(p))
                .unwrap_or(true)
                && language.map(|l| e.language == l).unwrap_or(true)
        })
        .collect();

    let total = filtered.len();
    let entries: Vec<_> = filtered.into_iter().take(effective_limit).collect();
    let returned = entries.len();

    serde_json::json!({
        "entries":       entries,
        "returned":      returned,
        "total":         total,
        "limit":         effective_limit,
    })
}

/// Build the `data` payload for a stdio `report` command.
pub(crate) fn build_stdio_report_payload(
    target: &str,
    id: Option<&str>,
    compare: Option<&[String]>,
) -> crate::error::Result<serde_json::Value> {
    let base = std::path::Path::new(target);

    if let Some(refs) = compare {
        if refs.len() != 2 {
            return Err(crate::error::BarzelError::Detection(
                format!("compare requires exactly 2 report refs, got {}", refs.len())
            ));
        }
        let baseline = crate::load_report_by_ref(base, &refs[0])?;
        let head     = crate::load_report_by_ref(base, &refs[1])?;
        let cmp = crate::compare::compare_reports(&baseline, &head);
        return Ok(serde_json::json!({ "comparison": cmp }));
    }

    let id_ref = id.unwrap_or("latest");
    let report = crate::load_report_by_ref(base, id_ref)?;
    let report_json = serde_json::to_value(&report)
        .map_err(|e| crate::error::BarzelError::Detection(e.to_string()))?;
    Ok(serde_json::json!({ "report": report_json }))
}

// ── Command dispatcher ────────────────────────────────────────────────────────

pub(crate) fn handle_stdio() -> ExitCode {
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        emit_error(None, format!("failed to read stdin: {}", e));
        return ExitCode::from(1);
    }

    let req: StdioRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            emit_error(None, format!("invalid JSON request: {}", e));
            return ExitCode::from(1);
        }
    };

    let request_id = Some(req.request_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()));

    match req.command.as_str() {
        "init" => {
            let path = req.project_path.as_deref().map(Path::new);
            let target = path.unwrap_or_else(|| Path::new("."));
            match crate::init::run_init(Some(target), true, req.force) {
                Ok(outcome) => {
                    use crate::init::InitOutcome;
                    let config_status = match outcome {
                        InitOutcome::Created    => "created",
                        InitOutcome::Skipped    => "skipped",
                        InitOutcome::Overwritten => "overwritten",
                    };
                    let project = crate::detect::detect_project(target).ok();
                    let resp = create_response(
                        "success",
                        request_id,
                        Some(serde_json::json!({
                            "message": "project initialized",
                            "config_file": ".barzel.toml",
                            "config_status": config_status,
                            "language": project.as_ref().map(|p| p.language.to_string()),
                            "frameworks": project.as_ref().map(|p| serde_json::json!({
                                "is_nextjs": p.frameworks.is_nextjs,
                                "has_ai_deps": p.frameworks.has_ai_deps,
                                "ai_frameworks": p.frameworks.ai_frameworks,
                            })),
                            "config_schema": {
                                "layers.enabled": "list of layers to run: logic, structural, hostile, operational",
                                "layers.structural.mutation_testing": "bool — enable mutation testing",
                                "layers.structural.mutation_threshold": "float 0-100 — minimum mutation score to pass",
                                "reporting.fail_on": "minimum severity to fail: critical, high, medium, low, any",
                            },
                            "next_steps": [
                                "Run `barzel run` to verify your project",
                                "Use `{\"command\":\"run\"}` via --stdio for agent-mode output",
                            ]
                        })),
                        None,
                    );
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "run" => {
            let path = req.project_path.as_deref().map(Path::new);
            let since = req.since.as_deref();
            let progress_id = request_id.clone();
            let on_event = move |event: crate::orchestrator::RunnerEvent<'_>, package_path: Option<&str>| {
                emit_progress(&progress_id, progress_event_data(event, package_path));
            };
            match crate::run::run_verification_stdio_with_progress(
                path,
                req.layers,
                req.no_cache,
                req.fail_fast,
                since,
                &on_event,
            ) {
                Ok(report) => {
                    let exit_code = crate::report_exit_code(&report);
                    let data = build_run_data(&report);
                    let resp = create_response("success", request_id.clone(), Some(data), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    exit_code
                }
                Err(e) => {
                    emit_error(request_id.clone(), e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "check" => {
            let path = req.project_path.as_deref().map(Path::new);
            let target = path.unwrap_or_else(|| std::path::Path::new("."));
            match crate::detect::detect_workspace(target) {
                Ok(workspace) => {
                    use crate::process::OsProcessRunner;
                    let cfg = match crate::config::BarzelConfig::try_load_for_project(target) {
                        Ok(c) => c,
                        Err(e) => { emit_error(request_id, e.to_string()); return ExitCode::from(1); }
                    };
                    if let Err(e) = cfg.validate() {
                        emit_error(request_id, e.to_string());
                        return ExitCode::from(1);
                    }
                    if let Err(e) = crate::validate_workspace_member_configs(&workspace) {
                        emit_error(request_id, e.to_string());
                        return ExitCode::from(1);
                    }
                    let payload = crate::build_check_payload(workspace, &cfg, &OsProcessRunner);
                    let resp = create_response("success", request_id, Some(payload), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "report" => {
            let target = req.project_path.as_deref().unwrap_or(".");
            match build_stdio_report_payload(target, req.id.as_deref(), req.compare.as_deref()) {
                Ok(payload) => {
                    let resp = create_response("success", request_id, Some(payload), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    emit_error(request_id, e.to_string());
                    ExitCode::from(1)
                }
            }
        }

        "history" => {
            let target = req.project_path.as_deref().unwrap_or(".");
            let payload = build_stdio_history_payload(
                target,
                req.limit,
                req.package_path.as_deref(),
                req.language.as_deref(),
            );
            let resp = create_response("success", request_id, Some(payload), None);
            println!("{}", serde_json::to_string(&resp).unwrap());
            ExitCode::SUCCESS
        }

        other => {
            emit_error(request_id, format!("unknown command: '{}'. Valid commands: init, run, check, report, history", other));
            ExitCode::from(1)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::report::{
        BarzelReport, Finding, LayerMetrics, LayerResult, LayerStatus, ReportStatus, Severity,
        Summary, WorkspaceMemberReport,
    };

    fn make_finding(severity: Severity, code: &str) -> Finding {
        Finding {
            severity,
            code: code.to_string(),
            message: format!("{} issue", code),
            location: Some("src/app.py:10".to_string()),
            reproduce_cmd: Some(format!("grep -n {} src/app.py", code)),
            suggestion: None,
        }
    }

    fn make_layer(name: &str, runner: &str, findings: Vec<Finding>) -> LayerResult {
        let status = if findings.iter().any(|f| matches!(f.severity, Severity::Critical)) {
            LayerStatus::Fail
        } else {
            LayerStatus::Pass
        };
        LayerResult {
            name: name.to_string(),
            runner: runner.to_string(),
            status,
            findings,
            metrics: LayerMetrics::default(),
            duration_ms: 0,
        }
    }

    #[test]
    fn progress_started_payload_includes_agent_contract_fields() {
        let payload = progress_event_data(crate::orchestrator::RunnerEvent::Started {
            runner: "tsc",
            layer: "logic",
        }, None);

        assert_eq!(payload["event"], "runner_started");
        assert_eq!(payload["runner"], "tsc");
        assert_eq!(payload["layer"], "logic");
        assert!(payload["package_path"].is_null());
        assert!(payload["runner_status"].is_null());
        assert!(payload["duration_ms"].is_null());
    }

    #[test]
    fn progress_completed_payload_includes_status_and_duration() {
        let payload = progress_event_data(crate::orchestrator::RunnerEvent::Completed {
            runner: "tsc",
            layer: "logic",
            status: "pass",
            duration_ms: 42,
        }, None);

        assert_eq!(payload["event"], "runner_completed");
        assert_eq!(payload["runner"], "tsc");
        assert_eq!(payload["layer"], "logic");
        assert!(payload["package_path"].is_null());
        assert_eq!(payload["runner_status"], "pass");
        assert_eq!(payload["duration_ms"], 42);
    }

    #[test]
    fn progress_workspace_member_payload_includes_package_path() {
        let payload = progress_event_data(crate::orchestrator::RunnerEvent::Started {
            runner: "cargo-mutants",
            layer: "structural",
        }, Some("crates/api"));

        assert_eq!(payload["event"], "runner_started");
        assert_eq!(payload["runner"], "cargo-mutants");
        assert_eq!(payload["package_path"], "crates/api");

        let completed = progress_event_data(crate::orchestrator::RunnerEvent::Completed {
            runner: "cargo-mutants",
            layer: "structural",
            status: "pass",
            duration_ms: 99,
        }, Some("crates/api"));

        assert_eq!(completed["event"], "runner_completed");
        assert_eq!(completed["package_path"], "crates/api");
        assert_eq!(completed["runner_status"], "pass");
    }

    #[test]
    fn progress_response_reuses_supplied_request_id() {
        let payload = progress_event_data(crate::orchestrator::RunnerEvent::Started {
            runner: "pytest",
            layer: "logic",
        }, None);
        let response = create_response("progress", Some("req-123".to_string()), Some(payload), None);

        assert_eq!(response.status, "progress");
        assert_eq!(response.request_id, "req-123");
        assert_eq!(response.version, "1");
        assert!(response.error.is_none());
    }

    #[test]
    fn emit_progress_produces_valid_json_envelope_with_required_fields() {
        let request_id = Some("req-abc".to_string());
        let data = progress_event_data(crate::orchestrator::RunnerEvent::Completed {
            runner: "semgrep",
            layer: "hostile",
            status: "pass",
            duration_ms: 123,
        }, None);

        let envelope = serde_json::json!({
            "status": "progress",
            "request_id": request_id.as_deref().unwrap_or(""),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "version": "1",
            "data": data,
        });

        let line = serde_json::to_string(&envelope).expect("progress envelope must serialize");
        let parsed: serde_json::Value = serde_json::from_str(&line)
            .expect("progress line must be valid JSON");

        assert_eq!(parsed["status"], "progress");
        assert_eq!(parsed["version"], "1");
        assert_eq!(parsed["request_id"], "req-abc");
        assert!(parsed["timestamp"].is_string(), "timestamp must be a string");
        assert_eq!(parsed["data"]["event"], "runner_completed");
        assert_eq!(parsed["data"]["runner"], "semgrep");
        assert_eq!(parsed["data"]["layer"], "hostile");
        assert_eq!(parsed["data"]["runner_status"], "pass");
        assert_eq!(parsed["data"]["duration_ms"], 123);
    }

    fn workspace_report() -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Unknown,
            root: "/tmp/ws".to_string(),
            has_tests: true,
            package_name: Some("cargo-workspace".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.status = ReportStatus::Fail;

        let pkg_a_layer = make_layer("hostile", "ai-sec", vec![
            make_finding(Severity::Critical, "HARDCODED_API_KEY"),
        ]);
        let pkg_b_layer = make_layer("logic", "pytest", vec![
            make_finding(Severity::High, "TEST_FAILURE"),
        ]);

        report.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "crates/a".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Fail,
                layers: vec![pkg_a_layer.clone()],
                summary: Summary {
                    total_findings: 1, critical: 1, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Fail,
                },
            },
            WorkspaceMemberReport {
                package_path: "crates/b".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Partial,
                layers: vec![pkg_b_layer.clone()],
                summary: Summary {
                    total_findings: 1, critical: 0, high: 1, medium: 0, low: 0,
                    overall_status: ReportStatus::Partial,
                },
            },
        ];
        report.layers = vec![pkg_a_layer, pkg_b_layer];
        report
    }

    #[test]
    fn workspace_action_items_include_package_path() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let items = payload["action_items"].as_array().unwrap();
        assert!(!items.is_empty(), "action_items must not be empty for workspace with findings");
        for item in items {
            assert!(item.get("package_path").is_some(), "each action_item must have package_path");
            assert!(!item["package_path"].as_str().unwrap_or("").is_empty());
        }
    }

    #[test]
    fn workspace_action_items_are_priority_sorted() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let items = payload["action_items"].as_array().unwrap();
        let priorities: Vec<u64> = items.iter()
            .map(|v| v["priority"].as_u64().unwrap_or(99))
            .collect();
        let mut sorted = priorities.clone();
        sorted.sort();
        assert_eq!(priorities, sorted, "action_items must be sorted by priority (critical first)");
    }

    #[test]
    fn workspace_payload_has_top_level_layers() {
        let report = workspace_report();
        let payload = build_run_data(&report);

        let layers = payload["layers"].as_array().unwrap();
        assert!(!layers.is_empty(), "data.layers must be present even for workspaces");
    }

    #[test]
    fn workspace_payload_has_is_workspace_flag() {
        let report = workspace_report();
        let payload = build_run_data(&report);
        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert!(payload.get("packages").is_some());
    }

    #[test]
    fn single_project_action_items_have_no_package_path() {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/single".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("hostile", "bandit", vec![
            make_finding(Severity::High, "SQL_INJECTION"),
        ])];
        report.status = ReportStatus::Partial;

        let payload = build_run_data(&report);
        assert_eq!(payload.get("is_workspace"), None, "single project must not have is_workspace");
        let items = payload["action_items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            assert!(item.get("package_path").is_none(), "single project action_items must not have package_path");
        }
    }

    fn base_report() -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/proj".to_string(),
            has_tests: true,
            package_name: Some("proj".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        BarzelReport::new(project)
    }

    #[test]
    fn diff_mode_active_when_since_and_changed_files_set() {
        let mut report = base_report();
        report.diff_since = Some("HEAD~1".to_string());
        report.diff_changed_files = Some(2);

        let payload = build_run_data(&report);
        let dm = &payload["diff_mode"];
        assert_eq!(dm["active"].as_bool(), Some(true));
        assert_eq!(dm["since"].as_str(), Some("HEAD~1"));
        assert_eq!(dm["changed_files"].as_u64(), Some(2));
        assert!(dm["fallback_reason"].is_null());
    }

    #[test]
    fn diff_mode_inactive_when_fallback() {
        let mut report = base_report();
        report.diff_since = Some("bad-rev".to_string());
        report.diff_changed_files = None;
        report.diff_fallback_reason = Some("not a git repository or invalid revision".to_string());

        let payload = build_run_data(&report);
        let dm = &payload["diff_mode"];
        assert_eq!(dm["active"].as_bool(), Some(false));
        assert_eq!(dm["since"].as_str(), Some("bad-rev"));
        assert!(dm["changed_files"].is_null());
        assert_eq!(
            dm["fallback_reason"].as_str(),
            Some("not a git repository or invalid revision")
        );
    }

    #[test]
    fn diff_mode_absent_when_since_not_set() {
        let report = base_report();
        let payload = build_run_data(&report);
        assert!(payload.get("diff_mode").is_none(), "diff_mode must be absent when --since not used");
    }

    const ACTION_ITEM_REQUIRED_KEYS: &[&str] = &[
        "priority", "layer", "runner", "severity", "code", "message", "reproduce_cmd",
    ];

    fn assert_action_items_contract(payload: &serde_json::Value) {
        let items = payload["action_items"].as_array()
            .expect("action_items must be an array");

        for (i, item) in items.iter().enumerate() {
            for key in ACTION_ITEM_REQUIRED_KEYS {
                assert!(
                    item.get(*key).is_some(),
                    "action_item[{i}] missing required key '{key}'"
                );
            }
            let reproduce = item["reproduce_cmd"].as_str().unwrap_or("");
            assert!(
                !reproduce.trim().is_empty(),
                "action_item[{i}] has empty or null reproduce_cmd (code={:?})",
                item["code"].as_str().unwrap_or("?")
            );
        }

        let priorities: Vec<u64> = items.iter()
            .map(|v| v["priority"].as_u64().unwrap_or(99))
            .collect();
        let mut sorted = priorities.clone();
        sorted.sort();
        assert_eq!(priorities, sorted, "action_items must be sorted by priority (critical first)");
    }

    fn assert_run_payload_required_keys(payload: &serde_json::Value) {
        for key in &["passed", "overall_status", "action_items", "layers",
                     "total_findings", "critical", "high", "medium", "low",
                     "report_id", "timestamp"] {
            assert!(payload.get(*key).is_some(), "run payload missing required key '{key}'");
        }
    }

    #[test]
    fn single_project_action_items_all_satisfy_contract() {
        let project = ProjectInfo {
            language: Language::Python,
            root: "/tmp/single".to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("hostile", "bandit", vec![
            make_finding(Severity::Critical, "SQL_INJECTION"),
            make_finding(Severity::High,     "OPEN_REDIRECT"),
            make_finding(Severity::Medium,   "WEAK_CIPHER"),
        ])];
        report.status = ReportStatus::Fail;

        let payload = build_run_data(&report);
        assert_action_items_contract(&payload);
        assert_eq!(payload["action_items"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn workspace_action_items_all_satisfy_contract() {
        let payload = build_run_data(&workspace_report());
        assert_action_items_contract(&payload);
    }

    #[test]
    fn info_findings_are_excluded_from_action_items() {
        let project = ProjectInfo {
            language: Language::Rust,
            root: "/tmp/p".to_string(),
            has_tests: true,
            package_name: Some("p".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.layers = vec![make_layer("logic", "cargo-test", vec![
            make_finding(Severity::Info, "TESTS_PASSED"),
            make_finding(Severity::High, "MUTATION_SURVIVED"),
        ])];
        report.status = ReportStatus::Partial;

        let payload = build_run_data(&report);
        let items = payload["action_items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "only the High finding must appear; Info must be excluded");
        assert_eq!(items[0]["code"].as_str(), Some("MUTATION_SURVIVED"));
    }

    #[test]
    fn run_payload_single_has_required_keys() {
        let project = ProjectInfo {
            language: Language::Go,
            root: "/tmp/go".to_string(),
            has_tests: true,
            package_name: Some("svc".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let report = BarzelReport::new(project);
        let payload = build_run_data(&report);

        assert_run_payload_required_keys(&payload);
        assert!(payload.get("is_workspace").is_none());
        assert!(payload.get("packages").is_none());
    }

    #[test]
    fn run_payload_workspace_has_required_keys_plus_workspace_fields() {
        let payload = build_run_data(&workspace_report());

        assert_run_payload_required_keys(&payload);
        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert!(payload["packages"].is_array());
    }

    #[test]
    fn workspace_mixed_findings_global_priority_order() {
        let project = ProjectInfo {
            language: Language::Unknown,
            root: "/tmp/ws".to_string(),
            has_tests: true,
            package_name: Some("ws".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.status = ReportStatus::Fail;

        report.workspace_members = vec![
            WorkspaceMemberReport {
                package_path: "pkg-a".to_string(),
                language: "rust".to_string(),
                status: ReportStatus::Partial,
                layers: vec![make_layer("structural", "mutants", vec![
                    make_finding(Severity::Medium, "MUTANT_SURVIVED"),
                ])],
                summary: Summary { total_findings: 1, critical: 0, high: 0, medium: 1, low: 0,
                    overall_status: ReportStatus::Partial },
            },
            WorkspaceMemberReport {
                package_path: "pkg-b".to_string(),
                language: "typescript".to_string(),
                status: ReportStatus::Fail,
                layers: vec![make_layer("hostile", "semgrep", vec![
                    make_finding(Severity::Critical, "HARDCODED_SECRET"),
                ])],
                summary: Summary { total_findings: 1, critical: 1, high: 0, medium: 0, low: 0,
                    overall_status: ReportStatus::Fail },
            },
            WorkspaceMemberReport {
                package_path: "pkg-c".to_string(),
                language: "python".to_string(),
                status: ReportStatus::Partial,
                layers: vec![make_layer("logic", "pytest", vec![
                    make_finding(Severity::High, "TEST_FAILURE"),
                ])],
                summary: Summary { total_findings: 1, critical: 0, high: 1, medium: 0, low: 0,
                    overall_status: ReportStatus::Partial },
            },
        ];
        report.layers = report.workspace_members.iter()
            .flat_map(|m| m.layers.clone()).collect();

        let payload = build_run_data(&report);
        assert_action_items_contract(&payload);

        let items = payload["action_items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["code"].as_str(), Some("HARDCODED_SECRET"), "Critical must be first");
        assert_eq!(items[1]["code"].as_str(), Some("TEST_FAILURE"),      "High must be second");
        assert_eq!(items[2]["code"].as_str(), Some("MUTANT_SURVIVED"),   "Medium must be last");
        assert_eq!(items[0]["package_path"].as_str(), Some("pkg-b"));
        assert_eq!(items[1]["package_path"].as_str(), Some("pkg-c"));
        assert_eq!(items[2]["package_path"].as_str(), Some("pkg-a"));
    }

    fn saved_report(dir: &tempfile::TempDir) -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };
        let mut report = BarzelReport::new(project);
        report.add_layer(make_layer("hostile", "semgrep", vec![
            make_finding(Severity::High, "SQL_INJECT"),
        ]));
        report.save(dir.path()).unwrap();
        report
    }

    #[test]
    fn stdio_report_latest_returns_full_report_with_id() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), None, None).unwrap();
        let report_json = &payload["report"];
        assert_eq!(report_json["id"].as_str(), Some(saved.id.as_str()));
        assert_eq!(report_json["project"]["package_name"].as_str(), Some("myapp"));
    }

    #[test]
    fn stdio_report_by_id_prefix_returns_matching_report() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);
        let prefix = &saved.id[..8];

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), Some(prefix), None).unwrap();
        assert_eq!(payload["report"]["id"].as_str(), Some(saved.id.as_str()));
    }

    #[test]
    fn stdio_report_latest_keyword_resolves_to_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let saved = saved_report(&dir);

        let payload = build_stdio_report_payload(dir.path().to_str().unwrap(), Some("latest"), None).unwrap();
        assert_eq!(payload["report"]["id"].as_str(), Some(saved.id.as_str()));
    }

    #[test]
    fn stdio_report_not_found_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), None, None);
        assert!(result.is_err(), "missing report must return Err");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found") || msg.contains("latest"),
            "error message must reference what was not found: {msg}");
    }

    #[test]
    fn stdio_report_unknown_id_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let _ = saved_report(&dir);
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), Some("nonexistent"), None);
        assert!(result.is_err(), "unknown report id must return Err");
    }

    #[test]
    fn stdio_compare_returns_verdict_and_summary_delta() {
        let dir = tempfile::tempdir().unwrap();

        let project = || ProjectInfo {
            language: Language::Rust,
            root: dir.path().to_string_lossy().to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        };

        let baseline_report = BarzelReport::new(project());
        baseline_report.save(dir.path()).unwrap();
        let baseline_id = baseline_report.id.clone();

        std::thread::sleep(std::time::Duration::from_millis(5));

        let mut head_report = BarzelReport::new(project());
        head_report.add_layer(make_layer("hostile", "semgrep", vec![
            make_finding(Severity::High, "VULN_NEW"),
        ]));
        head_report.save(dir.path()).unwrap();
        let head_id = head_report.id.clone();

        let refs = vec![baseline_id[..8].to_string(), head_id[..8].to_string()];
        let payload = build_stdio_report_payload(
            dir.path().to_str().unwrap(), None, Some(&refs),
        ).unwrap();

        let cmp = &payload["comparison"];
        assert_eq!(cmp["verdict"].as_str(), Some("regressed"));
        assert_eq!(cmp["summary_delta"]["high"].as_i64(), Some(1));
        assert!(cmp["regressions"].as_array().map(|v| !v.is_empty()).unwrap_or(false));
    }

    #[test]
    fn stdio_compare_wrong_length_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let refs = vec!["only-one".to_string()];
        let result = build_stdio_report_payload(dir.path().to_str().unwrap(), None, Some(&refs));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("2"));
    }

    fn history_entry(
        package_path: Option<&str>,
        language: &str,
        hours_ago: i64,
    ) -> crate::history::HistoryEntry {
        crate::history::HistoryEntry {
            report_id: format!("hist-{}-{}", language, hours_ago),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(hours_ago),
            project: "myapp".to_string(),
            package_path: package_path.map(str::to_string),
            language: language.to_string(),
            status: ReportStatus::Pass,
            layers: vec![crate::history::HistoryLayerMetric {
                runner: "pytest".to_string(),
                status: LayerStatus::Pass,
                mutation_score: None,
                coverage: Some(0.90 - hours_ago as f64 * 0.01),
            }],
        }
    }

    fn save_history(dir: &tempfile::TempDir, entry: &crate::history::HistoryEntry) {
        crate::history::save_entry(entry, dir.path()).unwrap();
    }

    #[test]
    fn history_no_entries_returns_empty_payload() {
        let dir = tempfile::tempdir().unwrap();
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), None, None, None);
        assert_eq!(payload["returned"].as_u64(), Some(0));
        assert_eq!(payload["total"].as_u64(), Some(0));
        assert_eq!(payload["limit"].as_u64(), Some(20));
        assert!(payload["entries"].as_array().map(|a| a.is_empty()).unwrap_or(false));
    }

    #[test]
    fn history_limit_above_cap_is_clamped_to_200() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "python", 2));
        save_history(&dir, &history_entry(None, "python", 1));

        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(999), None, None);
        assert_eq!(payload["limit"].as_u64(), Some(200));
        assert_eq!(payload["returned"].as_u64(), Some(2));
        assert_eq!(payload["total"].as_u64(), Some(2));
    }

    #[test]
    fn history_entries_returned_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "python", 3));
        save_history(&dir, &history_entry(None, "python", 1));
        save_history(&dir, &history_entry(None, "python", 2));

        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), None, None, None);
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        let ids: Vec<&str> = entries.iter().map(|e| e["report_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["hist-python-1", "hist-python-2", "hist-python-3"]);
    }

    #[test]
    fn history_limit_truncates_returned_entries() {
        let dir = tempfile::tempdir().unwrap();
        for h in 1..=5 {
            save_history(&dir, &history_entry(None, "python", h));
        }
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(2), None, None);
        assert_eq!(payload["returned"].as_u64(), Some(2));
        assert_eq!(payload["total"].as_u64(), Some(5));
        assert_eq!(payload["limit"].as_u64(), Some(2));
        assert_eq!(payload["entries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn history_limit_zero_returns_empty_entries() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "python", 1));
        let payload = build_stdio_history_payload(dir.path().to_str().unwrap(), Some(0), None, None);
        assert_eq!(payload["returned"].as_u64(), Some(0));
        assert_eq!(payload["total"].as_u64(), Some(1));
        assert!(payload["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn history_package_path_filter() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(Some("crates/api"), "rust", 2));
        save_history(&dir, &history_entry(Some("apps/web"), "typescript", 1));

        let payload = build_stdio_history_payload(
            dir.path().to_str().unwrap(), None, Some("crates/api"), None,
        );
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["package_path"].as_str(), Some("crates/api"));
        assert_eq!(payload["total"].as_u64(), Some(1));
    }

    #[test]
    fn history_language_filter() {
        let dir = tempfile::tempdir().unwrap();
        save_history(&dir, &history_entry(None, "rust", 2));
        save_history(&dir, &history_entry(None, "python", 1));

        let payload = build_stdio_history_payload(
            dir.path().to_str().unwrap(), None, None, Some("rust"),
        );
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["language"].as_str(), Some("rust"));
    }

    // ── stdio init / force flag ───────────────────────────────────────────────

    #[test]
    fn stdio_request_parses_force_true() {
        let json = r#"{"command":"init","force":true}"#;
        let req: StdioRequest = serde_json::from_str(json).unwrap();
        assert!(req.force, "force:true must be parsed from request");
    }

    #[test]
    fn stdio_request_force_defaults_to_false() {
        let json = r#"{"command":"init"}"#;
        let req: StdioRequest = serde_json::from_str(json).unwrap();
        assert!(!req.force, "force must default to false when absent");
    }

    #[test]
    fn init_build_run_data_config_status_skipped() {
        use crate::init::{InitOutcome, run_init};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"x\"").unwrap();
        // First init creates the file
        run_init(Some(dir.path()), true, false).unwrap();
        // Second init without force returns Skipped
        let outcome = run_init(Some(dir.path()), true, false).unwrap();
        assert_eq!(outcome, InitOutcome::Skipped);
    }

    #[test]
    fn init_force_returns_overwritten_outcome() {
        use crate::init::{InitOutcome, run_init};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"x\"").unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        std::fs::write(dir.path().join(".barzel.toml"), b"# sentinel").unwrap();
        let outcome = run_init(Some(dir.path()), true, true).unwrap();
        assert_eq!(outcome, InitOutcome::Overwritten);
        let content = std::fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(!content.contains("sentinel"), "force must overwrite sentinel content");
    }

    // ── tool registry / check payload ─────────────────────────────────────────

    #[test]
    fn stdio_check_tool_json_includes_install_guidance() {
        use crate::process::MockProcessRunner;
        let statuses = crate::tool_registry::probe_all(&MockProcessRunner::unavailable());
        let tool_json = crate::tool_registry::tool_statuses_to_json(&statuses);

        for entry in &tool_json {
            let install = entry["install"].as_str().unwrap_or("");
            assert!(!install.is_empty(),
                "tool '{}' must have install guidance in stdio payload",
                entry["name"].as_str().unwrap_or("?"));
        }
    }

    #[test]
    fn tool_statuses_to_json_produces_correct_shape() {
        use crate::tool_registry::{tool_statuses_to_json, ToolStatus};
        let statuses = vec![
            ToolStatus { name: "cargo", layer: "core", available: true, install: "https://rustup.rs", applicable: true, required: true, reason: "Rust project".to_string() },
            ToolStatus { name: "semgrep", layer: "hostile", available: false, install: "pip install semgrep", applicable: true, required: true, reason: "all projects".to_string() },
        ];
        let json = tool_statuses_to_json(&statuses);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["name"].as_str(), Some("cargo"));
        assert_eq!(json[0]["available"].as_bool(), Some(true));
        assert_eq!(json[1]["available"].as_bool(), Some(false));
    }

    #[test]
    fn stdio_check_tool_json_contains_expected_tools() {
        use crate::process::MockProcessRunner;
        let statuses = crate::tool_registry::probe_all(&MockProcessRunner::passing("ok"));
        let tool_json = crate::tool_registry::tool_statuses_to_json(&statuses);
        let names: Vec<&str> = tool_json.iter()
            .filter_map(|v| v["name"].as_str())
            .collect();
        assert!(names.contains(&"go-mutesting"));
        assert!(names.contains(&"cargo audit"));
        assert!(names.contains(&"pip-audit"));
        assert!(names.contains(&"semgrep"));
        assert!(names.contains(&"npm"));
        assert!(names.contains(&"pnpm"));
        assert!(names.contains(&"yarn"));
    }

    fn make_project(language: Language, root: &str) -> ProjectInfo {
        ProjectInfo {
            language,
            root: root.to_string(),
            has_tests: true,
            package_name: Some(root.split('/').last().unwrap_or("pkg").to_string()),
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    #[test]
    fn check_payload_single_project_preserves_shape() {
        use crate::detect::WorkspaceInfo;
        use crate::process::MockProcessRunner;
        let dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Single(make_project(Language::Rust, dir.path().to_str().unwrap()));
        let cfg = crate::config::BarzelConfig::default();
        let payload = crate::build_check_payload(ws, &cfg, &MockProcessRunner::passing("ok"));

        assert_eq!(payload["language"].as_str(), Some("rust"));
        assert!(payload.get("frameworks").is_some());
        assert!(payload["missing_required_tools"].is_number());
        assert!(payload["tools"].is_array());
        assert!(payload.get("is_workspace").is_none());
        assert!(payload.get("packages").is_none());

        for tool in payload["tools"].as_array().unwrap() {
            for key in &["name", "layer", "available", "install", "applicable", "required", "reason"] {
                assert!(tool.get(key).is_some(), "tool entry missing field '{key}'");
            }
        }
    }

    #[test]
    fn check_payload_multi_workspace_contract() {
        use crate::detect::{WorkspaceInfo, WorkspaceKind};
        use crate::process::MockProcessRunner;
        let rust_dir = tempfile::tempdir().unwrap();
        let ts_dir   = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Multi {
            kind: WorkspaceKind::Cargo,
            members: vec![
                ("crates/api".to_string(), make_project(Language::Rust, rust_dir.path().to_str().unwrap())),
                ("apps/web".to_string(),   make_project(Language::TypeScript, ts_dir.path().to_str().unwrap())),
            ],
        };
        let cfg = crate::config::BarzelConfig::default();
        let payload = crate::build_check_payload(ws, &cfg, &MockProcessRunner::passing("ok"));

        assert_eq!(payload["is_workspace"].as_bool(), Some(true));
        assert_eq!(payload["workspace_kind"].as_str(), Some("cargo"));

        let pkgs = payload["packages"].as_array().expect("packages must be array");
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0]["path"].as_str(), Some("crates/api"));
        assert_eq!(pkgs[1]["path"].as_str(), Some("apps/web"));

        assert!(payload["missing_required_tools"].is_number());

        let tools = payload["tools"].as_array().expect("tools must be array");
        assert!(!tools.is_empty());
        for tool in tools {
            for key in &["name", "layer", "available", "install", "applicable", "required", "reason"] {
                assert!(tool.get(key).is_some(), "tool entry missing field '{key}'");
            }
        }

        let cargo = tools.iter().find(|t| t["name"] == "cargo").expect("cargo must be present");
        assert_eq!(cargo["applicable"].as_bool(), Some(true));
        assert_eq!(cargo["required"].as_bool(), Some(true));

        let cargo_reason = cargo["reason"].as_str().unwrap_or("");
        assert!(cargo_reason.contains("crates/api"));

        assert!(payload.get("language").is_none());
        assert!(payload.get("frameworks").is_none());
    }

    #[test]
    fn single_project_member_validation_is_ok() {
        use crate::detect::WorkspaceInfo;
        let dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Single(make_project(Language::Rust, dir.path().to_str().unwrap()));
        crate::validate_workspace_member_configs(&ws).expect("Single must always pass");
    }

    #[test]
    fn workspace_member_without_local_config_is_ok() {
        use crate::detect::{WorkspaceInfo, WorkspaceKind};
        let dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceInfo::Multi {
            kind: WorkspaceKind::Cargo,
            members: vec![
                ("crates/api".to_string(), make_project(Language::Rust, dir.path().to_str().unwrap())),
            ],
        };
        crate::validate_workspace_member_configs(&ws).expect("member without local config must pass");
    }

    #[test]
    fn validate_workspace_member_configs_returns_err_on_invalid_toml() {
        use crate::detect::{WorkspaceInfo, WorkspaceKind};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".barzel.toml"), b"not: valid: toml: {{").unwrap();
        let ws = WorkspaceInfo::Multi {
            kind: WorkspaceKind::Cargo,
            members: vec![
                ("crates/broken".to_string(), make_project(Language::Rust, dir.path().to_str().unwrap())),
            ],
        };
        let err = crate::validate_workspace_member_configs(&ws).unwrap_err().to_string();
        assert!(err.contains("crates/broken"));
        assert!(err.contains(".barzel.toml"));
    }

    #[test]
    fn report_exit_code_fail_on_low_info_only_does_not_exit_1() {
        let project = ProjectInfo {
            language: Language::Rust, root: "/tmp".to_string(), has_tests: false,
            package_name: None, frameworks: ProjectFrameworks::default(), workspace_root: None,
        };
        let mut r = BarzelReport::new(project);
        r.fail_on = "low".to_string();
        r.summary.total_findings = 1;
        assert_eq!(crate::report_exit_code(&r), ExitCode::SUCCESS);
    }

    fn exit_code_report(fail_on: &str, info: usize, low: usize, medium: usize, high: usize, critical: usize) -> BarzelReport {
        let project = ProjectInfo {
            language: Language::Rust, root: "/tmp".to_string(), has_tests: false,
            package_name: None, frameworks: ProjectFrameworks::default(), workspace_root: None,
        };
        let mut r = BarzelReport::new(project);
        r.fail_on = fail_on.to_string();
        r.summary.total_findings = info + low + medium + high + critical;
        r.summary.critical = critical;
        r.summary.high     = high;
        r.summary.medium   = medium;
        r.summary.low      = low;
        r
    }

    #[test]
    fn fail_on_any_info_only_exits_1() {
        let r = exit_code_report("any", 1, 0, 0, 0, 0);
        assert_eq!(crate::report_exit_code(&r), ExitCode::from(1));
    }

    #[test]
    fn fail_on_low_with_low_finding_exits_1() {
        let r = exit_code_report("low", 0, 1, 0, 0, 0);
        assert_eq!(crate::report_exit_code(&r), ExitCode::from(1));
    }

    #[test]
    fn fail_on_low_with_medium_finding_exits_1() {
        let r = exit_code_report("low", 0, 0, 1, 0, 0);
        assert_eq!(crate::report_exit_code(&r), ExitCode::from(1));
    }

    #[test]
    fn workspace_member_with_invalid_local_config_is_rejected() {
        use crate::detect::{WorkspaceInfo, WorkspaceKind};
        let dir = tempfile::tempdir().unwrap();
        let toml = r#"
[project]
name = "broken"
language = "rust"

[layers]
enabled = ["logic"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = false
mutation_threshold = 999.0

[layers.hostile]
fuzzing = false
sast = false

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let ws = WorkspaceInfo::Multi {
            kind: WorkspaceKind::Cargo,
            members: vec![
                ("crates/broken".to_string(), make_project(Language::Rust, dir.path().to_str().unwrap())),
            ],
        };
        let err = crate::validate_workspace_member_configs(&ws).unwrap_err().to_string();
        assert!(err.contains("crates/broken"), "error must name the member path: {err}");
        assert!(err.contains("layers.structural.mutation_threshold"), "error must name the bad key: {err}");
        assert!(err.contains("0.0..=100.0"), "error must state the valid range: {err}");
    }
}
