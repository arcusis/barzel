use crate::detect::Language;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Content-based hash of the project's source files, scoped by language.
pub fn compute_project_hash(project_root: &Path, language: Language) -> u64 {
    let mut hasher = DefaultHasher::new();
    match language {
        Language::Rust => hash_rust(project_root, &mut hasher),
        Language::TypeScript => hash_typescript(project_root, &mut hasher),
        Language::Python => hash_python(project_root, &mut hasher),
        Language::Go => hash_go(project_root, &mut hasher),
        Language::Unknown => hash_fallback(project_root, &mut hasher),
    }
    hasher.finish()
}

fn hash_rust(root: &Path, hasher: &mut DefaultHasher) {
    hash_file_content(&root.join("Cargo.toml"), hasher);
    hash_file_content(&root.join("Cargo.lock"), hasher);
    hash_dir_if_exists(&root.join("src"), hasher);
    hash_dir_if_exists(&root.join("tests"), hasher);
}

fn hash_typescript(root: &Path, hasher: &mut DefaultHasher) {
    hash_file_content(&root.join("package.json"), hasher);
    hash_file_content(&root.join("package-lock.json"), hasher);
    hash_file_content(&root.join("pnpm-lock.yaml"), hasher);
    hash_file_content(&root.join("yarn.lock"), hasher);
    for dir in &["src", "app", "pages", "components", "tests", "__tests__"] {
        hash_dir_if_exists(&root.join(dir), hasher);
    }
}

fn hash_python(root: &Path, hasher: &mut DefaultHasher) {
    hash_file_content(&root.join("pyproject.toml"), hasher);
    // requirements*.txt — collect and sort for determinism
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut req_files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("requirements") && n.ends_with(".txt"))
                    .unwrap_or(false)
            })
            .collect();
        req_files.sort();
        for f in req_files {
            hash_file_content(&f, hasher);
        }
    }
    hash_dir_if_exists(&root.join("src"), hasher);
    hash_dir_if_exists(&root.join("tests"), hasher);
    // Hash top-level Python package directories (dirs containing __init__.py).
    // Excludes common non-package dirs to keep hashing focused on source.
    hash_python_packages(root, hasher);
}

fn hash_python_packages(root: &Path, hasher: &mut DefaultHasher) {
    let Ok(mut entries) = std::fs::read_dir(root) else { return; };
    let mut dirs: Vec<_> = entries
        .by_ref()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !matches!(name, "src" | "tests" | "dist" | "build" | ".venv" | "venv"
                | "__pycache__" | ".git" | ".mypy_cache" | ".pytest_cache" | ".tox")
                && !name.starts_with('.')
        })
        .filter(|p| p.join("__init__.py").exists())
        .collect();
    dirs.sort();
    for dir in dirs {
        hash_python_package_recursive(&dir, hasher);
    }
}

/// Recursively hash Python source files inside a package directory.
/// Skips generated/cache directories at any depth so that bytecode and tool
/// caches do not affect the structural cache key.
fn hash_python_package_recursive(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(mut entries) = std::fs::read_dir(dir) else { return; };
    let mut paths: Vec<_> = entries.by_ref().flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if matches!(name, "__pycache__" | ".mypy_cache" | ".pytest_cache"
                | ".tox" | ".venv" | "venv" | "build" | "dist")
                || name.starts_with('.')
            {
                continue;
            }
            hash_python_package_recursive(&path, hasher);
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            hash_file_content(&path, hasher);
        }
    }
}

fn hash_go(root: &Path, hasher: &mut DefaultHasher) {
    hash_file_content(&root.join("go.mod"), hasher);
    hash_file_content(&root.join("go.sum"), hasher);
    hash_go_files_recursive(root, hasher);
}

fn hash_go_files_recursive(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.by_ref().flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "vendor" {
            continue;
        }
        if path.is_dir() {
            hash_go_files_recursive(&path, hasher);
        } else if path.extension().and_then(|e| e.to_str()) == Some("go") {
            hash_file_content(&path, hasher);
        }
    }
}

fn hash_fallback(root: &Path, hasher: &mut DefaultHasher) {
    hash_shallow(root, 0, 2, hasher);
}

fn hash_shallow(dir: &Path, depth: u8, max_depth: u8, hasher: &mut DefaultHasher) {
    if depth > max_depth {
        return;
    }
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.by_ref().flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        // Skip common build/output directories
        if matches!(name, "target" | "node_modules" | "dist" | "build" | "__pycache__") {
            continue;
        }
        if path.is_dir() {
            hash_shallow(&path, depth + 1, max_depth, hasher);
        } else {
            hash_file_content(&path, hasher);
        }
    }
}

