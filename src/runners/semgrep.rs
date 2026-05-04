use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub struct SemgrepRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for SemgrepRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

/// Collect `.yml` / `.yaml` rule files from `<root>/.barzel/rules/`, sorted
/// deterministically. Returns an empty vec when the directory is absent or empty.
/// Only regular files are included — directories named `*.yaml` are silently skipped.
fn collect_custom_rule_paths(root: &Path) -> Vec<PathBuf> {
    let rules_dir = root.join(".barzel").join("rules");
    let Ok(entries) = std::fs::read_dir(&rules_dir) else { return vec![]; };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e == "yml" || e == "yaml")
                    .unwrap_or(false)
        })
        .collect();

    paths.sort();
    paths
}

/// Wrap `s` in single quotes for safe POSIX shell inclusion.
/// Embedded single quotes are escaped as `'\''`.
/// Used only when building human/agent reproduce strings, never for argv.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Collect all custom rule paths for a run: workspace-root rules (sorted) come
/// first, then package-root rules (sorted). Canonical paths deduplicate entries
/// so that when `workspace_root == project.root` no rule is passed twice.
fn collect_all_custom_rule_paths(project_root: &Path, workspace_root: Option<&str>) -> Vec<PathBuf> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut result = Vec::new();

    // Workspace-root rules first
    if let Some(ws_root) = workspace_root {
        let ws_path = Path::new(ws_root);
        if ws_path != project_root {
            for p in collect_custom_rule_paths(ws_path) {
                let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
                if seen.insert(canonical) {
                    result.push(p);
                }
            }
        }
    }

    // Package-root rules
    for p in collect_custom_rule_paths(project_root) {
        let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
        if seen.insert(canonical) {
            result.push(p);
        }
    }

    result
}

/// Build the full semgrep argv as owned Strings.
/// Shape: `--json --quiet --config <ruleset> [--config <rule>...] .`
fn build_semgrep_args(ruleset: &str, custom_rules: &[PathBuf]) -> Vec<String> {
    let mut args = vec![
        "--json".to_string(),
        "--quiet".to_string(),
        "--config".to_string(),
        ruleset.to_string(),
    ];
    for rule in custom_rules {
        args.push("--config".to_string());
        args.push(rule.to_string_lossy().into_owned());
    }
    args.push(".".to_string());
    args
}

/// Format all `--config` values into a shell-pasteable reproduce command.
/// Config paths are shell-quoted to handle spaces; the literal target `.` is not quoted.
fn build_error_reproduce_cmd(ruleset: &str, custom_rules: &[PathBuf]) -> String {
    let configs: Vec<String> = std::iter::once(format!("--config {}", shell_quote(ruleset)))
        .chain(custom_rules.iter().map(|p| format!("--config {}", shell_quote(&p.to_string_lossy()))))
        .collect();
    format!("semgrep {} . 2>&1", configs.join(" "))
}

