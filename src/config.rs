use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarzelConfig {
    pub project: ProjectConfig,
    pub layers: LayersConfig,
    pub reporting: ReportingConfig,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicConfig {
    pub property_based: bool,
    pub formal_verification: bool,
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
                ],
                logic: LogicConfig {
                    property_based: true,
                    formal_verification: true,
                },
                structural: StructuralConfig {
                    mutation_testing: true,
                    mutation_threshold: 95.0,
                },
                hostile: HostileConfig {
                    fuzzing: true,
                    sast: true,
                },
            },
            reporting: ReportingConfig {
                format: "json".to_string(),
                fail_on: "high".to_string(),
            },
        }
    }
}

impl BarzelConfig {
    /// Load from `.barzel.toml` in `root`, falling back to defaults if missing or unreadable.
    pub fn load_for_project(root: &Path) -> Self {
        let config_path = root.join(".barzel.toml");
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(cfg) = toml::from_str::<Self>(&content) {
                    return cfg;
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
        // Should silently fall back to default
        let def = BarzelConfig::default();
        assert_eq!(cfg.reporting.fail_on, def.reporting.fail_on);
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
}
