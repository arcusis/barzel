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
    /// Absolute path to the workspace root, when this project is a workspace member.
    /// Absent for single-project repos and the synthetic workspace-root project itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// Result of workspace detection.
#[derive(Debug, Clone)]
pub enum WorkspaceInfo {
    /// Ordinary single-project repository — keeps all existing behavior
    Single(ProjectInfo),
    /// Monorepo with multiple independently runnable packages.
    /// Each member is (relative_path_from_root, ProjectInfo).
    Multi {
        kind: WorkspaceKind,
        members: Vec<(String, ProjectInfo)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Cargo,
    Pnpm,
    Npm,
    Lerna,
    Turbo,
}

impl std::fmt::Display for WorkspaceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceKind::Cargo => write!(f, "cargo"),
            WorkspaceKind::Pnpm => write!(f, "pnpm"),
            WorkspaceKind::Npm => write!(f, "npm"),
            WorkspaceKind::Lerna => write!(f, "lerna"),
            WorkspaceKind::Turbo => write!(f, "turbo"),
        }
    }
}

/// Detect whether `path` is a workspace root and enumerate its members.
/// Falls back to `WorkspaceInfo::Single` for ordinary single-project repos.
/// Single-project behavior is completely unchanged.
pub fn detect_workspace(path: &Path) -> crate::error::Result<WorkspaceInfo> {
    let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let root_str = root.to_string_lossy().to_string();

    // Cargo workspace: root Cargo.toml with [workspace] section
    if root.join("Cargo.toml").exists() {
        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
        if content.contains("[workspace]") {
            let members = stamp_workspace_root(parse_cargo_workspace_members(&root, &content), &root_str);
            if !members.is_empty() {
                return Ok(WorkspaceInfo::Multi { kind: WorkspaceKind::Cargo, members });
            }
        }
    }

    // pnpm workspace: pnpm-workspace.yaml
    if root.join("pnpm-workspace.yaml").exists() {
        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap_or_default();
        let patterns_owned = parse_yaml_list_items(&content);
        let patterns: Vec<&str> = patterns_owned.iter().map(|s| s.as_str()).collect();
        let members = stamp_workspace_root(expand_js_workspace_patterns(&root, &patterns), &root_str);
        if !members.is_empty() {
            return Ok(WorkspaceInfo::Multi { kind: WorkspaceKind::Pnpm, members });
        }
    }

    // npm/yarn workspaces or turborepo: package.json with "workspaces" field
    if root.join("package.json").exists() {
        let pkg = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
        if pkg.contains("\"workspaces\"") {
            let members = stamp_workspace_root(parse_npm_workspace_members(&root, &pkg), &root_str);
            if !members.is_empty() {
                let kind = if root.join("turbo.json").exists() { WorkspaceKind::Turbo } else { WorkspaceKind::Npm };
                return Ok(WorkspaceInfo::Multi { kind, members });
            }
        }
    }

    // lerna.json without npm workspaces
    if root.join("lerna.json").exists() {
        let content = std::fs::read_to_string(root.join("lerna.json")).unwrap_or_default();
        let members = stamp_workspace_root(parse_lerna_members(&root, &content), &root_str);
        if !members.is_empty() {
            return Ok(WorkspaceInfo::Multi { kind: WorkspaceKind::Lerna, members });
        }
    }

    Ok(WorkspaceInfo::Single(detect_project(path)?))
}

/// Set `workspace_root` on every member to the detected workspace root path.
fn stamp_workspace_root(members: Vec<(String, ProjectInfo)>, root: &str) -> Vec<(String, ProjectInfo)> {
    members.into_iter().map(|(rel, mut info)| {
        info.workspace_root = Some(root.to_string());
        (rel, info)
    }).collect()
}

/// Returns `(relative_path, ProjectInfo)` pairs from a Cargo workspace root.
///
/// Uses `toml` parsing instead of line-scanning to handle:
/// - `workspace.members` patterns (literal and glob)
/// - `workspace.exclude` to skip matched members
/// - Root packages (Cargo.toml has both `[package]` and `[workspace]`)
/// - De-duplication (overlapping patterns like `["crates/*", "crates/api"]`)
fn parse_cargo_workspace_members(root: &Path, content: &str) -> Vec<(String, ProjectInfo)> {
    // Try TOML-based parsing first; fall back to the old line scanner on failure.
    if let Ok(doc) = toml::from_str::<toml::Value>(content) {
        return parse_cargo_workspace_members_toml(root, &doc);
    }
    parse_cargo_workspace_members_fallback(root, content)
}

