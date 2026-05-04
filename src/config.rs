use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarzelConfig {
    pub project: ProjectConfig,
    pub layers: LayersConfig,
    pub reporting: ReportingConfig,
    #[serde(default)]
    pub history: HistoryConfig,
}

/// Controls local history snapshots and metric-regression findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    /// When false, history is still saved but regression findings are not injected.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum absolute drop in coverage (0.0–1.0) before a finding is emitted.
    /// Default 0.0 means any decrease triggers a finding.
    /// Example: 0.02 means only report drops greater than 2 percentage points.
    /// Values outside 0.0..=1.0 are clamped to the nearest valid bound.
    #[serde(default)]
    pub coverage_regression_tolerance: f64,
    /// Minimum absolute drop in mutation score (0.0–1.0) before a finding is emitted.
    /// Values outside 0.0..=1.0 are clamped to the nearest valid bound.
    #[serde(default)]
    pub mutation_regression_tolerance: f64,
    /// Maximum history entries to keep per (package_path, language) group after each save.
    /// Oldest entries are pruned first. 0 means keep all entries (no pruning).
    /// Default: 50.
    #[serde(default = "default_max_entries")]
    pub max_entries_per_package: usize,
}

fn default_true() -> bool { true }
fn default_max_entries() -> usize { 50 }

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            coverage_regression_tolerance: 0.0,
            mutation_regression_tolerance: 0.0,
            max_entries_per_package: 50,
        }
    }
}

