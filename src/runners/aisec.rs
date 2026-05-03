/// AI Security Runner — detects security patterns specific to AI/LLM applications.
/// Runs on any language where AI frameworks are detected.
///
/// Checks for:
/// - Hardcoded API keys for AI providers
/// - Prompt injection vulnerabilities (user input directly in prompts)
/// - Missing input validation on LLM outputs
/// - Insecure deserialization of LLM responses
/// - Logging/tracing of sensitive data (PII in prompts)
use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::plugin::{Layer, TestRunner};
use crate::process::{OsProcessRunner, SubprocessRunner};
use crate::report::{Finding, LayerMetrics, LayerResult, LayerStatus, Severity};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct AiSecRunner {
    proc: Arc<dyn SubprocessRunner>,
}

impl Default for AiSecRunner {
    fn default() -> Self {
        Self { proc: Arc::new(OsProcessRunner) }
    }
}

/// Patterns that indicate security issues in AI applications.
struct AiSecPattern {
    code: &'static str,
    pattern: &'static str,
    severity: crate::report::Severity,
    message: &'static str,
    suggestion: &'static str,
    file_extensions: &'static [&'static str],
}

const AI_SEC_PATTERNS: &[AiSecPattern] = &[
    AiSecPattern {
        code: "HARDCODED_API_KEY",
        pattern: "sk-",
        severity: Severity::Critical,
        message: "Possible hardcoded OpenAI API key (sk- prefix)",
        suggestion: "Move API keys to environment variables. Use `os.environ['OPENAI_API_KEY']` or dotenv.",
        file_extensions: &[".py", ".ts", ".js", ".go", ".rs"],
    },
    AiSecPattern {
        code: "HARDCODED_ANTHROPIC_KEY",
        pattern: "sk-ant-",
        severity: Severity::Critical,
        message: "Possible hardcoded Anthropic API key (sk-ant- prefix)",
        suggestion: "Move API keys to environment variables. Never commit API keys to source control.",
        file_extensions: &[".py", ".ts", ".js", ".go", ".rs"],
    },
    AiSecPattern {
        code: "PROMPT_INJECTION_RISK",
        pattern: "f\"",
        severity: Severity::High,
        message: "F-string interpolation in potential prompt — risk of prompt injection",
        suggestion: "Validate and sanitize user input before including it in prompts. \
                     Use structured inputs with system/user role separation.",
        file_extensions: &[".py"],
    },
    AiSecPattern {
        code: "PROMPT_TEMPLATE_INJECTION",
        pattern: "${",
        severity: Severity::High,
        message: "Template literal in potential prompt — risk of prompt injection",
        suggestion: "Sanitize user input before template interpolation. \
                     Consider using structured message arrays instead of string templates.",
        file_extensions: &[".ts", ".js"],
    },
    AiSecPattern {
        code: "EVAL_LLM_OUTPUT",
        pattern: "eval(",
        severity: Severity::Critical,
        message: "`eval()` detected — never execute LLM-generated code without sandboxing",
        suggestion: "Never pass LLM output to eval(). Use a safe interpreter or sandbox. \
                     LLM output is untrusted user input.",
        file_extensions: &[".py", ".js", ".ts"],
    },
    AiSecPattern {
        code: "EXEC_LLM_OUTPUT",
        pattern: "exec(",
        severity: Severity::Critical,
        message: "`exec()` detected — executing LLM-generated code is a critical security risk",
        suggestion: "Use a sandboxed execution environment (e.g., subprocess with restricted permissions).",
        file_extensions: &[".py"],
    },
    AiSecPattern {
        code: "UNSAFE_PICKLE",
        pattern: "pickle.loads",
        severity: Severity::High,
        message: "`pickle.loads` detected — deserializing LLM responses via pickle is unsafe",
        suggestion: "Use JSON for LLM response deserialization. Never unpickle untrusted data.",
        file_extensions: &[".py"],
    },
    AiSecPattern {
        code: "MISSING_OUTPUT_VALIDATION",
        pattern: "json.loads(response",
        severity: Severity::Medium,
        message: "Direct JSON parsing of LLM response without validation",
        suggestion: "Always validate LLM JSON responses against a schema (e.g., Pydantic, Zod). \
                     LLMs can return malformed or adversarial JSON.",
        file_extensions: &[".py"],
    },
];

impl TestRunner for AiSecRunner {
    fn name(&self) -> &'static str { "ai-sec" }
    fn layer(&self) -> Layer { Layer::Hostile }

