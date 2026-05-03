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