impl HistoryConfig {
    /// Return a validated copy with tolerances clamped to [0.0, 1.0].
    ///
    /// A negative tolerance inverts the regression signal (`drop > negative` is true for
    /// any improvement). Non-finite values (NaN, ±Inf) are treated as 0.0.
    /// Values above 1.0 suppress all real-world findings because scores are fractions
    /// (0.91 = 91%) and a drop can never exceed 1.0.
    pub fn normalized(&self) -> Self {
        let clamp_tolerance = |v: f64| {
            if !v.is_finite() { 0.0 } else { v.clamp(0.0, 1.0) }
        };
        Self {
            enabled: self.enabled,
            coverage_regression_tolerance: clamp_tolerance(self.coverage_regression_tolerance),
            mutation_regression_tolerance: clamp_tolerance(self.mutation_regression_tolerance),
            max_entries_per_package: self.max_entries_per_package,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayersConfig {
    pub enabled: Vec<String>,
    pub logic: LogicConfig,
    pub structural: StructuralConfig,
    pub hostile: HostileConfig,
    #[serde(default)]
    pub operational: OperationalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OperationalConfig {
    #[serde(default)]
    pub health_checks: Vec<HealthCheckConfig>,
    #[serde(default)]
    pub commands: Vec<OperationalCommandConfig>,
}

/// A custom command to run during the Operational layer.
///
/// ```toml
/// [[layers.operational.commands]]
/// name    = "db-migrate-check"
/// cmd     = "python"
/// args    = ["manage.py", "migrate", "--check"]
/// cwd     = "backend"   # relative to project root; omit to use project root
/// timeout_ms = 10000
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalCommandConfig {
    /// Human-readable name shown in findings and report output.
    pub name: String,
    /// Binary/executable to run (no shell expansion).
    pub cmd: String,
    /// Arguments passed directly to the binary. Default: empty.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory relative to the project root. Default: project root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Timeout in milliseconds. Default: 30000.
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 { 30_000 }

/// A single HTTP health-check endpoint to verify during the Operational layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// Human-readable name shown in findings and report output.
    pub name: String,
    /// Full URL to GET, e.g. `http://localhost:3000/health`.
    pub url: String,
    /// HTTP status code considered healthy. Default: 200.
    #[serde(default = "default_expected_status")]
    pub expected_status: u16,
    /// Request timeout in milliseconds. Default: 5000.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_expected_status() -> u16 { 200 }
fn default_timeout_ms() -> u64 { 5000 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicConfig {
    pub property_based: bool,
    pub formal_verification: bool,
    /// Minimum coverage percentage (0–100) to pass the Logic layer.
    /// Omit or set to null to disable the check.
    #[serde(default)]
    pub min_coverage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralConfig {
    pub mutation_testing: bool,
    /// Minimum mutation score to pass (0.0–100.0). Default: 95.0.
    pub mutation_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostileConfig {
    pub fuzzing: bool,
    pub sast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportingConfig {
    pub format: String,
    /// Minimum severity that causes a non-zero exit: critical | high | medium | any.
    pub fail_on: String,
}

impl Default for BarzelConfig {
    fn default() -> Self {
        Self {
            project: ProjectConfig {
                name: "unknown".to_string(),
                language: "unknown".to_string(),
            },
            layers: LayersConfig {
                enabled: vec![
                    "logic".to_string(),
                    "structural".to_string(),
                    "hostile".to_string(),
                    "operational".to_string(),
                ],
                logic: LogicConfig {
                    property_based: true,
                    formal_verification: true,
                    min_coverage: None,
                },
                structural: StructuralConfig {
                    mutation_testing: true,
                    mutation_threshold: 95.0,
                },
                hostile: HostileConfig {
                    fuzzing: true,
                    sast: true,
                },
                operational: OperationalConfig::default(),
            },
            reporting: ReportingConfig {
                format: "json".to_string(),
                fail_on: "high".to_string(),
            },
            history: HistoryConfig::default(),
        }
    }
}

impl BarzelConfig {
    /// Load from `.barzel.toml` in `root`, falling back to defaults if missing or unreadable.
    pub fn load_for_project(root: &Path) -> Self {
        let config_path = root.join(".barzel.toml");
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                match toml::from_str::<Self>(&content) {
                    Ok(mut cfg) => {
                        cfg.history = cfg.history.normalized();
                        return cfg;
                    }
                    Err(e) => eprintln!("barzel: warning: .barzel.toml is invalid — using defaults ({})", e),
                }
            }
        }
        Self::default()
    }

    pub fn from_project_info(info: &crate::detect::ProjectInfo) -> Self {
        let mut cfg = Self::default();
        cfg.project.name = info
            .package_name
            .clone()
            .unwrap_or_else(|| "project".to_string());
        cfg.project.language = info.language.to_string();
        cfg
    }

    /// Validate semantically meaningful config fields.
    /// Rejects empty layer lists, unknown layer names, and invalid `fail_on` values.
    /// Syntax errors are caught earlier by TOML parsing; this covers parseable-but-wrong values.
    pub fn validate(&self) -> crate::error::Result<()> {
        const VALID_LAYERS: &[&str] = &["logic", "structural", "hostile", "operational"];
        const VALID_FAIL_ON: &[&str] = &["critical", "high", "medium", "low", "any"];

        if self.layers.enabled.is_empty() {
            return Err(crate::error::BarzelError::Config(format!(
                "layers.enabled must not be empty; valid layers: {}",
                VALID_LAYERS.join(", ")
            )));
        }

        let bad_layers: Vec<&str> = self
            .layers
            .enabled
            .iter()
            .filter(|l| !VALID_LAYERS.contains(&l.as_str()))
            .map(String::as_str)
            .collect();
        if !bad_layers.is_empty() {
            return Err(crate::error::BarzelError::Config(format!(
                "unknown layer{} in layers.enabled: {}; valid layers: {}",
                if bad_layers.len() == 1 { "" } else { "s" },
                bad_layers.join(", "),
                VALID_LAYERS.join(", ")
            )));
        }

        if !VALID_FAIL_ON.contains(&self.reporting.fail_on.as_str()) {
            return Err(crate::error::BarzelError::Config(format!(
                "invalid reporting.fail_on '{}'; valid values: {}",
                self.reporting.fail_on,
                VALID_FAIL_ON.join(", ")
            )));
        }

        Ok(())
    }

    pub fn save(&self, path: &Path) -> crate::error::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Language, ProjectInfo};
    use tempfile::tempdir;

    fn rust_info(name: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::Rust,
            root: "/tmp".to_string(),
            has_tests: true,
            package_name: Some(name.to_string()),
            frameworks: Default::default(),
            workspace_root: None,
        }
    }

    // ── load_for_project ──────────────────────────────────────────────────────

    #[test]
    fn load_for_project_returns_default_when_no_file() {
        let dir = tempdir().unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        let def = BarzelConfig::default();
        assert_eq!(cfg.layers.structural.mutation_threshold, def.layers.structural.mutation_threshold);
        assert_eq!(cfg.reporting.fail_on, def.reporting.fail_on);
    }

    #[test]
    fn load_for_project_reads_barzel_toml() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "loaded-project"
language = "rust"

[layers]
enabled = ["logic"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = true
mutation_threshold = 80.0

[layers.hostile]
fuzzing = false
sast = true

[reporting]
format = "json"
fail_on = "critical"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert_eq!(cfg.project.name, "loaded-project");
        assert_eq!(cfg.layers.structural.mutation_threshold, 80.0);
        assert_eq!(cfg.reporting.fail_on, "critical");
        assert_eq!(cfg.layers.enabled, vec!["logic"]);
    }

    #[test]
    fn load_for_project_falls_back_on_invalid_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".barzel.toml"), b"not: valid: toml: {{").unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        // Returns defaults even when TOML is invalid (warning emitted to stderr — not tested here)
        let def = BarzelConfig::default();
        assert_eq!(cfg.reporting.fail_on, def.reporting.fail_on);
        assert_eq!(cfg.layers.structural.mutation_threshold, def.layers.structural.mutation_threshold);
    }

    // ── from_project_info ─────────────────────────────────────────────────────

    #[test]
    fn from_project_info_sets_name_and_language() {
        let cfg = BarzelConfig::from_project_info(&rust_info("my-crate"));
        assert_eq!(cfg.project.name, "my-crate");
        assert_eq!(cfg.project.language, "rust");
    }

    #[test]
    fn from_project_info_defaults_name_when_none() {
        let info = ProjectInfo {
            language: Language::TypeScript,
            root: "/tmp".to_string(),
            has_tests: false,
            package_name: None,
            frameworks: Default::default(),
            workspace_root: None,
        };
        let cfg = BarzelConfig::from_project_info(&info);
        assert_eq!(cfg.project.name, "project");
        assert_eq!(cfg.project.language, "typescript");
    }

    // ── save / round-trip ─────────────────────────────────────────────────────

    #[test]
    fn save_creates_readable_toml_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".barzel.toml");
        let cfg = BarzelConfig::from_project_info(&rust_info("test-crate"));
        cfg.save(&path).unwrap();
        assert!(path.exists());
        let loaded = BarzelConfig::load_for_project(dir.path());
        assert_eq!(loaded.project.name, "test-crate");
    }