fn hash_file_content(path: &Path, hasher: &mut DefaultHasher) {
    if let Ok(content) = std::fs::read(path) {
        content.hash(hasher);
        path.to_string_lossy().hash(hasher);
    }
}

fn hash_dir_if_exists(dir: &Path, hasher: &mut DefaultHasher) {
    if dir.exists() {
        hash_dir_recursive(dir, hasher);
    }
}

fn hash_dir_recursive(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut paths: Vec<_> = entries
        .by_ref()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();

    for path in paths {
        if path.is_dir() {
            hash_dir_recursive(&path, hasher);
        } else {
            hash_file_content(&path, hasher);
        }
    }
}

/// Returns the stored hash for a runner, if it exists.
pub fn load_runner_hash(project_root: &Path, language: Language, runner_name: &str) -> Option<u64> {
    let cache_file = cache_path(project_root, language, runner_name);
    let content = std::fs::read_to_string(cache_file).ok()?;
    content.trim().parse().ok()
}

/// Persists a specific hash for a runner.
pub fn save_runner_hash(project_root: &Path, language: Language, runner_name: &str, hash: u64) {
    let cache_dir = project_root
        .join(".barzel")
        .join("cache")
        .join(language.to_string());
    if std::fs::create_dir_all(&cache_dir).is_ok() {
        let cache_file = cache_dir.join(format!("{}.hash", sanitize(runner_name)));
        let _ = std::fs::write(cache_file, hash.to_string());
    }
}

/// Computes the current project hash and saves it for a runner.
pub fn save_current_hash(project_root: &Path, language: Language, runner_name: &str) {
    let hash = compute_project_hash(project_root, language);
    save_runner_hash(project_root, language, runner_name, hash);
}

/// Returns true when the project is unchanged since the last cached run.
pub fn is_cached(project_root: &Path, language: Language, runner_name: &str) -> bool {
    let current = compute_project_hash(project_root, language);
    load_runner_hash(project_root, language, runner_name)
        .map(|stored| stored == current)
        .unwrap_or(false)
}