fn parse_cargo_workspace_members_toml(root: &Path, doc: &toml::Value) -> Vec<(String, ProjectInfo)> {
    let workspace = match doc.get("workspace") {
        Some(w) => w,
        None => return Vec::new(),
    };

    let member_patterns: Vec<&str> = workspace
        .get("members")
        .and_then(|m| m.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let exclude_patterns: Vec<&str> = workspace
        .get("exclude")
        .and_then(|e| e.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut seen = std::collections::BTreeSet::new();
    let mut members: Vec<(String, ProjectInfo)> = Vec::new();

    // Expand member patterns, respecting exclusions and de-duplicating.
    for pattern in &member_patterns {
        let candidates = if pattern.contains('*') {
            let fake = format!("- {}", pattern);
            expand_glob_patterns(root, &fake)
        } else {
            let full = root.join(pattern);
            if full.is_dir() {
                detect_project(&full).ok()
                    .map(|info| (pattern.to_string(), info))
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            }
        };

        for (rel, info) in candidates {
            // Apply exclude list: skip if this relative path matches any exclude pattern.
            let excluded = exclude_patterns.iter().any(|ex| {
                if ex.contains('*') {
                    // Minimal prefix glob: "crates/*" excludes "crates/foo"
                    if let Some(prefix) = ex.strip_suffix("/*") {
                        rel == *ex || rel.starts_with(&format!("{}/", prefix))
                    } else {
                        rel == *ex
                    }
                } else {
                    rel == *ex
                }
            });
            if excluded { continue; }

            if seen.insert(rel.clone()) {
                members.push((rel, info));
            }
        }
    }

    // If the root Cargo.toml also has a [package] section, it is itself a
    // workspace member. Cargo treats it as such; include it as path ".".
    if doc.get("package").is_some() && seen.insert(".".to_string()) {
        if let Ok(root_info) = detect_project(root) {
            members.push((".".to_string(), root_info));
        }
    }

    members
}

/// Line-scanner fallback for unparseable TOML (preserves old behavior).
fn parse_cargo_workspace_members_fallback(root: &Path, content: &str) -> Vec<(String, ProjectInfo)> {
    let mut members = Vec::new();
    let mut in_members = false;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("members") && t.contains('=') { in_members = true; }
        if !in_members { continue; }

        let mut s = t;
        while let Some(start) = s.find('"') {
            s = &s[start + 1..];
            if let Some(end) = s.find('"') {
                let rel = &s[..end];
                if !rel.is_empty() {
                    if rel.contains('*') {
                        let fake = format!("- {}", rel);
                        members.extend(expand_glob_patterns(root, &fake));
                    } else {
                        let full = root.join(rel);
                        if full.is_dir() {
                            if let Ok(info) = detect_project(&full) {
                                members.push((rel.to_string(), info));
                            }
                        }
                    }
                }
                s = &s[end + 1..];
            } else { break; }
        }
        if t.contains(']') { break; }
    }
    members
}

/// Expand `packages/*` style patterns. Returns `(relative_path, ProjectInfo)` pairs.
fn expand_glob_patterns(root: &Path, content: &str) -> Vec<(String, ProjectInfo)> {
    let mut members = Vec::new();
    for line in content.lines() {
        let pat = line.trim().trim_start_matches('-').trim().trim_matches('"').trim_matches('\'');
        if pat.is_empty() || pat.starts_with('#') { continue; }

        if let Some(prefix) = pat.strip_suffix("/*") {
            let dir = root.join(prefix);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
                paths.sort();
                for path in paths {
                    if path.is_dir() {
                        if let Ok(info) = detect_project(&path) {
                            let rel = format!("{}/{}", prefix, path.file_name()
                                .and_then(|n| n.to_str()).unwrap_or(""));
                            members.push((rel, info));
                        }
                    }
                }
            }
        } else {
            let full = root.join(pat);
            if full.is_dir() {
                if let Ok(info) = detect_project(&full) {
                    members.push((pat.to_string(), info));
                }
            }
        }
    }
    members
}