    #[test]
    fn mutation_threshold_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".barzel.toml");
        let mut cfg = BarzelConfig::default();
        cfg.layers.structural.mutation_threshold = 80.0;
        cfg.save(&path).unwrap();
        let loaded = BarzelConfig::load_for_project(dir.path());
        assert!((loaded.layers.structural.mutation_threshold - 80.0).abs() < 0.001);
    }

    #[test]
    fn min_coverage_defaults_to_none() {
        let cfg = BarzelConfig::default();
        assert!(cfg.layers.logic.min_coverage.is_none());
    }

    #[test]
    fn min_coverage_parses_from_toml() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "cov-test"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false
min_coverage = 85.0

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert_eq!(cfg.layers.logic.min_coverage, Some(85.0));
    }

    #[test]
    fn min_coverage_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".barzel.toml");
        let mut cfg = BarzelConfig::default();
        cfg.layers.logic.min_coverage = Some(75.0);
        cfg.save(&path).unwrap();
        let loaded = BarzelConfig::load_for_project(dir.path());
        assert_eq!(loaded.layers.logic.min_coverage, Some(75.0));
    }

    #[test]
    fn no_operational_section_defaults_to_empty_health_checks() {
        let cfg = BarzelConfig::default();
        assert!(cfg.layers.operational.health_checks.is_empty(),
            "no configured health_checks must produce an empty vec by default");
    }

    #[test]
    fn missing_operational_section_in_toml_loads_with_empty_checks() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "rust"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert!(cfg.layers.operational.health_checks.is_empty(),
            "existing .barzel.toml without [layers.operational] must load cleanly");
    }

    #[test]
    fn health_checks_parse_from_toml() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[[layers.operational.health_checks]]