fn cache_path(project_root: &Path, language: Language, runner_name: &str) -> std::path::PathBuf {
    project_root
        .join(".barzel")
        .join("cache")
        .join(language.to_string())
        .join(format!("{}.hash", sanitize(runner_name)))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    // ── sanitize ──────────────────────────────────────────────────────────────

    #[test]
    fn sanitize_replaces_slash() {
        let result = sanitize("cargo/mutants");
        assert!(!result.contains('/'));
        assert_eq!(result, "cargo_mutants");
    }

    #[test]
    fn sanitize_keeps_hyphens() {
        assert_eq!(sanitize("cargo-mutants"), "cargo-mutants");
    }

    #[test]
    fn sanitize_empty_stays_empty() {
        assert_eq!(sanitize(""), "");
    }

    // ── hash ─────────────────────────────────────────────────────────────────

    #[test]
    fn hash_changes_when_file_content_changes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '1'").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Rust);
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '2'").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Rust);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_when_src_file_added() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Rust);
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("main.rs"), b"fn main() {}").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Rust);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_typescript_uses_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), br#"{"name":"a"}"#).unwrap();
        let h1 = compute_project_hash(dir.path(), Language::TypeScript);
        std::fs::write(dir.path().join("package.json"), br#"{"name":"b"}"#).unwrap();
        let h2 = compute_project_hash(dir.path(), Language::TypeScript);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_python_uses_pyproject_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname = 'x'").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Python);
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname = 'y'").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Python);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_go_uses_go_mod() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), b"module example.com/a\n").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Go);
        std::fs::write(dir.path().join("go.mod"), b"module example.com/b\n").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Go);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_unknown_fallback_is_deterministic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"hello").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Unknown);
        let h2 = compute_project_hash(dir.path(), Language::Unknown);
        assert_eq!(h1, h2);
    }

    #[test]
    fn rust_and_typescript_hashes_differ_for_same_root() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        std::fs::write(dir.path().join("package.json"), br#"{"name":"x"}"#).unwrap();
        let h_rust = compute_project_hash(dir.path(), Language::Rust);
        let h_ts = compute_project_hash(dir.path(), Language::TypeScript);
        assert_ne!(h_rust, h_ts);
    }

    // ── test/source directory invalidation ───────────────────────────────────

    #[test]
    fn rust_tests_dir_change_invalidates_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Rust);

        let tests_dir = dir.path().join("tests");
        std::fs::create_dir(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("integration_test.rs"), b"#[test] fn it_works() {}").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Rust);

        assert_ne!(h1, h2, "adding a file under tests/ must change the Rust hash");
    }

    #[test]
    fn typescript_tests_dir_change_invalidates_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), br#"{"name":"x"}"#).unwrap();
        let h1 = compute_project_hash(dir.path(), Language::TypeScript);

        let tests_dir = dir.path().join("tests");
        std::fs::create_dir(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("app.test.ts"), b"test('ok', () => {})").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::TypeScript);

        assert_ne!(h1, h2, "adding a file under tests/ must change the TypeScript hash");
    }

    #[test]
    fn typescript_double_underscore_tests_dir_change_invalidates_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), br#"{"name":"x"}"#).unwrap();
        let h1 = compute_project_hash(dir.path(), Language::TypeScript);

        let tests_dir = dir.path().join("__tests__");
        std::fs::create_dir(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("app.test.ts"), b"test('ok', () => {})").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::TypeScript);

        assert_ne!(h1, h2, "adding a file under __tests__/ must change the TypeScript hash");
    }

    #[test]
    fn python_top_level_package_dir_change_invalidates_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='x'").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Python);

        // Create a top-level Python package dir (has __init__.py)
        let pkg = dir.path().join("mypackage");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), b"").unwrap();
        std::fs::write(pkg.join("core.py"), b"def run(): pass").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Python);
        assert_ne!(h1, h2, "creating a top-level package dir must change the Python hash");

        // Changing a file in that package must also invalidate
        std::fs::write(pkg.join("core.py"), b"def run(): return 42").unwrap();
        let h3 = compute_project_hash(dir.path(), Language::Python);
        assert_ne!(h2, h3, "modifying a file in a top-level package dir must change the Python hash");
    }

    #[test]
    fn go_nested_file_change_invalidates_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), b"module example.com/m\n").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Go);

        let sub = dir.path().join("internal").join("util");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("helper.go"), b"package util").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Go);
        assert_ne!(h1, h2, "adding a nested .go file must change the Go hash");

        std::fs::write(sub.join("helper.go"), b"package util\nfunc F() {}").unwrap();
        let h3 = compute_project_hash(dir.path(), Language::Go);
        assert_ne!(h2, h3, "modifying a nested .go file must change the Go hash");
    }

    #[test]
    fn workspace_member_hash_is_isolated_from_sibling() {
        // Use a realistic workspace layout: workspace/crates/a and workspace/crates/b
        let ws = tempdir().unwrap();
        let crates = ws.path().join("crates");
        let member_a = crates.join("a");
        let member_b = crates.join("b");
        std::fs::create_dir_all(&member_a).unwrap();
        std::fs::create_dir_all(&member_b).unwrap();

        std::fs::write(member_a.join("Cargo.toml"), b"[package]\nname='a'").unwrap();
        std::fs::write(member_b.join("Cargo.toml"), b"[package]\nname='b'").unwrap();

        let ha1 = compute_project_hash(&member_a, Language::Rust);
        let hb1 = compute_project_hash(&member_b, Language::Rust);

        // Modify member_b; member_a's hash must not change
        std::fs::write(member_b.join("Cargo.toml"), b"[package]\nname='b-changed'").unwrap();
        let ha2 = compute_project_hash(&member_a, Language::Rust);
        let hb2 = compute_project_hash(&member_b, Language::Rust);

        assert_eq!(ha1, ha2, "member_a hash must not change when sibling member_b changes");
        assert_ne!(hb1, hb2, "member_b hash must change after its own file changes");
    }

    #[test]
    fn python_pycache_dir_does_not_affect_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), b"[project]\nname='x'").unwrap();

        let pkg = dir.path().join("mypackage");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), b"").unwrap();
        std::fs::write(pkg.join("core.py"), b"def run(): pass").unwrap();
        let h1 = compute_project_hash(dir.path(), Language::Python);

        // Add a __pycache__ directory with bytecode — must not change the hash
        let pycache = pkg.join("__pycache__");
        std::fs::create_dir(&pycache).unwrap();
        std::fs::write(pycache.join("core.cpython-311.pyc"), b"\x00\x00bytecode").unwrap();
        let h2 = compute_project_hash(dir.path(), Language::Python);

        assert_eq!(h1, h2, "__pycache__ contents must not affect the Python structural cache hash");
    }

    // ── save / load round-trip ────────────────────────────────────────────────

    #[test]
    fn save_and_load_hash_round_trips() {
        let dir = tempdir().unwrap();
        save_runner_hash(dir.path(), Language::Rust, "test-runner", 0xDEAD_BEEF);
        let loaded = load_runner_hash(dir.path(), Language::Rust, "test-runner");
        assert_eq!(loaded, Some(0xDEAD_BEEF));
    }

    #[test]
    fn load_returns_none_when_no_cache() {
        let dir = tempdir().unwrap();
        assert!(load_runner_hash(dir.path(), Language::Rust, "nonexistent").is_none());
    }

    #[test]
    fn different_runner_names_have_isolated_caches() {
        let dir = tempdir().unwrap();
        save_runner_hash(dir.path(), Language::Rust, "runner-a", 111);
        save_runner_hash(dir.path(), Language::Rust, "runner-b", 222);
        assert_eq!(load_runner_hash(dir.path(), Language::Rust, "runner-a"), Some(111));
        assert_eq!(load_runner_hash(dir.path(), Language::Rust, "runner-b"), Some(222));
    }

    #[test]
    fn different_languages_have_isolated_caches() {
        let dir = tempdir().unwrap();
        save_runner_hash(dir.path(), Language::Rust, "runner", 111);
        save_runner_hash(dir.path(), Language::TypeScript, "runner", 222);
        assert_eq!(load_runner_hash(dir.path(), Language::Rust, "runner"), Some(111));
        assert_eq!(load_runner_hash(dir.path(), Language::TypeScript, "runner"), Some(222));
    }

    // ── is_cached ────────────────────────────────────────────────────────────

    #[test]
    fn is_cached_true_when_hash_matches() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        let hash = compute_project_hash(dir.path(), Language::Rust);
        save_runner_hash(dir.path(), Language::Rust, "cargo-mutants", hash);
        assert!(is_cached(dir.path(), Language::Rust, "cargo-mutants"));
    }

    #[test]
    fn is_cached_false_when_no_stored_hash() {
        let dir = tempdir().unwrap();
        assert!(!is_cached(dir.path(), Language::Rust, "cargo-mutants"));
    }

    #[test]
    fn is_cached_false_after_file_changes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '1'").unwrap();
        // Save current hash
        save_current_hash(dir.path(), Language::Rust, "runner");
        // Modify the file
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '2'").unwrap();
        // Cache should be stale
        assert!(!is_cached(dir.path(), Language::Rust, "runner"));
    }

    #[test]
    fn save_current_hash_then_is_cached_true() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        save_current_hash(dir.path(), Language::Rust, "runner");
        assert!(is_cached(dir.path(), Language::Rust, "runner"));
    }

    // ── proptest ─────────────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn sanitize_only_safe_chars(s in ".*") {
            let result = sanitize(&s);
            prop_assert!(result.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
        }

        #[test]
        fn sanitize_preserves_length(s in ".*") {
            prop_assert_eq!(sanitize(&s).chars().count(), s.chars().count());
        }

        #[test]
        fn hash_is_deterministic(dir_seed in 0u8..=255u8) {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("Cargo.toml"), vec![dir_seed; 64]).unwrap();
            let h1 = compute_project_hash(dir.path(), Language::Rust);
            let h2 = compute_project_hash(dir.path(), Language::Rust);
            prop_assert_eq!(h1, h2);
        }

        #[test]
        fn different_content_different_hash(a in 0u8..=127u8, b in 128u8..=255u8) {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("Cargo.toml"), vec![a; 32]).unwrap();
            let h1 = compute_project_hash(dir.path(), Language::Rust);
            std::fs::write(dir.path().join("Cargo.toml"), vec![b; 32]).unwrap();
            let h2 = compute_project_hash(dir.path(), Language::Rust);
            prop_assert_ne!(h1, h2);
        }

        #[test]
        fn save_load_round_trip(hash in 0u64..u64::MAX, name in "[a-z][a-z0-9-]{0,10}") {
            let dir = tempdir().unwrap();
            save_runner_hash(dir.path(), Language::Rust, &name, hash);
            prop_assert_eq!(load_runner_hash(dir.path(), Language::Rust, &name), Some(hash));
        }
    }
}