    fn skip_message(&self) -> &'static str {
        "No AI framework detected — ai-sec scanner only runs when AI dependencies (openai, anthropic, langchain, etc.) are found"
    }

    fn is_available(&self, project: &ProjectInfo) -> bool {
        project.frameworks.has_ai_deps
    }

    fn run(&self, project: &ProjectInfo) -> Result<LayerResult> {
        let start = Instant::now();
        let root = Path::new(&project.root);

        let mut all_findings = Vec::new();

        // First: try semgrep with AI-specific rules if available
        if self.proc.is_available("semgrep", &["--version"]) {
            if let Ok(out) = self.proc.run(
                "semgrep",
                &["--json", "--quiet", "--config=p/secrets", "."],
                root,
            ) {
                let semgrep_findings = parse_semgrep_for_ai(&out.stdout);
                all_findings.extend(semgrep_findings);
            }
        }

        // Always: run static pattern scan regardless of semgrep
        let pattern_findings = scan_source_patterns(root);
        all_findings.extend(pattern_findings);

        // Deduplicate by (file, code)
        all_findings.dedup_by(|a, b| a.code == b.code && a.location == b.location);

        // AI-specific guidance if no issues found
        if all_findings.is_empty() {
            all_findings.push(Finding {
                severity: Severity::Info,
                code: "AI_SEC_PASSED".to_string(),
                message: format!(
                    "No AI security issues detected in {} project. AI frameworks: {}",
                    project.language,
                    project.frameworks.ai_frameworks.join(", ")
                ),
                reproduce_cmd: None,
                suggestion: Some(
                    "Consider adding adversarial prompt tests. \
                     Test with inputs like 'Ignore all previous instructions and...' \
                     to verify prompt injection resistance."
                        .to_string(),
                ),
                ..Default::default()
            });
        }

        let status = if all_findings.iter().any(|f| matches!(f.severity, Severity::Critical)) {
            LayerStatus::Fail
        } else if all_findings.iter().any(|f| matches!(f.severity, Severity::High | Severity::Medium)) {
            LayerStatus::Partial
        } else {
            LayerStatus::Pass
        };

        Ok(LayerResult {
            name: "hostile".to_string(),
            runner: "ai-sec".to_string(),
            status,
            findings: all_findings,
            metrics: LayerMetrics::default(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn scan_source_patterns(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    scan_dir(root, &mut findings);
    findings
}

fn scan_dir(dir: &Path, findings: &mut Vec<Finding>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip common non-source dirs
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if matches!(name.as_str(), "node_modules" | ".git" | "target" | ".venv" | "__pycache__" | ".barzel") {
            continue;
        }
        if path.is_dir() {
            scan_dir(&path, findings);
        } else {
            scan_file(&path, findings);
        }
    }
}

fn scan_file(path: &Path, findings: &mut Vec<Finding>) {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();
    let Ok(content) = std::fs::read_to_string(path) else { return; };

    for pattern in AI_SEC_PATTERNS {
        if !pattern.file_extensions.contains(&ext.as_str()) { continue; }
        if !content.contains(pattern.pattern) { continue; }

        // Find the line number
        for (line_no, line) in content.lines().enumerate() {
            if line.contains(pattern.pattern) {
                // Skip comments
                let trimmed = line.trim();
                if trimmed.starts_with('#') || trimmed.starts_with("//") { continue; }

                findings.push(Finding {
                    severity: pattern.severity,
                    code: pattern.code.to_string(),
                    message: pattern.message.to_string(),
                    location: Some(format!("{}:{}", path.display(), line_no + 1)),
                    reproduce_cmd: Some(format!(
                        "grep -n '{}' {} | head -5",
                        pattern.pattern,
                        path.display()
                    )),
                    suggestion: Some(pattern.suggestion.to_string()),
                });
                break; // one finding per pattern per file
            }
        }
    }
}

fn parse_semgrep_for_ai(stdout: &str) -> Vec<Finding> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return vec![];
    };
    let Some(results) = json.get("results").and_then(|r| r.as_array()) else {
        return vec![];
    };

    results.iter().map(|item| {
        let severity = item
            .get("extra").and_then(|e| e.get("severity")).and_then(|s| s.as_str())
            .map(|s| match s.to_uppercase().as_str() {
                "ERROR" => Severity::Critical,
                "WARNING" => Severity::High,
                _ => Severity::Medium,
            })
            .unwrap_or(Severity::Medium);

        let code = item.get("check_id").and_then(|c| c.as_str()).unwrap_or("AI_SEC").to_string();
        Finding {
            severity,
            code: code.clone(),
            message: item.get("extra").and_then(|e| e.get("message")).and_then(|m| m.as_str())
                .unwrap_or("AI security issue").to_string(),
            location: item.get("path").and_then(|p| p.as_str()).map(|p| {
                let line = item.get("start").and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0);
                format!("{}:{}", p, line)
            }),
            reproduce_cmd: item.get("path").and_then(|p| p.as_str()).map(|p| {
                format!("semgrep --config={} {} 2>&1 | head -20", code, p)
            }),
            suggestion: Some("Review the flagged code for AI-specific security risks.".to_string()),
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectFrameworks, ProjectInfo};
    use crate::process::MockProcessRunner;
    use tempfile::tempdir;

    fn ai_project() -> ProjectInfo {
        ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some("my-ai-app".to_string()),
            frameworks: ProjectFrameworks {
                is_nextjs: false,
                has_ai_deps: true,
                ai_frameworks: vec!["OpenAI SDK".to_string()],
            },
            workspace_root: None,
        }
    }

    fn no_ai_project() -> ProjectInfo {
        ProjectInfo {
            language: Language::Python,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: None,
            frameworks: ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    #[test]
    fn name_is_ai_sec() { assert_eq!(AiSecRunner::default().name(), "ai-sec"); }

    #[test]
    fn layer_is_hostile() { assert!(matches!(AiSecRunner::default().layer(), Layer::Hostile)); }

    #[test]
    fn not_available_without_ai_deps() {
        assert!(!AiSecRunner::default().is_available(&no_ai_project()));
    }

    #[test]
    fn available_with_ai_deps() {
        assert!(AiSecRunner::default().is_available(&ai_project()));
    }

    #[test]
    fn detects_hardcoded_api_key_in_python_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), b"api_key = \"sk-1234567890abcdef\"").unwrap();

        let mut info = ai_project();
        info.root = dir.path().to_string_lossy().to_string();

        let r = AiSecRunner { proc: Arc::new(MockProcessRunner::unavailable()) };
        let result = r.run(&info).unwrap();

        assert!(result.findings.iter().any(|f| f.code == "HARDCODED_API_KEY"));
        let finding = result.findings.iter().find(|f| f.code == "HARDCODED_API_KEY").unwrap();
        assert_eq!(finding.severity, Severity::Critical);
        assert!(finding.location.is_some());
    }

    #[test]
    fn detects_eval_in_python() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("agent.py"), b"result = eval(llm_response)").unwrap();

        let mut info = ai_project();
        info.root = dir.path().to_string_lossy().to_string();

        let r = AiSecRunner { proc: Arc::new(MockProcessRunner::unavailable()) };
        let result = r.run(&info).unwrap();

        assert!(result.findings.iter().any(|f| f.code == "EVAL_LLM_OUTPUT"));
    }

    #[test]
    fn skips_commented_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("test.py"), b"# api_key = \"sk-example\"\nlegit = 1").unwrap();

        let mut info = ai_project();
        info.root = dir.path().to_string_lossy().to_string();

        let r = AiSecRunner { proc: Arc::new(MockProcessRunner::unavailable()) };
        let result = r.run(&info).unwrap();

        assert!(!result.findings.iter().any(|f| f.code == "HARDCODED_API_KEY"));
    }

    #[test]
    fn clean_project_returns_info_finding() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), b"client = openai.OpenAI()\n").unwrap();

        let mut info = ai_project();
        info.root = dir.path().to_string_lossy().to_string();

        let r = AiSecRunner { proc: Arc::new(MockProcessRunner::unavailable()) };
        let result = r.run(&info).unwrap();

        assert!(result.findings.iter().any(|f| f.code == "AI_SEC_PASSED"));
        assert!(matches!(result.status, LayerStatus::Pass));
    }

    #[test]
    fn parse_pytest_output_parses_passed() {
        let (p, f, e) = crate::runners::pytest::parse_pytest_output("5 passed in 0.45s");
        assert_eq!(p, 5); assert_eq!(f, 0); assert_eq!(e, 0);
    }

    #[test]
    fn semgrep_findings_have_reproduce_cmd() {
        let stdout = r#"{
            "results": [{
                "check_id": "python.secrets.hardcoded-api-key",
                "path": "src/app.py",
                "start": {"line": 10},
                "extra": {
                    "severity": "ERROR",
                    "message": "Hardcoded API key detected"
                }
            }]
        }"#;
        let findings = parse_semgrep_for_ai(stdout);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].reproduce_cmd.is_some(), "semgrep finding must have reproduce_cmd");
        let cmd = findings[0].reproduce_cmd.as_ref().unwrap();
        assert!(cmd.contains("python.secrets.hardcoded-api-key"));
        assert!(cmd.contains("src/app.py"));
    }
}