name = "api"
url = "http://localhost:3000/health"
expected_status = 200
timeout_ms = 5000

[[layers.operational.health_checks]]
name = "worker"
url = "http://localhost:4000/ready"
expected_status = 204
timeout_ms = 3000

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        let checks = &cfg.layers.operational.health_checks;
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "api");
        assert_eq!(checks[0].url, "http://localhost:3000/health");
        assert_eq!(checks[0].expected_status, 200);
        assert_eq!(checks[0].timeout_ms, 5000);
        assert_eq!(checks[1].name, "worker");
        assert_eq!(checks[1].expected_status, 204);
    }

    #[test]
    fn health_check_defaults_applied() {
        let dir = tempdir().unwrap();
        // Omit expected_status and timeout_ms — defaults should apply
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[[layers.operational.health_checks]]
name = "api"
url = "http://localhost:3000/health"

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        let check = &cfg.layers.operational.health_checks[0];
        assert_eq!(check.expected_status, 200, "default expected_status is 200");
        assert_eq!(check.timeout_ms, 5000, "default timeout_ms is 5000");
    }

    #[test]
    fn no_commands_section_defaults_to_empty() {
        let cfg = BarzelConfig::default();
        assert!(cfg.layers.operational.commands.is_empty(),
            "no configured commands must produce empty vec by default");
    }

    #[test]
    fn operational_commands_parse_from_toml() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[[layers.operational.commands]]
name = "db-migrate-check"
cmd = "python"
args = ["manage.py", "migrate", "--check"]
cwd = "backend"
timeout_ms = 10000

[[layers.operational.commands]]
name = "lint"
cmd = "ruff"
args = ["check", "."]

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        let cmds = &cfg.layers.operational.commands;
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "db-migrate-check");
        assert_eq!(cmds[0].cmd, "python");
        assert_eq!(cmds[0].args, ["manage.py", "migrate", "--check"]);
        assert_eq!(cmds[0].cwd.as_deref(), Some("backend"));
        assert_eq!(cmds[0].timeout_ms, 10000);
        assert_eq!(cmds[1].name, "lint");
        assert_eq!(cmds[1].cwd, None);
        assert_eq!(cmds[1].timeout_ms, 30000, "default timeout_ms is 30000");
    }

    #[test]
    fn existing_toml_without_commands_loads_cleanly() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[[layers.operational.health_checks]]
name = "api"
url = "http://localhost:3000/health"

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert!(cfg.layers.operational.commands.is_empty(),
            ".barzel.toml without [[layers.operational.commands]] must load cleanly with empty commands");
    }

    // ── [history] config ──────────────────────────────────────────────────────

    #[test]
    fn history_defaults_to_enabled_with_zero_tolerance() {
        let cfg = BarzelConfig::default();
        assert!(cfg.history.enabled, "history.enabled must default to true");
        assert_eq!(cfg.history.coverage_regression_tolerance, 0.0);
        assert_eq!(cfg.history.mutation_regression_tolerance, 0.0);
    }

    #[test]
    fn old_barzel_toml_without_history_section_loads_cleanly() {
        let dir = tempdir().unwrap();
        // A .barzel.toml with no [history] section at all
        let toml = r#"
[project]
name = "legacy-app"
language = "rust"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert!(cfg.history.enabled,
            "missing [history] section must fall back to enabled=true default");
        assert_eq!(cfg.history.coverage_regression_tolerance, 0.0);
    }

    #[test]
    fn history_section_parses_tolerances() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"

