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
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
        let package_name = extract_package_name_from_content(&content);
        return Ok(ProjectInfo {
            language: Language::Rust,
            root: root_str,
            has_tests: root.join("tests").exists() || root.join("src").join("lib.rs").exists(),
            package_name,
        });
    }

    if root.join("package.json").exists() {
        let pkg_content = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
        let package_name = extract_json_string_field(&pkg_content, "name");
        return Ok(ProjectInfo {
            language: Language::TypeScript,
            root: root_str,
            has_tests: root.join("tests").exists()
                || root.join("__tests__").exists()
                || root.join("src").join("__tests__").exists(),
            package_name,
        });
    }

    if root.join("go.mod").exists() {
        let mod_content = std::fs::read_to_string(root.join("go.mod")).unwrap_or_default();
        let package_name = extract_go_module_name(&mod_content);
        let has_tests = std::fs::read_dir(root.join("src"))
            .or_else(|_| std::fs::read_dir(root))
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.file_name().to_string_lossy().ends_with("_test.go")
                })
            })
            .unwrap_or(false);
        return Ok(ProjectInfo {
            language: Language::Go,
            root: root_str,
            has_tests,
            package_name,
        });
    }

    Ok(ProjectInfo {
        language: Language::Unknown,
        root: root_str,
        has_tests: false,
        package_name: None,
    })
}

fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let search = format!("\"{}\"", field);
    let pos = json.find(&search)?;
    let after_key = &json[pos + search.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();
    if let Some(value) = after_colon.strip_prefix('"') {
        let end = value.find('"')?;
        let name = value[..end].to_string();
        if name.is_empty() { None } else { Some(name) }
    } else {
        None
    }
}

fn extract_go_module_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            let name = rest.trim().to_string();
            if !name.is_empty() {
                return Some(name.split('/').next_back().unwrap_or(&name).to_string());
            }
        }
    }
    None
}

pub fn extract_package_name_from_content(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("name") {
            if let Some(value) = line.split('=').nth(1) {
                let name = value.trim().trim_matches('"').trim_matches('\'').to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_rust_project() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test-crate\"",
        )
        .unwrap();

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

    #[test]
    fn extracts_package_name_basic() {
        let content = "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"";
        assert_eq!(
            extract_package_name_from_content(content),
            Some("my-crate".to_string())
        );
    }

    #[test]
    fn extract_returns_none_for_empty() {
        assert_eq!(extract_package_name_from_content(""), None);
    }

    // ── Language Display ──────────────────────────────────────────────────────

    #[test]
    fn language_display_values() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
        assert_eq!(Language::Go.to_string(), "go");
        assert_eq!(Language::Unknown.to_string(), "unknown");
    }

    // ── TypeScript detection ──────────────────────────────────────────────────

    #[test]
    fn detects_typescript_with_package_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::TypeScript);
        assert_eq!(info.package_name, Some("my-app".to_string()));
    }

    #[test]
    fn typescript_has_tests_with_tests_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), b"{}").unwrap();
        fs::create_dir(dir.path().join("tests")).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.has_tests);
    }

    #[test]
    fn typescript_has_tests_with_dunder_tests_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), b"{}").unwrap();
        fs::create_dir(dir.path().join("__tests__")).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.has_tests);
    }

    #[test]
    fn typescript_no_tests_without_test_dirs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), b"{}").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(!info.has_tests);
    }

    #[test]
    fn rust_has_tests_with_lib_rs_only() {
        // tests/ does NOT exist, src/lib.rs DOES — catches || → && mutation
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"x\"").unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("lib.rs"), b"pub fn x() {}").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.has_tests);
    }

    // ── Go detection ──────────────────────────────────────────────────────────

    #[test]
    fn detects_go_project_with_module_name() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), b"module github.com/user/myproject\ngo 1.21\n").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Go);
        assert_eq!(info.package_name, Some("myproject".to_string()));
    }

    #[test]
    fn go_module_name_extracts_last_segment() {
        assert_eq!(
            extract_go_module_name("module github.com/user/my-app\n"),
            Some("my-app".to_string())
        );
    }

    #[test]
    fn go_module_name_returns_none_for_empty() {
        assert_eq!(extract_go_module_name(""), None);
        assert_eq!(extract_go_module_name("no module line"), None);
    }

    // ── extract_json_string_field ─────────────────────────────────────────────

    #[test]
    fn extracts_name_from_package_json() {
        let json = r#"{"name": "my-package", "version": "1.0.0"}"#;
        assert_eq!(
            extract_json_string_field(json, "name"),
            Some("my-package".to_string())
        );
    }

    #[test]
    fn returns_none_when_field_missing() {
        assert_eq!(extract_json_string_field(r#"{"version":"1.0"}"#, "name"), None);
    }

    #[test]
    fn extracts_field_when_not_first_in_object() {
        // pos is large here — catches the pos + len vs pos * len mutation
        let json = r#"{"version":"1.0","description":"a lib","name":"real-name"}"#;
        assert_eq!(
            extract_json_string_field(json, "name"),
            Some("real-name".to_string())
        );
    }

    #[test]
    fn returns_none_for_empty_string_value() {
        assert_eq!(extract_json_string_field(r#"{"name":""}"#, "name"), None);
    }

    proptest! {
        #[test]
        fn extract_never_panics(content in ".*") {
            let _ = extract_package_name_from_content(&content);
        }

        #[test]
        fn detect_always_returns_result(path_suffix in "[a-z]{1,8}") {
            let dir = tempdir().unwrap();
            let sub = dir.path().join(path_suffix);
            let result = detect_project(&sub);
            prop_assert!(result.is_ok());
        }

        #[test]
        fn extract_with_explicit_name_round_trips(name in "[a-z][a-z0-9-]{0,20}") {
            let content = format!("[package]\nname = \"{name}\"");
            let extracted = extract_package_name_from_content(&content);
            prop_assert_eq!(extracted, Some(name));
        }

        #[test]
        fn extract_json_field_never_panics(json in ".*", field in "[a-z]+") {
            let _ = extract_json_string_field(&json, &field);
        }

        #[test]
        fn go_module_name_never_panics(content in ".*") {
            let _ = extract_go_module_name(&content);
        }
    }
}