impl TestRunner for SemgrepRunner {
    fn name(&self) -> &'static str {
        "semgrep"
    }

    fn layer(&self) -> Layer {
        Layer::Hostile
    }

    fn skip_message(&self) -> &'static str {
        "semgrep not installed — install: `pip install semgrep` or `brew install semgrep` (runs on all languages)"
    }

    fn is_available(&self, _project: &ProjectInfo) -> bool {
        self.proc.is_available("semgrep", &["--version"])
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        use crate::detect::Language;
        let start = Instant::now();
        let root = Path::new(&project.root);

        let ruleset = match project.language {
            Language::Python => "p/python",
            Language::TypeScript => "p/typescript",
            Language::Rust => "p/rust",
            Language::Go => "p/golang",
            Language::Unknown => "p/default",
        };

        // Collect custom rules: workspace-root rules first (sorted), then package-root
        // rules (sorted). Canonical paths deduplicate when workspace_root == project.root.
        let custom_rules = collect_all_custom_rule_paths(root, project.workspace_root.as_deref());
        let args_owned = build_semgrep_args(ruleset, &custom_rules);
        let args_ref: Vec<&str> = args_owned.iter().map(String::as_str).collect();

        // Collect all config values for finding-level reproduce_cmd
        let all_configs: Vec<String> = std::iter::once(ruleset.to_string())
            .chain(custom_rules.iter().map(|p| p.to_string_lossy().into_owned()))
            .collect();

        match self.proc.run("semgrep", &args_ref, root) {
            Ok(out) => {
                let findings = parse_semgrep_json(&out.stdout, &all_configs);

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
                    reproduce_cmd: Some(build_error_reproduce_cmd(ruleset, &custom_rules)),
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

fn parse_semgrep_json(output: &str, all_configs: &[String]) -> Vec<Finding> {
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

            // Include all configs in the reproduce command so the exact run can be
            // replicated regardless of whether the finding came from a standard
            // ruleset or a local custom rule. Paths are shell-quoted to handle spaces.
            let reproduce_cmd = path.map(|p| {
                let config_args: Vec<String> = all_configs
                    .iter()
                    .map(|c| format!("--config {}", shell_quote(c)))
                    .collect();
                format!("semgrep {} --include {} .", config_args.join(" "), shell_quote(p))
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
    use crate::detect::{Language, ProjectInfo};
    use crate::process::MockProcessRunner;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn info() -> ProjectInfo {
        ProjectInfo { language: Language::Rust, root: "/tmp".to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None }
    }

    fn info_at(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::Python, root: root.to_string(), has_tests: false, package_name: None, frameworks: Default::default(), workspace_root: None }
    }

    fn runner_with(mock: MockProcessRunner) -> SemgrepRunner {
        SemgrepRunner { proc: Arc::new(mock) }
    }

    // ── metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn name_is_semgrep() {
        assert_eq!(SemgrepRunner::default().name(), "semgrep");
    }

    #[test]
    fn layer_is_hostile() {
        assert!(matches!(SemgrepRunner::default().layer(), Layer::Hostile));
    }

    #[test]
    fn skip_message_mentions_semgrep() {
        assert!(SemgrepRunner::default().skip_message().contains("semgrep"));
    }

    // ── is_available ──────────────────────────────────────────────────────────

    #[test]
    fn available_when_command_succeeds() {
        assert!(runner_with(MockProcessRunner::passing("semgrep 1.0")).is_available(&info()));
    }

    #[test]
    fn not_available_when_command_fails() {
        assert!(!runner_with(MockProcessRunner::unavailable()).is_available(&info()));
    }

    // ── collect_custom_rule_paths ─────────────────────────────────────────────

    #[test]
    fn no_rules_dir_returns_empty() {
        let dir = tempdir().unwrap();
        assert!(collect_custom_rule_paths(dir.path()).is_empty());
    }

    #[test]
    fn empty_rules_dir_returns_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".barzel/rules")).unwrap();
        assert!(collect_custom_rule_paths(dir.path()).is_empty());
    }

    #[test]
    fn ignores_non_yaml_files() {
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("notes.txt"), b"not a rule").unwrap();
        std::fs::write(rules.join("README.md"), b"docs").unwrap();
        assert!(collect_custom_rule_paths(dir.path()).is_empty());
    }

    #[test]
    fn ignores_directory_named_with_yaml_extension() {
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        // A directory ending in .yaml must not be treated as a rule file
        std::fs::create_dir_all(rules.join("ignored.yaml")).unwrap();
        std::fs::write(rules.join("real.yml"), b"rules:").unwrap();
        let paths = collect_custom_rule_paths(dir.path());
        assert_eq!(paths.len(), 1);
        assert!(paths[0].file_name().unwrap() == "real.yml");
    }

    #[test]
    fn collects_yml_and_yaml_files_sorted() {
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("z-rules.yaml"), b"rules:").unwrap();
        std::fs::write(rules.join("a-rules.yml"), b"rules:").unwrap();
        std::fs::write(rules.join("m-rules.yaml"), b"rules:").unwrap();
        std::fs::write(rules.join("skip.txt"), b"ignored").unwrap();

        let paths = collect_custom_rule_paths(dir.path());
        assert_eq!(paths.len(), 3);
        // Must be sorted deterministically
        let names: Vec<&str> = paths.iter()
            .map(|p| p.file_name().and_then(|n| n.to_str()).unwrap())
            .collect();
        assert_eq!(names, ["a-rules.yml", "m-rules.yaml", "z-rules.yaml"]);
    }

    // ── shell_quote ───────────────────────────────────────────────────────────

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("p/python"), "'p/python'");
    }

    #[test]
    fn shell_quote_handles_paths_with_spaces() {
        assert_eq!(shell_quote("/my rules/custom.yml"), "'/my rules/custom.yml'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn reproduce_cmd_with_spaces_in_rule_path_is_quoted() {
        let json = r#"{"results":[{
            "check_id": "my-rule",
            "path": "src/my file.py",
            "start": {"line": 1},
            "extra": {"severity": "ERROR", "message": "issue"}
        }]}"#;
        let configs = vec![
            "p/python".to_string(),
            "/project/my rules/custom.yml".to_string(),
        ];
        let findings = parse_semgrep_json(json, &configs);
        let cmd = findings[0].reproduce_cmd.as_deref().unwrap();
        // Both the config path and the --include path must be quoted
        assert!(cmd.contains("'p/python'"), "ruleset must be quoted");
        assert!(cmd.contains("'/project/my rules/custom.yml'"), "rule path with space must be quoted");
        assert!(cmd.contains("'src/my file.py'"), "file path with space must be quoted");
    }

    #[test]
    fn error_reproduce_cmd_with_spaces_is_quoted() {
        let custom = vec![PathBuf::from("/project/my rules/custom.yml")];
        let cmd = build_error_reproduce_cmd("p/python", &custom);
        assert!(cmd.contains("'p/python'"));
        assert!(cmd.contains("'/project/my rules/custom.yml'"));
    }

    // ── build_semgrep_args ────────────────────────────────────────────────────

    #[test]
    fn no_custom_rules_produces_standard_args() {
        let args = build_semgrep_args("p/python", &[]);
        assert_eq!(args, ["--json", "--quiet", "--config", "p/python", "."]);
    }

    #[test]
    fn custom_rules_appended_after_language_ruleset() {
        let rules = vec![
            PathBuf::from(".barzel/rules/a.yml"),
            PathBuf::from(".barzel/rules/b.yaml"),
        ];
        let args = build_semgrep_args("p/rust", &rules);
        assert_eq!(args, [
            "--json", "--quiet",
            "--config", "p/rust",
            "--config", ".barzel/rules/a.yml",
            "--config", ".barzel/rules/b.yaml",
            ".",
        ]);
    }

    // ── run() with custom rules ───────────────────────────────────────────────

    #[test]
    fn run_without_custom_rules_command_shape_unchanged() {
        // Verify the args passed to the process runner match the pre-custom-rules shape.
        use std::sync::Mutex;
        struct CapturingProc(Mutex<Vec<Vec<String>>>);
        impl SubprocessRunner for CapturingProc {
            fn run(&self, _cmd: &str, args: &[&str], _cwd: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                self.0.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
                Ok(crate::process::ProcessOutput { stdout: r#"{"results":[]}"#.to_string(), stderr: String::new(), success: true })
            }
        }
        let dir = tempdir().unwrap();
        let captured = Arc::new(CapturingProc(Mutex::new(vec![])));
        let runner = SemgrepRunner { proc: captured.clone() };
        runner.run(&info_at(dir.path().to_str().unwrap())).unwrap();

        let calls = captured.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ["--json", "--quiet", "--config", "p/python", "."]);
    }

    #[test]
    fn run_with_custom_rules_includes_each_rule_path() {
        use std::sync::Mutex;
        struct CapturingProc(Mutex<Vec<Vec<String>>>);
        impl SubprocessRunner for CapturingProc {
            fn run(&self, _cmd: &str, args: &[&str], _cwd: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                self.0.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
                Ok(crate::process::ProcessOutput { stdout: r#"{"results":[]}"#.to_string(), stderr: String::new(), success: true })
            }
        }
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("a.yml"), b"rules:").unwrap();
        std::fs::write(rules.join("b.yaml"), b"rules:").unwrap();

        let captured = Arc::new(CapturingProc(Mutex::new(vec![])));
        let runner = SemgrepRunner { proc: captured.clone() };
        runner.run(&info_at(dir.path().to_str().unwrap())).unwrap();

        let calls = captured.0.lock().unwrap();
        let args = &calls[0];
        // Language ruleset comes first
        assert_eq!(&args[..4], ["--json", "--quiet", "--config", "p/python"]);
        // Both custom rules present in deterministic order
        let config_pairs: Vec<(&str, &str)> = args.windows(2)
            .filter(|w| w[0] == "--config")
            .map(|w| ("--config", w[1].as_str()))
            .collect();
        let configs: Vec<&str> = config_pairs.iter().map(|(_, v)| *v).collect();
        assert!(configs.contains(&"p/python"));
        assert!(configs.iter().any(|c| c.ends_with("a.yml")));
        assert!(configs.iter().any(|c| c.ends_with("b.yaml")));
        // Ends with "."
        assert_eq!(args.last().map(String::as_str), Some("."));
    }

    #[test]
    fn error_reproduce_cmd_includes_all_configs() {
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("custom.yml"), b"rules:").unwrap();

        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
            }
        }
        let runner = SemgrepRunner { proc: Arc::new(BrokenProc) };
        let result = runner.run(&info_at(dir.path().to_str().unwrap())).unwrap();

        let cmd = result.findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(cmd.contains("p/python"), "reproduce_cmd must include language ruleset");
        assert!(cmd.contains("custom.yml"), "reproduce_cmd must include custom rule path");
    }

    // ── run() core behavior ───────────────────────────────────────────────────

    #[test]
    fn run_returns_pass_with_no_findings() {
        let result = runner_with(MockProcessRunner::passing(r#"{"results":[]}"#))
            .run(&info())
            .unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn run_returns_fail_on_critical_finding() {
        let json = r#"{"results":[{"check_id":"sqli","path":"src/db.rs","start":{"line":1},"extra":{"severity":"ERROR","message":"SQL injection"}}]}"#;
        let result = runner_with(MockProcessRunner::passing(json)).run(&info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings.len(), 1);
        assert!(matches!(result.findings[0].severity, Severity::Critical));
    }

    #[test]
    fn run_returns_fail_on_subprocess_error() {
        struct BrokenProc;
        impl SubprocessRunner for BrokenProc {
            fn run(&self, _: &str, _: &[&str], _: &std::path::Path) -> std::io::Result<crate::process::ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "semgrep not found"))
            }
        }
        let runner = SemgrepRunner { proc: Arc::new(BrokenProc) };
        let result = runner.run(&info()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }

    // ── parse_semgrep_json ────────────────────────────────────────────────────

    #[test]
    fn empty_json_returns_no_findings() {
        assert!(parse_semgrep_json("", &[]).is_empty());
        assert!(parse_semgrep_json("{}", &[]).is_empty());
        assert!(parse_semgrep_json(r#"{"results":[]}"#, &[]).is_empty());
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
        let findings = parse_semgrep_json(json, &["p/rust".to_string()]);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Critical));
        assert_eq!(findings[0].code, "rules.sqli");
        assert!(findings[0].location.as_deref().unwrap().contains("src/db.rs"));
        assert!(findings[0].location.as_deref().unwrap().contains("42"));
    }

    #[test]
    fn finding_reproduce_cmd_includes_all_configs() {
        let json = r#"{"results":[{
            "check_id": "my-rule",
            "path": "src/app.py",
            "start": {"line": 1},
            "extra": {"severity": "ERROR", "message": "issue"}
        }]}"#;
        let configs = vec!["p/python".to_string(), ".barzel/rules/custom.yml".to_string()];
        let findings = parse_semgrep_json(json, &configs);
        let cmd = findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(cmd.contains("--config 'p/python'"));
        assert!(cmd.contains("--config '.barzel/rules/custom.yml'"));
        assert!(cmd.contains("--include 'src/app.py'"));
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

    // ── collect_all_custom_rule_paths (workspace integration) ─────────────────

    #[test]
    fn workspace_rules_included_when_package_has_none() {
        let dir = tempdir().unwrap();
        // Workspace root has rules; package dir has none
        let ws_rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&ws_rules).unwrap();
        std::fs::write(ws_rules.join("shared.yml"), b"rules:").unwrap();

        let pkg = dir.path().join("packages/api");
        std::fs::create_dir_all(&pkg).unwrap();

        let paths = collect_all_custom_rule_paths(&pkg, Some(dir.path().to_str().unwrap()));
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("shared.yml"));
    }

    #[test]
    fn both_workspace_and_package_rules_included_in_order() {
        let dir = tempdir().unwrap();
        // Workspace root rules
        let ws_rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&ws_rules).unwrap();
        std::fs::write(ws_rules.join("ws-rule.yml"), b"rules:").unwrap();
        // Package rules
        let pkg = dir.path().join("packages/api");
        let pkg_rules = pkg.join(".barzel/rules");
        std::fs::create_dir_all(&pkg_rules).unwrap();
        std::fs::write(pkg_rules.join("pkg-rule.yml"), b"rules:").unwrap();

        let paths = collect_all_custom_rule_paths(&pkg, Some(dir.path().to_str().unwrap()));
        assert_eq!(paths.len(), 2);
        // Workspace rules come first
        assert!(paths[0].to_string_lossy().contains("ws-rule.yml"));
        assert!(paths[1].to_string_lossy().contains("pkg-rule.yml"));
    }

    #[test]
    fn no_duplication_when_workspace_root_equals_project_root() {
        let dir = tempdir().unwrap();
        let rules = dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("rule.yml"), b"rules:").unwrap();

        // workspace_root == project.root → same dir, rules should appear once
        let paths = collect_all_custom_rule_paths(dir.path(), Some(dir.path().to_str().unwrap()));
        assert_eq!(paths.len(), 1, "rule must not be duplicated when workspace_root == project.root");
    }

    #[test]
    fn run_includes_workspace_rules_for_package_with_none() {
        use std::sync::Mutex;
        struct CapturingProc(Mutex<Vec<Vec<String>>>);
        impl SubprocessRunner for CapturingProc {
            fn run(&self, _cmd: &str, args: &[&str], _cwd: &Path) -> std::io::Result<crate::process::ProcessOutput> {
                self.0.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
                Ok(crate::process::ProcessOutput { stdout: r#"{"results":[]}"#.to_string(), stderr: String::new(), success: true })
            }
        }
        let ws_dir = tempdir().unwrap();
        let ws_rules = ws_dir.path().join(".barzel/rules");
        std::fs::create_dir_all(&ws_rules).unwrap();
        std::fs::write(ws_rules.join("shared.yml"), b"rules:").unwrap();

        let pkg_dir = tempdir().unwrap(); // package has no local rules

        let mut project = info_at(pkg_dir.path().to_str().unwrap());
        project.workspace_root = Some(ws_dir.path().to_string_lossy().into_owned());

        let captured = Arc::new(CapturingProc(Mutex::new(vec![])));
        let runner = SemgrepRunner { proc: captured.clone() };
        runner.run(&project).unwrap();

        let calls = captured.0.lock().unwrap();
        let args = &calls[0];
        assert!(args.iter().any(|a| a.ends_with("shared.yml")),
            "workspace-root rule must appear in semgrep argv");
    }

    proptest! {
        #[test]
        fn parse_semgrep_json_never_panics(s in ".*") {
            let _ = parse_semgrep_json(&s, &[]);
        }

        #[test]
        fn map_severity_never_panics(s in ".*") {
            let _ = map_semgrep_severity(&s);
        }
    }
}