/// Extract list items from the top-level `packages:` section of pnpm-workspace.yaml.
/// Stops collecting when another top-level key (no leading whitespace, ends with `:`)
/// begins, so values in other sections (ignoredBuiltDependencies, catalogs, …) are
/// never mistaken for workspace members.
fn parse_yaml_list_items(content: &str) -> Vec<String> {
    let mut in_packages = false;
    let mut result = Vec::new();
    for line in content.lines() {
        // Top-level key: no leading whitespace, non-empty, ends with ':'
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let trimmed = line.trim();
            in_packages = trimmed == "packages:";
            continue;
        }
        if !in_packages { continue; }
        let s = line.trim().trim_start_matches('-').trim()
            .trim_matches('"').trim_matches('\'');
        if s.is_empty() || s.starts_with('#') { continue; }
        result.push(s.to_string());
    }
    result
}

/// True if `exclude_pat` (without leading `!`) matches `rel_path`.
fn js_glob_excludes(exclude_pat: &str, rel_path: &str) -> bool {
    if let Some(prefix) = exclude_pat.strip_suffix("/*") {
        rel_path.starts_with(&format!("{}/", prefix))
    } else {
        rel_path == exclude_pat
    }
}

/// Expand a list of JS workspace patterns (include and `!`-prefixed exclude) into
/// workspace members.  Deduplicates via BTreeSet on relative path so that
/// overlapping patterns like `["packages/*", "packages/api"]` yield one entry.
fn expand_js_workspace_patterns(root: &Path, patterns: &[&str]) -> Vec<(String, ProjectInfo)> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for &p in patterns {
        if let Some(exc) = p.strip_prefix('!') {
            excludes.push(exc);
        } else {
            includes.push(p);
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut members = Vec::new();

    for pat in includes {
        if let Some(prefix) = pat.strip_suffix("/*") {
            let dir = root.join(prefix);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
                paths.sort();
                for path in paths {
                    if !path.is_dir() { continue; }
                    let rel = format!("{}/{}", prefix, path.file_name()
                        .and_then(|n| n.to_str()).unwrap_or(""));
                    if excludes.iter().any(|ex| js_glob_excludes(ex, &rel)) { continue; }
                    if seen.insert(rel.clone()) {
                        if let Ok(info) = detect_project(&path) {
                            members.push((rel, info));
                        }
                    }
                }
            }
        } else {
            let full = root.join(pat);
            if full.is_dir() {
                if excludes.iter().any(|ex| js_glob_excludes(ex, pat)) { continue; }
                if seen.insert(pat.to_string()) {
                    if let Ok(info) = detect_project(&full) {
                        members.push((pat.to_string(), info));
                    }
                }
            }
        }
    }
    members
}

fn parse_npm_workspace_members(root: &Path, pkg_json: &str) -> Vec<(String, ProjectInfo)> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(pkg_json) {
        let raw = json.get("workspaces")
            .and_then(|w| {
                if let Some(arr) = w.as_array() { Some(arr.clone()) }
                else { w.get("packages").and_then(|p| p.as_array()).cloned() }
            })
            .unwrap_or_default();
        let patterns: Vec<&str> = raw.iter().filter_map(|p| p.as_str()).collect();
        return expand_js_workspace_patterns(root, &patterns);
    }
    vec![]
}

