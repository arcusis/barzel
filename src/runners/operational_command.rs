use crate::config::OperationalCommandConfig;
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct OperationalCommandRunner {
    commands: Vec<OperationalCommandConfig>,
    proc: Arc<dyn SubprocessRunner>,
}

impl OperationalCommandRunner {
    pub fn new(commands: Vec<OperationalCommandConfig>) -> Self {
        Self { commands, proc: Arc::new(OsProcessRunner) }
    }
}

/// Produce a shell-pasteable reproduce string for a configured command.
/// Both the executable and every arg are single-quoted; a `cd <dir> &&` prefix
/// is prepended when `cwd` is set.
fn build_reproduce_cmd(cfg: &OperationalCommandConfig) -> String {
    let quoted_cmd = shell_quote(&cfg.cmd);
    let quoted_args: Vec<String> = cfg.args.iter().map(|a| shell_quote(a)).collect();
    let base = if quoted_args.is_empty() {
        quoted_cmd
    } else {
        format!("{} {}", quoted_cmd, quoted_args.join(" "))
    };
    match &cfg.cwd {
        Some(cwd) => format!("cd {} && {}", shell_quote(cwd), base),
        None => base,
    }
}

/// Wrap `s` in POSIX single quotes; escape embedded single quotes as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Truncate long command output to keep finding messages readable.
fn truncate_output(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.len() <= max_chars {
        trimmed.to_string()
    } else {
        format!("{}… (truncated)", trimmed[..max_chars].trim_end())
    }
}

/// Resolve the working directory for a command.
/// - `None` → project root (default)
/// - Relative path → joined with project root
/// - Absolute path → used as-is
fn resolve_cwd(project_root: &Path, cwd: Option<&str>) -> std::path::PathBuf {
    match cwd {
        None => project_root.to_path_buf(),
        Some(c) => {
            let p = Path::new(c);
            if p.is_absolute() { p.to_path_buf() } else { project_root.join(p) }
        }
    }
}

impl TestRunner for OperationalCommandRunner {
    fn name(&self) -> &'static str { "operational-cmd" }
    fn layer(&self) -> Layer { Layer::Operational }