[history]
enabled = true
coverage_regression_tolerance = 0.02
mutation_regression_tolerance = 0.05
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert!(cfg.history.enabled);
        assert!((cfg.history.coverage_regression_tolerance - 0.02).abs() < 1e-9);
        assert!((cfg.history.mutation_regression_tolerance - 0.05).abs() < 1e-9);
    }

    // ── HistoryConfig::normalized ─────────────────────────────────────────────

    #[test]
    fn negative_tolerance_clamped_to_zero() {
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: -0.05,
            mutation_regression_tolerance: -1.0,
            ..HistoryConfig::default()
        };
        let n = cfg.normalized();
        assert_eq!(n.coverage_regression_tolerance, 0.0,
            "negative coverage tolerance must be clamped to 0.0");
        assert_eq!(n.mutation_regression_tolerance, 0.0,
            "negative mutation tolerance must be clamped to 0.0");
    }

    #[test]
    fn tolerance_above_one_clamped_to_one() {
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: 1.5,
            mutation_regression_tolerance: 99.0,
            ..HistoryConfig::default()
        };
        let n = cfg.normalized();
        assert_eq!(n.coverage_regression_tolerance, 1.0,
            "tolerance > 1.0 must be clamped to 1.0 (scores are stored as fractions)");
        assert_eq!(n.mutation_regression_tolerance, 1.0);
    }

    #[test]
    fn valid_tolerance_passes_through_unchanged() {
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: 0.02,
            mutation_regression_tolerance: 0.05,
            ..HistoryConfig::default()
        };
        let n = cfg.normalized();
        assert!((n.coverage_regression_tolerance - 0.02).abs() < 1e-9);
        assert!((n.mutation_regression_tolerance - 0.05).abs() < 1e-9);
    }

    #[test]
    fn zero_tolerance_passes_through_unchanged() {
        let cfg = HistoryConfig::default();
        let n = cfg.normalized();
        assert_eq!(n.coverage_regression_tolerance, 0.0);
        assert_eq!(n.mutation_regression_tolerance, 0.0);
        assert!(n.enabled);
    }

    #[test]
    fn enabled_flag_preserved_through_normalization() {
        let cfg = HistoryConfig { enabled: false, ..HistoryConfig::default() };
        assert!(!cfg.normalized().enabled);
    }

    #[test]
    fn history_max_entries_defaults_to_50() {
        let cfg = BarzelConfig::default();
        assert_eq!(cfg.history.max_entries_per_package, 50);
    }

    #[test]
    fn old_barzel_toml_without_max_entries_loads_with_default_50() {
        let dir = tempdir().unwrap();
        // A .barzel.toml with [history] but no max_entries_per_package field
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"

[history]
enabled = true
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert_eq!(cfg.history.max_entries_per_package, 50,
            "missing max_entries_per_package must default to 50");
    }

    #[test]
    fn max_entries_zero_parses_and_preserved_through_normalization() {
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: 0.0,
            mutation_regression_tolerance: 0.0,
            max_entries_per_package: 0,
        };
        assert_eq!(cfg.normalized().max_entries_per_package, 0,
            "max_entries=0 (no pruning) must survive normalization unchanged");
    }

    #[test]
    fn max_entries_preserved_through_normalization() {
        let cfg = HistoryConfig { max_entries_per_package: 10, ..HistoryConfig::default() };
        assert_eq!(cfg.normalized().max_entries_per_package, 10);
    }

    #[test]
    fn load_for_project_normalizes_invalid_tolerances() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"