fn parse_lerna_members(root: &Path, lerna_json: &str) -> Vec<(String, ProjectInfo)> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(lerna_json) {
        if let Some(pats) = json.get("packages").and_then(|p| p.as_array()) {
            let patterns: Vec<&str> = pats.iter().filter_map(|p| p.as_str()).collect();
            return expand_js_workspace_patterns(root, &patterns);
        }
    }
    vec![]
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
            workspace_root: None,
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
            workspace_root: None,
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
            workspace_root: None,
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
            workspace_root: None,
        });
    }

    Ok(ProjectInfo {
        language: Language::Unknown,
        root: root_str,
        has_tests: false,
        package_name: None,
        frameworks: ProjectFrameworks::default(),
        workspace_root: None,
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

    // ── Workspace detection ───────────────────────────────────────────────────

    #[test]
    fn single_rust_project_returns_single() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"myapp\"").unwrap();
        let ws = detect_workspace(dir.path()).unwrap();
        assert!(matches!(ws, WorkspaceInfo::Single(_)));
    }

    #[test]
    fn cargo_workspace_with_members_returns_multi() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n").unwrap();
        let a = dir.path().join("crates/a");
        let b = dir.path().join("crates/b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("Cargo.toml"), b"[package]\nname=\"a\"").unwrap();
        fs::write(b.join("Cargo.toml"), b"[package]\nname=\"b\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { kind, members, .. } => {
                assert_eq!(kind, WorkspaceKind::Cargo);
                assert_eq!(members.len(), 2);
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"crates/a"));
                assert!(paths.contains(&"crates/b"));
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn pnpm_workspace_returns_multi() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pnpm-workspace.yaml"), b"packages:\n  - 'apps/*'\n").unwrap();
        let apps = dir.path().join("apps/web");
        fs::create_dir_all(&apps).unwrap();
        fs::write(apps.join("package.json"), br#"{"name":"web"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { kind, members, .. } => {
                assert_eq!(kind, WorkspaceKind::Pnpm);
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].0, "apps/web");
                assert_eq!(members[0].1.language, Language::TypeScript);
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn npm_workspace_returns_multi() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"),
            br#"{"name":"root","workspaces":["packages/*"]}"#).unwrap();
        let pkg = dir.path().join("packages/ui");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("package.json"), br#"{"name":"ui"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { kind, members, .. } => {
                assert_eq!(kind, WorkspaceKind::Npm);
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].0, "packages/ui");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn lerna_workspace_returns_multi() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("lerna.json"),
            br#"{"packages":["packages/*"]}"#).unwrap();
        let pkg = dir.path().join("packages/core");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("package.json"), br#"{"name":"core"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { kind, members, .. } => {
                assert_eq!(kind, WorkspaceKind::Lerna);
                assert_eq!(members.len(), 1);
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn empty_workspace_falls_back_to_single() {
        let dir = tempdir().unwrap();
        // Cargo.toml with [workspace] but no member dirs exist
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"nonexistent\"]\n").unwrap();
        let ws = detect_workspace(dir.path()).unwrap();
        // No members found → Single
        assert!(matches!(ws, WorkspaceInfo::Single(_)));
    }

    #[test]
    fn workspace_member_language_detected_correctly() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pnpm-workspace.yaml"), b"packages:\n  - 'apps/*'\n").unwrap();
        // Python member
        let py = dir.path().join("apps/api");
        fs::create_dir_all(&py).unwrap();
        fs::write(py.join("pyproject.toml"), b"[project]\nname = 'api'").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].1.language, Language::Python);
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn cargo_workspace_with_glob_members_returns_multi() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"crates/*\"]\n").unwrap();
        let a = dir.path().join("crates/alpha");
        let b = dir.path().join("crates/beta");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("Cargo.toml"), b"[package]\nname=\"alpha\"").unwrap();
        fs::write(b.join("Cargo.toml"), b"[package]\nname=\"beta\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { kind, members, .. } => {
                assert_eq!(kind, WorkspaceKind::Cargo);
                assert_eq!(members.len(), 2);
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.iter().any(|p| p.ends_with("alpha")));
                assert!(paths.iter().any(|p| p.ends_with("beta")));
            }
            _ => panic!("Expected Multi from cargo glob workspace"),
        }
    }

    #[test]
    fn turbo_workspace_returns_turbo_kind() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"),
            br#"{"workspaces":["apps/*"]}"#).unwrap();
        fs::write(dir.path().join("turbo.json"), b"{}").unwrap();
        let apps = dir.path().join("apps/web");
        fs::create_dir_all(&apps).unwrap();
        fs::write(apps.join("package.json"), br#"{"name":"web"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        assert!(matches!(ws, WorkspaceInfo::Multi { kind: WorkspaceKind::Turbo, .. }));
    }

    #[test]
    fn cargo_workspace_members_have_workspace_root_set() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n").unwrap();
        let a = dir.path().join("crates/a");
        let b = dir.path().join("crates/b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("Cargo.toml"), b"[package]\nname=\"a\"").unwrap();
        fs::write(b.join("Cargo.toml"), b"[package]\nname=\"b\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                for (_, info) in &members {
                    assert!(info.workspace_root.is_some(),
                        "each Cargo workspace member must have workspace_root set");
                    let ws_root = info.workspace_root.as_deref().unwrap();
                    let ws_root_path = std::path::Path::new(ws_root);
                    assert!(ws_root_path.join("Cargo.toml").exists(),
                        "workspace_root must point to the directory containing the workspace Cargo.toml");
                }
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn pnpm_workspace_members_have_workspace_root_set() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pnpm-workspace.yaml"), b"packages:\n  - 'apps/*'\n").unwrap();
        let app = dir.path().join("apps/web");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("package.json"), br#"{"name":"web"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let (_, info) = &members[0];
                assert!(info.workspace_root.is_some());
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn single_project_workspace_root_is_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"solo\"").unwrap();
        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Single(info) => {
                assert!(info.workspace_root.is_none(),
                    "single-project workspace_root must be None");
            }
            _ => panic!("Expected Single"),
        }
    }

    // ── Cargo workspace hardening ─────────────────────────────────────────────

    #[test]
    fn cargo_workspace_with_package_and_workspace_includes_root_as_dot() {
        // Root Cargo.toml has both [package] and [workspace]: root is itself a member.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[package]\nname=\"root-crate\"\n[workspace]\nmembers = [\"crates/a\"]\n").unwrap();
        let a = dir.path().join("crates/a");
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("Cargo.toml"), b"[package]\nname=\"a\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"."), "root package must be included as \".\"");
                assert!(paths.contains(&"crates/a"), "declared member must still be present");
                assert_eq!(members.len(), 2);
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn cargo_workspace_exclude_removes_matching_members() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/experimental\"]\n").unwrap();
        for name in &["api", "core", "experimental"] {
            let p = dir.path().join("crates").join(name);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("Cargo.toml"), format!("[package]\nname=\"{}\"", name).as_bytes()).unwrap();
        }

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(!paths.iter().any(|p| p.ends_with("experimental")),
                    "excluded member must not appear");
                assert!(paths.iter().any(|p| p.ends_with("api")),
                    "non-excluded member must be present");
                assert!(paths.iter().any(|p| p.ends_with("core")),
                    "non-excluded member must be present");
                assert_eq!(members.len(), 2);
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn cargo_workspace_deduplicates_overlapping_patterns() {
        // "crates/*" and "crates/api" both match crates/api — should appear only once.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[workspace]\nmembers = [\"crates/*\", \"crates/api\"]\n").unwrap();
        let api = dir.path().join("crates/api");
        let lib = dir.path().join("crates/lib");
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&lib).unwrap();
        fs::write(api.join("Cargo.toml"), b"[package]\nname=\"api\"").unwrap();
        fs::write(lib.join("Cargo.toml"), b"[package]\nname=\"lib\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let api_count = members.iter().filter(|(p, _)| p.ends_with("api")).count();
                assert_eq!(api_count, 1, "crates/api must appear exactly once despite duplicate patterns");
                assert_eq!(members.len(), 2, "total members must be 2 (api + lib)");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn cargo_workspace_root_member_has_workspace_root_stamped() {
        // The "." root member must also get workspace_root set.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"),
            b"[package]\nname=\"root-crate\"\n[workspace]\nmembers = [\"crates/a\"]\n").unwrap();
        let a = dir.path().join("crates/a");
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("Cargo.toml"), b"[package]\nname=\"a\"").unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let root_member = members.iter().find(|(p, _)| p == ".");
                assert!(root_member.is_some(), "root member must be present");
                let (_, info) = root_member.unwrap();
                assert!(info.workspace_root.is_some(),
                    "root member must have workspace_root stamped");
            }
            _ => panic!("Expected Multi"),
        }
    }

    // ── JS workspace exclusion / dedup ────────────────────────────────────────

    #[test]
    fn pnpm_workspace_exclude_removes_matching_member() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pnpm-workspace.yaml"),
            b"packages:\n  - 'packages/*'\n  - '!packages/experimental'\n").unwrap();
        let keep = dir.path().join("packages/core");
        let exclude = dir.path().join("packages/experimental");
        fs::create_dir_all(&keep).unwrap();
        fs::create_dir_all(&exclude).unwrap();
        fs::write(keep.join("package.json"), br#"{"name":"core"}"#).unwrap();
        fs::write(exclude.join("package.json"), br#"{"name":"experimental"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"packages/core"), "core must be included");
                assert!(!paths.contains(&"packages/experimental"), "experimental must be excluded");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn lerna_exclude_removes_matching_member() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("lerna.json"),
            br#"{"packages":["packages/*","!packages/legacy"]}"#).unwrap();
        let keep = dir.path().join("packages/api");
        let exclude = dir.path().join("packages/legacy");
        fs::create_dir_all(&keep).unwrap();
        fs::create_dir_all(&exclude).unwrap();
        fs::write(keep.join("package.json"), br#"{"name":"api"}"#).unwrap();
        fs::write(exclude.join("package.json"), br#"{"name":"legacy"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"packages/api"), "api must be included");
                assert!(!paths.contains(&"packages/legacy"), "legacy must be excluded");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn npm_workspaces_exclude_removes_matching_member() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"),
            br#"{"name":"root","workspaces":["apps/*","!apps/legacy"]}"#).unwrap();
        let keep = dir.path().join("apps/web");
        let exclude = dir.path().join("apps/legacy");
        fs::create_dir_all(&keep).unwrap();
        fs::create_dir_all(&exclude).unwrap();
        fs::write(keep.join("package.json"), br#"{"name":"web"}"#).unwrap();
        fs::write(exclude.join("package.json"), br#"{"name":"legacy"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"apps/web"), "web must be included");
                assert!(!paths.contains(&"apps/legacy"), "legacy must be excluded");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn js_workspace_deduplicates_overlapping_patterns() {
        let dir = tempdir().unwrap();
        // "packages/*" expands packages/api; "packages/api" names it again explicitly.
        fs::write(dir.path().join("package.json"),
            br#"{"name":"root","workspaces":["packages/*","packages/api"]}"#).unwrap();
        let api = dir.path().join("packages/api");
        let other = dir.path().join("packages/utils");
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(api.join("package.json"), br#"{"name":"api"}"#).unwrap();
        fs::write(other.join("package.json"), br#"{"name":"utils"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let api_count = members.iter().filter(|(p, _)| p == "packages/api").count();
                assert_eq!(api_count, 1, "packages/api must appear exactly once despite two matching patterns");
                assert_eq!(members.len(), 2, "total members must be 2 (api + utils)");
            }
            _ => panic!("Expected Multi"),
        }
    }

    #[test]
    fn pnpm_other_yaml_sections_not_treated_as_workspace_members() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pnpm-workspace.yaml"),
            b"packages:\n  - \"packages/*\"\nignoredBuiltDependencies:\n  - \"tools/not-a-workspace\"\n").unwrap();
        let member = dir.path().join("packages/core");
        let false_positive = dir.path().join("tools/not-a-workspace");
        fs::create_dir_all(&member).unwrap();
        fs::create_dir_all(&false_positive).unwrap();
        fs::write(member.join("package.json"), br#"{"name":"core"}"#).unwrap();
        fs::write(false_positive.join("package.json"), br#"{"name":"not-a-member"}"#).unwrap();

        let ws = detect_workspace(dir.path()).unwrap();
        match ws {
            WorkspaceInfo::Multi { members, .. } => {
                let paths: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).collect();
                assert!(paths.contains(&"packages/core"), "packages/core must be detected");
                assert!(!paths.contains(&"tools/not-a-workspace"),
                    "tools/not-a-workspace must not be detected — it is in ignoredBuiltDependencies, not packages");
            }
            _ => panic!("Expected Multi"),
        }
    }
}