    fn skip_message(&self) -> &'static str {
        "no commands configured — add [[layers.operational.commands]] to .barzel.toml"
    }

    fn is_available(&self, _project: &ProjectInfo) -> bool {
        !self.commands.is_empty()
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let project_root = Path::new(&project.root);
        let mut findings = Vec::new();
        let mut passed: u64 = 0;
        let mut failed: u64 = 0;

        for cmd_cfg in &self.commands {
            let cwd = resolve_cwd(project_root, cmd_cfg.cwd.as_deref());
            let args_ref: Vec<&str> = cmd_cfg.args.iter().map(String::as_str).collect();
            let reproduce = build_reproduce_cmd(cmd_cfg);

            match self.proc.run_timed(&cmd_cfg.cmd, &args_ref, &cwd, cmd_cfg.timeout_ms) {
                Ok(out) if out.success => {
                    passed += 1;
                }
                Ok(out) => {
                    failed += 1;
                    let combined = format!("{}\n{}", out.stdout, out.stderr);
                    let snippet = if combined.trim().is_empty() {
                        "(no output)".to_string()
                    } else {
                        truncate_output(combined.trim(), 300)
                    };
                    findings.push(Finding {
                        severity: Severity::High,
                        code: "OPERATIONAL_COMMAND_FAILED".to_string(),
                        message: format!(
                            "Command '{}' exited non-zero: {}",
                            cmd_cfg.name, snippet
                        ),
                        location: Some(format!("{} {}", cmd_cfg.cmd, cmd_cfg.args.join(" "))),
                        reproduce_cmd: Some(reproduce.clone()),
                        suggestion: Some(format!(
                            "Run the command manually to diagnose the failure: {}",
                            reproduce
                        )),
                    });
                }
                Err(err) => {
                    failed += 1;
                    let code = if err.kind() == std::io::ErrorKind::TimedOut {
                        "OPERATIONAL_COMMAND_TIMEOUT"
                    } else {
                        "OPERATIONAL_COMMAND_SPAWN_FAILED"
                    };
                    let sev = if err.kind() == std::io::ErrorKind::TimedOut {
                        Severity::High
                    } else {
                        Severity::Critical
                    };
                    let suggestion = if err.kind() == std::io::ErrorKind::TimedOut {
                        format!(
                            "Command exceeded timeout of {}ms. \
                             Increase timeout_ms in .barzel.toml or optimize the command.",
                            cmd_cfg.timeout_ms
                        )
                    } else {
                        format!(
                            "Ensure '{}' is installed and on PATH, then re-run barzel.",
                            cmd_cfg.cmd
                        )
                    };
                    findings.push(Finding {
                        severity: sev,
                        code: code.to_string(),
                        message: format!("Command '{}' ('{}') failed: {}", cmd_cfg.name, cmd_cfg.cmd, err),
                        location: Some(cmd_cfg.cmd.clone()),
                        reproduce_cmd: Some(reproduce),
                        suggestion: Some(suggestion),
                    });
                }
            }
        }

        let status = if !findings.is_empty() { LayerStatus::Fail } else { LayerStatus::Pass };

        Ok(LayerResult {
            name: "operational".to_string(),
            runner: "operational-cmd".to_string(),
            status,
            findings,
            metrics: LayerMetrics {
                tests_run: self.commands.len() as u64,
                passed,
                failed,
                ..Default::default()
            },
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::{MockProcessRunner, ProcessOutput};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn project_at(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::Unknown,
            root: root.to_string(),
            has_tests: false,
            package_name: None,
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    fn project() -> ProjectInfo { project_at("/tmp") }

    fn make_cmd(name: &str, cmd: &str, args: &[&str]) -> OperationalCommandConfig {
        OperationalCommandConfig {
            name: name.to_string(),
            cmd: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            timeout_ms: 5000,
        }
    }

    fn runner_with(commands: Vec<OperationalCommandConfig>, proc: impl SubprocessRunner + 'static) -> OperationalCommandRunner {
        OperationalCommandRunner { commands, proc: Arc::new(proc) }
    }

    // ── metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn name_is_operational_cmd() {
        assert_eq!(OperationalCommandRunner::new(vec![]).name(), "operational-cmd");
    }

    #[test]
    fn layer_is_operational() {
        assert!(matches!(OperationalCommandRunner::new(vec![]).layer(), Layer::Operational));
    }

    #[test]
    fn not_available_when_no_commands() {
        assert!(!OperationalCommandRunner::new(vec![]).is_available(&project()));
    }

    #[test]
    fn available_when_commands_configured() {
        let r = OperationalCommandRunner::new(vec![make_cmd("c", "python", &["--version"])]);
        assert!(r.is_available(&project()));
    }

    // ── pass ──────────────────────────────────────────────────────────────────

    #[test]
    fn pass_when_command_exits_zero() {
        let r = runner_with(
            vec![make_cmd("check", "python", &["--version"])],
            MockProcessRunner::passing("Python 3.11"),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Pass));
        assert!(result.findings.is_empty());
        assert_eq!(result.metrics.tests_run, 1);
        assert_eq!(result.metrics.passed, 1);
        assert_eq!(result.metrics.failed, 0);
    }

    #[test]
    fn all_pass_metrics_correct() {
        let r = runner_with(
            vec![make_cmd("a", "cmd1", &[]), make_cmd("b", "cmd2", &[])],
            MockProcessRunner::passing("ok"),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.metrics.tests_run, 2);
        assert_eq!(result.metrics.passed, 2);
        assert_eq!(result.metrics.failed, 0);
    }

    // ── non-zero exit ─────────────────────────────────────────────────────────

    #[test]
    fn nonzero_exit_is_high_severity() {
        let r = runner_with(
            vec![make_cmd("migrate", "python", &["manage.py", "migrate", "--check"])],
            MockProcessRunner::failing("unapplied migrations"),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::High);
        assert_eq!(result.findings[0].code, "OPERATIONAL_COMMAND_FAILED");
        assert!(result.findings[0].message.contains("migrate"));
    }

    #[test]
    fn nonzero_exit_output_in_finding() {
        let r = runner_with(
            vec![make_cmd("check", "sh", &[])],
            MockProcessRunner::failing("something went wrong"),
        );
        let result = r.run(&project()).unwrap();
        assert!(result.findings[0].message.contains("something went wrong"));
    }

    #[test]
    fn nonzero_exit_has_reproduce_cmd() {
        let r = runner_with(
            vec![make_cmd("check", "python", &["manage.py", "check"])],
            MockProcessRunner::failing("error"),
        );
        let result = r.run(&project()).unwrap();
        let repr = result.findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(repr.contains("python"), "reproduce_cmd must contain executable");
        assert!(repr.contains("manage.py"), "reproduce_cmd must contain args");
    }

    #[test]
    fn nonzero_exit_layer_is_fail() {
        let r = runner_with(
            vec![make_cmd("check", "false", &[])],
            MockProcessRunner::failing(""),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.failed, 1);
    }

    // ── spawn error ───────────────────────────────────────────────────────────

    #[test]
    fn spawn_error_is_critical_severity() {
        // MockProcessRunner::spawn_error("not found") returns Err(io::Error) — true spawn failure
        let r = runner_with(
            vec![make_cmd("check", "nonexistent-binary", &[])],
            MockProcessRunner::spawn_error("not found"),
        );
        let result = r.run(&project()).unwrap();
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert_eq!(result.findings[0].code, "OPERATIONAL_COMMAND_SPAWN_FAILED");
        assert!(result.findings[0].message.contains("nonexistent-binary"));
    }

    #[test]
    fn spawn_error_has_reproduce_cmd() {
        let r = runner_with(
            vec![make_cmd("check", "my-tool", &["--verify"])],
            MockProcessRunner::spawn_error("not found"),
        );
        let result = r.run(&project()).unwrap();
        let repr = result.findings[0].reproduce_cmd.as_deref().unwrap();
        assert!(repr.contains("my-tool"));
        assert!(repr.contains("--verify"));
    }

    #[test]
    fn spawn_error_layer_is_fail() {
        let r = runner_with(vec![make_cmd("check", "missing", &[])], MockProcessRunner::spawn_error("not found"));
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
    }

    // ── timeout ───────────────────────────────────────────────────────────────

    #[test]
    fn timeout_produces_timed_out_finding() {
        // Simulate a timeout by returning io::ErrorKind::TimedOut
        struct TimedOutProc;
        impl SubprocessRunner for TimedOutProc {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out after 5000ms"))
            }
            fn run_timed(&self, cmd: &str, args: &[&str], cwd: &Path, _timeout_ms: u64) -> std::io::Result<ProcessOutput> {
                self.run(cmd, args, cwd)
            }
        }
        let r = runner_with(vec![make_cmd("slow-cmd", "slow", &[])], TimedOutProc);
        let result = r.run(&project()).unwrap();
        assert_eq!(result.findings[0].code, "OPERATIONAL_COMMAND_TIMEOUT");
        assert_eq!(result.findings[0].severity, Severity::High);
        assert!(result.findings[0].suggestion.as_deref().unwrap().contains("timeout_ms"));
    }

    #[test]
    fn timeout_value_is_passed_to_run_timed() {
        // Capture the timeout_ms passed to run_timed
        struct TimeoutRecorder(Mutex<Vec<u64>>);
        impl SubprocessRunner for TimeoutRecorder {
            fn run(&self, _: &str, _: &[&str], _: &Path) -> std::io::Result<ProcessOutput> {
                Ok(ProcessOutput { success: true, stdout: String::new(), stderr: String::new() })
            }
            fn run_timed(&self, cmd: &str, args: &[&str], cwd: &Path, timeout_ms: u64) -> std::io::Result<ProcessOutput> {
                self.0.lock().unwrap().push(timeout_ms);
                self.run(cmd, args, cwd)
            }
        }
        let recorded = Arc::new(TimeoutRecorder(Mutex::new(vec![])));
        let mut cfg = make_cmd("check", "python", &["--version"]);
        cfg.timeout_ms = 12_345;
        let r = OperationalCommandRunner { commands: vec![cfg], proc: recorded.clone() };
        r.run(&project()).unwrap();
        assert_eq!(recorded.0.lock().unwrap()[0], 12_345,
            "timeout_ms from config must be passed to run_timed");
    }

    // ── mixed results ─────────────────────────────────────────────────────────

    #[test]
    fn mixed_pass_and_fail_layer_is_fail() {
        let r = runner_with(
            vec![make_cmd("a", "ok", &[]), make_cmd("b", "fail", &[])],
            MockProcessRunner::sequence(vec![
                ProcessOutput { success: true, stdout: "ok".into(), stderr: String::new() },
                ProcessOutput { success: false, stdout: String::new(), stderr: "err".into() },
            ]),
        );
        let result = r.run(&project()).unwrap();
        assert!(matches!(result.status, LayerStatus::Fail));
        assert_eq!(result.metrics.passed, 1);
        assert_eq!(result.metrics.failed, 1);
    }

    // ── reproduce_cmd formatting ──────────────────────────────────────────────

    #[test]
    fn reproduce_cmd_quotes_executable_and_args() {
        let cfg = make_cmd("test", "python", &["manage.py", "test"]);
        let repr = build_reproduce_cmd(&cfg);
        assert!(repr.starts_with("'python'"), "executable must be single-quoted");
        assert!(repr.contains("'manage.py'"), "args must be single-quoted");
    }

    #[test]
    fn reproduce_cmd_includes_cwd_prefix() {
        let mut cfg = make_cmd("test", "pytest", &[]);
        cfg.cwd = Some("backend".to_string());
        let repr = build_reproduce_cmd(&cfg);
        assert!(repr.starts_with("cd 'backend' && "), "cwd must be shell-quoted cd prefix");
    }

    #[test]
    fn reproduce_cmd_no_cwd_no_prefix() {
        let cfg = make_cmd("test", "pytest", &[]);
        let repr = build_reproduce_cmd(&cfg);
        assert!(!repr.contains("cd "));
        assert!(repr.starts_with("'pytest'"));
    }

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("path with spaces"), "'path with spaces'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    // ── cwd resolution ────────────────────────────────────────────────────────

    #[test]
    fn relative_cwd_is_joined_with_project_root() {
        let dir = tempdir().unwrap();
        let resolved = resolve_cwd(dir.path(), Some("backend"));
        assert_eq!(resolved, dir.path().join("backend"));
    }

    #[test]
    fn absolute_cwd_used_as_is() {
        let dir = tempdir().unwrap();
        let abs = dir.path().to_path_buf();
        let resolved = resolve_cwd(Path::new("/some/root"), Some(abs.to_str().unwrap()));
        assert_eq!(resolved, abs);
    }

    #[test]
    fn none_cwd_uses_project_root() {
        let dir = tempdir().unwrap();
        let resolved = resolve_cwd(dir.path(), None);
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn cwd_is_passed_to_subprocess() {
        // A mock that records the cwd it received
        struct CwdRecorder(Mutex<Vec<PathBuf>>);
        impl SubprocessRunner for CwdRecorder {
            fn run(&self, _: &str, _: &[&str], cwd: &Path) -> std::io::Result<ProcessOutput> {
                self.0.lock().unwrap().push(cwd.to_path_buf());
                Ok(ProcessOutput { success: true, stdout: String::new(), stderr: String::new() })
            }
        }
        let recorder = Arc::new(CwdRecorder(Mutex::new(vec![])));
        let dir = tempdir().unwrap();
        let mut cfg = make_cmd("test", "pytest", &[]);
        cfg.cwd = Some("backend".to_string());
        let r = OperationalCommandRunner { commands: vec![cfg], proc: recorder.clone() };
        let p = project_at(dir.path().to_str().unwrap());
        r.run(&p).unwrap();
        let recorded = recorder.0.lock().unwrap();
        assert_eq!(recorded[0], dir.path().join("backend"),
            "relative cwd must be joined with project root and passed to subprocess");
    }

    // ── output truncation ─────────────────────────────────────────────────────

    #[test]
    fn long_output_is_truncated_in_finding() {
        let long_output = "x".repeat(500);
        let r = runner_with(
            vec![make_cmd("check", "sh", &[])],
            MockProcessRunner::sequence(vec![
                ProcessOutput { success: false, stdout: long_output, stderr: String::new() }
            ]),
        );
        let result = r.run(&project()).unwrap();
        let msg = &result.findings[0].message;
        assert!(msg.contains("truncated"), "long output must be truncated in finding");
        assert!(msg.len() < 500, "truncated message must be shorter than raw output");
    }
}
