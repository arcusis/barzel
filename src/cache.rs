use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Content-based hash of the project's source files.
/// Walks src/ recursively and hashes file contents + Cargo.toml.
pub fn compute_project_hash(project_root: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();

    hash_file_content(&project_root.join("Cargo.toml"), &mut hasher);

    let src_dir = project_root.join("src");
    if src_dir.exists() {
        hash_dir_recursive(&src_dir, &mut hasher);
    }

    hasher.finish()
}

fn hash_file_content(path: &Path, hasher: &mut DefaultHasher) {
    if let Ok(content) = std::fs::read(path) {
        content.hash(hasher);
        path.to_string_lossy().hash(hasher);
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
pub fn load_runner_hash(project_root: &Path, runner_name: &str) -> Option<u64> {
    let cache_file = cache_path(project_root, runner_name);
    let content = std::fs::read_to_string(cache_file).ok()?;
    content.trim().parse().ok()
}

/// Persists a specific hash for a runner.
pub fn save_runner_hash(project_root: &Path, runner_name: &str, hash: u64) {
    let cache_dir = project_root.join(".barzel").join("cache");
    if std::fs::create_dir_all(&cache_dir).is_ok() {
        let cache_file = cache_dir.join(format!("{}.hash", sanitize(runner_name)));
        let _ = std::fs::write(cache_file, hash.to_string());
    }
}

/// Computes the current project hash and saves it for a runner.
pub fn save_current_hash(project_root: &Path, runner_name: &str) {
    let hash = compute_project_hash(project_root);
    save_runner_hash(project_root, runner_name, hash);
}

/// Returns true when the project is unchanged since the last cached run.
pub fn is_cached(project_root: &Path, runner_name: &str) -> bool {
    let current = compute_project_hash(project_root);
    load_runner_hash(project_root, runner_name)
        .map(|stored| stored == current)
        .unwrap_or(false)
}

fn cache_path(project_root: &Path, runner_name: &str) -> std::path::PathBuf {
    project_root
        .join(".barzel")
        .join("cache")
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
        let h1 = compute_project_hash(dir.path());
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '2'").unwrap();
        let h2 = compute_project_hash(dir.path());
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_when_src_file_added() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        let h1 = compute_project_hash(dir.path());
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("main.rs"), b"fn main() {}").unwrap();
        let h2 = compute_project_hash(dir.path());
        assert_ne!(h1, h2);
    }

    // ── save / load round-trip ────────────────────────────────────────────────

    #[test]
    fn save_and_load_hash_round_trips() {
        let dir = tempdir().unwrap();
        save_runner_hash(dir.path(), "test-runner", 0xDEAD_BEEF);
        let loaded = load_runner_hash(dir.path(), "test-runner");
        assert_eq!(loaded, Some(0xDEAD_BEEF));
    }

    #[test]
    fn load_returns_none_when_no_cache() {
        let dir = tempdir().unwrap();
        assert!(load_runner_hash(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn different_runner_names_have_isolated_caches() {
        let dir = tempdir().unwrap();
        save_runner_hash(dir.path(), "runner-a", 111);
        save_runner_hash(dir.path(), "runner-b", 222);
        assert_eq!(load_runner_hash(dir.path(), "runner-a"), Some(111));
        assert_eq!(load_runner_hash(dir.path(), "runner-b"), Some(222));
    }

    // ── is_cached ────────────────────────────────────────────────────────────

    #[test]
    fn is_cached_true_when_hash_matches() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        let hash = compute_project_hash(dir.path());
        save_runner_hash(dir.path(), "cargo-mutants", hash);
        assert!(is_cached(dir.path(), "cargo-mutants"));
    }

    #[test]
    fn is_cached_false_when_no_stored_hash() {
        let dir = tempdir().unwrap();
        assert!(!is_cached(dir.path(), "cargo-mutants"));
    }

    #[test]
    fn is_cached_false_after_file_changes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '1'").unwrap();
        // Save current hash
        save_current_hash(dir.path(), "runner");
        // Modify the file
        std::fs::write(dir.path().join("Cargo.toml"), b"version = '2'").unwrap();
        // Cache should be stale
        assert!(!is_cached(dir.path(), "runner"));
    }

    #[test]
    fn save_current_hash_then_is_cached_true() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").unwrap();
        save_current_hash(dir.path(), "runner");
        assert!(is_cached(dir.path(), "runner"));
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
            let h1 = compute_project_hash(dir.path());
            let h2 = compute_project_hash(dir.path());
            prop_assert_eq!(h1, h2);
        }

        #[test]
        fn different_content_different_hash(a in 0u8..=127u8, b in 128u8..=255u8) {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("Cargo.toml"), vec![a; 32]).unwrap();
            let h1 = compute_project_hash(dir.path());
            std::fs::write(dir.path().join("Cargo.toml"), vec![b; 32]).unwrap();
            let h2 = compute_project_hash(dir.path());
            prop_assert_ne!(h1, h2);
        }

        #[test]
        fn save_load_round_trip(hash in 0u64..u64::MAX, name in "[a-z][a-z0-9-]{0,10}") {
            let dir = tempdir().unwrap();
            save_runner_hash(dir.path(), &name, hash);
            prop_assert_eq!(load_runner_hash(dir.path(), &name), Some(hash));
        }
    }
}
