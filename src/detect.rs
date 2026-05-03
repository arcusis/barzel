use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Unknown,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::TypeScript => write!(f, "typescript"),
            Language::Python => write!(f, "python"),
            Language::Go => write!(f, "go"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

/// Extended project metadata used by framework-specific runners.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ProjectFrameworks {
    /// Next.js / React project detected (next.config.*)
    pub is_nextjs: bool,
    /// AI framework detected (langchain, openai, anthropic, etc.)
    pub has_ai_deps: bool,
    /// Detected AI framework names (for reporting)
    pub ai_frameworks: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectInfo {
    pub language: Language,
    pub root: String,
    pub has_tests: bool,
    pub package_name: Option<String>,
    #[serde(default)]
    pub frameworks: ProjectFrameworks,
}

pub fn detect_project(path: &Path) -> crate::error::Result<ProjectInfo> {
    let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root_str = root.to_string_lossy().to_string();

    if root.join("Cargo.toml").exists() {
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
        let package_name = extract_package_name_from_content(&content);
        let ai_frameworks = detect_rust_ai_frameworks(&content);
        return Ok(ProjectInfo {
            language: Language::Rust,
            root: root_str,
            has_tests: root.join("tests").exists() || root.join("src").join("lib.rs").exists(),
            package_name,
            frameworks: ProjectFrameworks {
                is_nextjs: false,
                has_ai_deps: !ai_frameworks.is_empty(),
                ai_frameworks,
            },
        });
    }

    if root.join("package.json").exists() {
        let pkg_content = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
        let package_name = extract_json_string_field(&pkg_content, "name");
        let is_nextjs = root.join("next.config.js").exists()
            || root.join("next.config.ts").exists()
            || root.join("next.config.mjs").exists()
            || pkg_content.contains("\"next\"");
        let ai_frameworks = detect_js_ai_frameworks(&pkg_content);
        return Ok(ProjectInfo {
            language: Language::TypeScript,
            root: root_str,
            has_tests: root.join("tests").exists()
                || root.join("__tests__").exists()
                || root.join("src").join("__tests__").exists()
                || root.join("e2e").exists()
                || root.join("playwright.config.ts").exists()
                || root.join("playwright.config.js").exists(),
            package_name,
            frameworks: ProjectFrameworks {
                is_nextjs,
                has_ai_deps: !ai_frameworks.is_empty(),
                ai_frameworks,
            },
        });
    }

    // Python: pyproject.toml takes priority over requirements.txt
    if root.join("pyproject.toml").exists() || root.join("setup.py").exists() || root.join("requirements.txt").exists() {
        let pkg_content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap_or_default();
        let req_content = std::fs::read_to_string(root.join("requirements.txt")).unwrap_or_default();
        let combined = format!("{}\n{}", pkg_content, req_content);
        let package_name = extract_python_package_name(&pkg_content);
        let ai_frameworks = detect_python_ai_frameworks(&combined);
        return Ok(ProjectInfo {
            language: Language::Python,
            root: root_str,
            has_tests: root.join("tests").exists()
                || root.join("test").exists()
                || root.join("tests.py").exists(),
            package_name,
            frameworks: ProjectFrameworks {
                is_nextjs: false,
                has_ai_deps: !ai_frameworks.is_empty(),
                ai_frameworks,
            },
        });
    }

    if root.join("go.mod").exists() {
        let mod_content = std::fs::read_to_string(root.join("go.mod")).unwrap_or_default();
        let package_name = extract_go_module_name(&mod_content);
        let has_tests = walk_dir_has_suffix(&root, "_test.go");
        let ai_frameworks = detect_go_ai_frameworks(&mod_content);
        return Ok(ProjectInfo {
            language: Language::Go,
            root: root_str,
            has_tests,
            package_name,
            frameworks: ProjectFrameworks {
                is_nextjs: false,
                has_ai_deps: !ai_frameworks.is_empty(),
                ai_frameworks,
            },
        });
    }

    Ok(ProjectInfo {
        language: Language::Unknown,
        root: root_str,
        has_tests: false,
        package_name: None,
        frameworks: ProjectFrameworks::default(),
    })
}

// ── AI framework detection ────────────────────────────────────────────────────

const JS_AI_DEPS: &[(&str, &str)] = &[
    ("openai", "OpenAI SDK"),
    ("@anthropic-ai", "Anthropic SDK"),
    ("langchain", "LangChain"),
    ("@langchain", "LangChain"),
    ("llamaindex", "LlamaIndex"),
    ("@google/generative-ai", "Google Generative AI"),
    ("cohere-ai", "Cohere"),
    ("@mistralai", "Mistral AI"),
    ("ai", "Vercel AI SDK"),
    ("@vercel/ai", "Vercel AI SDK"),
    ("groq-sdk", "Groq"),
    ("together-ai", "Together AI"),
];

const PYTHON_AI_DEPS: &[(&str, &str)] = &[
    ("openai", "OpenAI SDK"),
    ("anthropic", "Anthropic SDK"),
    ("langchain", "LangChain"),
    ("langchain-openai", "LangChain OpenAI"),
    ("llama-index", "LlamaIndex"),
    ("llama_index", "LlamaIndex"),
    ("google-generativeai", "Google Generative AI"),
    ("cohere", "Cohere"),
    ("mistralai", "Mistral AI"),
    ("groq", "Groq"),
    ("together", "Together AI"),
    ("transformers", "HuggingFace Transformers"),
    ("sentence-transformers", "Sentence Transformers"),
    ("tiktoken", "tiktoken (OpenAI tokenizer)"),
    ("litellm", "LiteLLM"),
    ("instructor", "Instructor"),
    ("dspy", "DSPy"),
    ("crewai", "CrewAI"),
    ("autogen", "AutoGen"),
];

const RUST_AI_DEPS: &[(&str, &str)] = &[
    ("async-openai", "async-openai"),
    ("anthropic", "Anthropic SDK"),
    ("langchain-rust", "LangChain Rust"),
    ("rig-core", "Rig"),
    ("llm", "LLM crate"),
];

const GO_AI_DEPS: &[(&str, &str)] = &[
    ("github.com/sashabaranov/go-openai", "go-openai"),
    ("github.com/tmc/langchaingo", "LangChain Go"),
    ("github.com/anthropics/anthropic-sdk-go", "Anthropic SDK Go"),
];

fn detect_in_content(content: &str, registry: &[(&str, &str)]) -> Vec<String> {
    registry
        .iter()
        .filter(|(dep, _)| content.contains(dep))
        .map(|(_, name)| name.to_string())
        .collect()
}

fn detect_js_ai_frameworks(pkg_json: &str) -> Vec<String> {
    detect_in_content(pkg_json, JS_AI_DEPS)
}

fn detect_python_ai_frameworks(deps: &str) -> Vec<String> {
    detect_in_content(deps, PYTHON_AI_DEPS)
}

fn detect_rust_ai_frameworks(cargo_toml: &str) -> Vec<String> {
    detect_in_content(cargo_toml, RUST_AI_DEPS)
}

fn detect_go_ai_frameworks(go_mod: &str) -> Vec<String> {
    detect_in_content(go_mod, GO_AI_DEPS)
}

// ── Package name extraction ───────────────────────────────────────────────────

fn extract_python_package_name(pyproject: &str) -> Option<String> {
    for line in pyproject.lines() {
        let line = line.trim();
        if line.starts_with("name") && line.contains('=') {
            if let Some(val) = line.split('=').nth(1) {
                let name = val.trim().trim_matches('"').trim_matches('\'').trim_matches('"').to_string();
                if !name.is_empty() { return Some(name); }
            }
        }
    }
    None
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

/// Walk one level of a directory looking for files ending with `suffix`.
fn walk_dir_has_suffix(root: &Path, suffix: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else { return false; };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.ends_with(suffix) { return true; }
        if entry.path().is_dir() && walk_dir_has_suffix(&entry.path(), suffix) { return true; }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::tempdir;

    // ── Language Display ──────────────────────────────────────────────────────

    #[test]
    fn language_display_values() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
        assert_eq!(Language::Python.to_string(), "python");
        assert_eq!(Language::Go.to_string(), "go");
        assert_eq!(Language::Unknown.to_string(), "unknown");
    }

    // ── Rust detection ────────────────────────────────────────────────────────

    #[test]
    fn detects_rust_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test-crate\"").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Rust);
        assert_eq!(info.package_name, Some("test-crate".to_string()));
    }

    #[test]
    fn rust_has_tests_with_lib_rs_only() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"x\"").unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("lib.rs"), b"pub fn x() {}").unwrap();
        assert!(detect_project(dir.path()).unwrap().has_tests);
    }

    // ── TypeScript / Next.js detection ───────────────────────────────────────

    #[test]
    fn detects_typescript_with_package_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::TypeScript);
        assert_eq!(info.package_name, Some("my-app".to_string()));
    }

    #[test]
    fn detects_nextjs_from_next_config() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        fs::write(dir.path().join("next.config.js"), b"module.exports = {}").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.frameworks.is_nextjs);
    }

    #[test]
    fn detects_nextjs_from_package_json_dep() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"app","dependencies":{"next":"14.0.0"}}"#).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.frameworks.is_nextjs);
    }

    #[test]
    fn typescript_has_tests_with_playwright_config() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), b"{}").unwrap();
        fs::write(dir.path().join("playwright.config.ts"), b"export default {}").unwrap();
        assert!(detect_project(dir.path()).unwrap().has_tests);
    }

    // ── Python detection ──────────────────────────────────────────────────────

    #[test]
    fn detects_python_from_pyproject_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname = \"my-lib\"").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Python);
        assert_eq!(info.package_name, Some("my-lib".to_string()));
    }

    #[test]
    fn detects_python_from_requirements_txt() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), b"pytest\nrequests").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Python);
    }

    #[test]
    fn detects_python_from_setup_py() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("setup.py"), b"from setuptools import setup").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert_eq!(info.language, Language::Python);
    }

    // ── AI framework detection ────────────────────────────────────────────────

    #[test]
    fn detects_openai_in_typescript() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"dependencies":{"openai":"4.0.0"}}"#).unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.frameworks.has_ai_deps);
        assert!(info.frameworks.ai_frameworks.iter().any(|f| f.contains("OpenAI")));
    }

    #[test]
    fn detects_anthropic_in_python() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), b"anthropic\nfastapi").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.frameworks.has_ai_deps);
        assert!(info.frameworks.ai_frameworks.iter().any(|f| f.contains("Anthropic")));
    }

    #[test]
    fn detects_langchain_in_python() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), b"langchain\nlangchain-openai").unwrap();
        let info = detect_project(dir.path()).unwrap();
        assert!(info.frameworks.has_ai_deps);
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
    fn detects_unknown_project() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_project(dir.path()).unwrap().language, Language::Unknown);
    }

    // ── Helper functions ──────────────────────────────────────────────────────

    #[test]
    fn extracts_package_name_basic() {
        assert_eq!(
            extract_package_name_from_content("[package]\nname = \"my-crate\"\nversion = \"0.1.0\""),
            Some("my-crate".to_string())
        );
    }

    #[test]
    fn extract_returns_none_for_empty() {
        assert_eq!(extract_package_name_from_content(""), None);
    }

    #[test]
    fn extracts_python_package_name() {
        assert_eq!(
            extract_python_package_name("[project]\nname = \"my-lib\"\n"),
            Some("my-lib".to_string())
        );
    }

    #[test]
    fn extracts_name_when_not_first_field() {
        let json = r#"{"version":"1.0","description":"a lib","name":"real-name"}"#;
        assert_eq!(extract_json_string_field(json, "name"), Some("real-name".to_string()));
    }

    #[test]
    fn go_module_name_extracts_last_segment() {
        assert_eq!(
            extract_go_module_name("module github.com/user/my-app\n"),
            Some("my-app".to_string())
        );
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
            prop_assert!(detect_project(&sub).is_ok());
        }

        #[test]
        fn extract_with_explicit_name_round_trips(name in "[a-z][a-z0-9-]{0,20}") {
            let content = format!("[package]\nname = \"{name}\"");
            prop_assert_eq!(extract_package_name_from_content(&content), Some(name));
        }

        #[test]
        fn extract_json_field_never_panics(json in ".*", field in "[a-z]+") {
            let _ = extract_json_string_field(&json, &field);
        }

        #[test]
        fn go_module_name_never_panics(content in ".*") {
            let _ = extract_go_module_name(&content);
        }

        #[test]
        fn python_package_name_never_panics(content in ".*") {
            let _ = extract_python_package_name(&content);
        }
    }
}
