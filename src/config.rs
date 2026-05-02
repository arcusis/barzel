use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    pub design_by_contract: bool,
    pub formal_verification: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralConfig {
    pub mutation_testing: bool,
    pub mcdc_coverage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostileConfig {
    pub fuzzing: bool,
    pub sast: bool,
    pub api_contract_testing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportingConfig {
    pub format: String,
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
                enabled: vec!["logic".to_string(), "structural".to_string()],
                logic: LogicConfig {
                    property_based: true,
                    design_by_contract: true,
                    formal_verification: false,
                },
                structural: StructuralConfig {
                    mutation_testing: true,
                    mcdc_coverage: true,
                },
                hostile: HostileConfig {
                    fuzzing: false,
                    sast: true,
                    api_contract_testing: false,
                },
            },
            reporting: ReportingConfig {
                format: "json".to_string(),
                fail_on: "critical".to_string(),
            },
        }
    }
}

impl BarzelConfig {
    pub fn from_project_info(info: &crate::detect::ProjectInfo) -> Self {
        let mut cfg = Self::default();
        cfg.project.name = info.package_name.clone().unwrap_or_else(|| "project".to_string());
        cfg.project.language = info.language.to_string();

        // Enable hostile layer for Rust/TS projects
        if matches!(info.language, crate::detect::Language::Rust | crate::detect::Language::TypeScript) {
            cfg.layers.enabled.push("hostile".to_string());
        }

        cfg
    }

    #[allow(dead_code)]
    pub fn load(path: &PathBuf) -> crate::error::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: BarzelConfig = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &PathBuf) -> crate::error::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}