[history]
enabled = true
coverage_regression_tolerance = -0.2
mutation_regression_tolerance = 2.0
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert_eq!(cfg.history.coverage_regression_tolerance, 0.0,
            "negative tolerance in .barzel.toml must be clamped to 0.0 on load");
        assert_eq!(cfg.history.mutation_regression_tolerance, 1.0,
            "tolerance > 1.0 in .barzel.toml must be clamped to 1.0 on load");
    }

    #[test]
    fn non_finite_tolerance_treated_as_zero() {
        let cfg = HistoryConfig {
            enabled: true,
            coverage_regression_tolerance: f64::NAN,
            mutation_regression_tolerance: f64::INFINITY,
            ..HistoryConfig::default()
        };
        let n = cfg.normalized();
        assert_eq!(n.coverage_regression_tolerance, 0.0, "NaN tolerance must become 0.0");
        assert_eq!(n.mutation_regression_tolerance, 0.0, "Inf tolerance must become 0.0");
    }

    #[test]
    fn history_enabled_false_parses() {
        let dir = tempdir().unwrap();
        let toml = r#"
[project]
name = "myapp"
language = "python"

[layers]
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"

[history]
enabled = false
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let cfg = BarzelConfig::load_for_project(dir.path());
        assert!(!cfg.history.enabled, "history.enabled = false must parse correctly");
    }

    // ── BarzelConfig::validate ────────────────────────────────────────────────

    #[test]
    fn default_config_validates_successfully() {
        BarzelConfig::default().validate().expect("default config must be valid");
    }

    #[test]
    fn empty_layers_enabled_is_rejected() {
        let mut cfg = BarzelConfig::default();
        cfg.layers.enabled = vec![];
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("layers.enabled"), "error must mention layers.enabled: {err}");
        assert!(err.contains("must not be empty"), "error must say 'must not be empty': {err}");
        assert!(err.contains("logic"), "error must list valid layers: {err}");
    }

    #[test]
    fn unknown_layer_in_enabled_is_rejected() {
        let mut cfg = BarzelConfig::default();
        cfg.layers.enabled = vec!["security".to_string()];
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("security"), "error must name the bad value: {err}");
        assert!(err.contains("hostile"), "error must list valid layers: {err}");
    }

    #[test]
    fn uppercase_layer_in_enabled_is_rejected() {
        let mut cfg = BarzelConfig::default();
        cfg.layers.enabled = vec!["Hostile".to_string()];
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("Hostile"), "error must name the bad value: {err}");
        assert!(err.contains("hostile"), "error must show correct casing: {err}");
    }

    #[test]
    fn invalid_fail_on_is_rejected() {
        let mut cfg = BarzelConfig::default();
        cfg.reporting.fail_on = "severe".to_string();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("severe"), "error must name the bad value: {err}");
        assert!(err.contains("critical"), "error must list valid thresholds: {err}");
        assert!(err.contains("any"), "error must list 'any' as valid: {err}");
    }

    #[test]
    fn run_verification_rejects_invalid_config_before_running() {
        let dir = tempdir().unwrap();
        // Write a parseable but semantically invalid .barzel.toml
        let toml = r#"
[project]
name = "myapp"
language = "rust"

[layers]
enabled = ["security"]

[layers.logic]
property_based = true
formal_verification = true

[layers.structural]
mutation_testing = true
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.path().join(".barzel.toml"), toml).unwrap();
        let result = crate::run::run_verification(
            Some(dir.path()),
            None,
            false, false, true, false, None,
        );
        assert!(result.is_err(), "invalid config must cause run_verification to return Err");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("security"), "error must name the bad layer: {msg}");
        assert!(msg.contains("hostile"), "error must list valid layers: {msg}");
    }
}
