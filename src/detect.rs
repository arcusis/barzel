use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    Go,
    Unknown,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::TypeScript => write!(f, "typescript"),
            Language::Go => write!(f, "go"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectInfo {
    pub language: Language,
    pub root: String,
    pub has_tests: bool,
    pub package_name: Option<String>,
}

pub fn detect_project(path: &Path) -> crate::error::Result<ProjectInfo> {
    let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root_str = root.to_string_lossy().to_string();

    if root.join("Cargo.toml").exists() {
        let package_name = extract_rust_package_name(&root);
        return Ok(ProjectInfo {
            language: Language::Rust,
            root: root_str,
            has_tests: root.join("tests").exists() || root.join("src").join("lib.rs").exists(),
            package_name,
        });
    }

    if root.join("package.json").exists() {
        return Ok(ProjectInfo {
            language: Language::TypeScript,
            root: root_str,
            has_tests: root.join("tests").exists() || root.join("__tests__").exists(),
            package_name: None,
        });
    }

    if root.join("go.mod").exists() {
        return Ok(ProjectInfo {
            language: Language::Go,
            root: root_str,
            has_tests: root.join("*_test.go").exists(),
            package_name: None,
        });
    }

    Ok(ProjectInfo {
        language: Language::Unknown,
        root: root_str,
        has_tests: false,
        package_name: None,
    })
}

fn extract_rust_package_name(root: &Path) -> Option<String> {
    let cargo_toml = root.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name") {
                if let Some(name) = line.split('=').nth(1) {
                    return Some(name.trim().trim_matches('"').trim_matches('\'').to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_rust_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test-crate\"").unwrap();

        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Rust);
        assert_eq!(info.package_name, Some("test-crate".to_string()));
    }

    #[test]
    fn detects_unknown_project() {
        let dir = tempdir().unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Unknown);
    }
}
