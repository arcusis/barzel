/// Git diff helpers for `barzel run --since <rev>`.
///
/// Determines which files changed between a revision and HEAD, then maps
/// those paths to workspace members so only affected packages are verified.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Lockfile names whose change anywhere in the repo means dependency security
/// may have shifted — triggers a full-workspace run.
const LOCKFILE_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.sum",
    "requirements.txt",
    "requirements-dev.txt",
    "requirements-prod.txt",
    "poetry.lock",
    "Pipfile.lock",
];

/// Root-level manifest/config files whose change forces a full workspace run
/// because they may alter which packages exist or how the build system resolves deps.
const ROOT_MANIFEST_NAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    ".barzel.toml",
    "turbo.json",
    "lerna.json",
    "pnpm-workspace.yaml",
];

pub struct DiffContext {
    /// Paths changed since the given revision, relative to `repo_root`.
    pub changed_files: Vec<PathBuf>,
    /// Absolute path to the git repository root.
    pub repo_root: PathBuf,
}

impl DiffContext {
    /// Compute changed files between `rev` and HEAD, plus any untracked files.
    /// Untracked files are included because AI agents commonly add new files before
    /// committing; excluding them would cause `--since` to incorrectly skip new packages.
    /// Returns `None` when `root` is not inside a git repo or the rev is invalid.
    pub fn since(root: &Path, rev: &str) -> Option<Self> {
        let repo_root = git_repo_root(root)?;

        let diff_out = Command::new("git")
            .args(["diff", "--name-only", rev, "--"])
            .current_dir(&repo_root)
            .output()
            .ok()?;

        if !diff_out.status.success() {
            return None;
        }

        let mut changed_files: Vec<PathBuf> = String::from_utf8_lossy(&diff_out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();

        // Also include untracked files — they are part of the working change set.
        if let Ok(untracked_out) = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(&repo_root)
            .output()
        {
            let untracked: Vec<PathBuf> = String::from_utf8_lossy(&untracked_out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect();
            changed_files.extend(untracked);
        }

        Some(DiffContext { changed_files, repo_root })
    }

    /// True if any changed file is under `project_root`.
    pub fn affects_path(&self, project_root: &Path) -> bool {
        let project_root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
        self.changed_files.iter().any(|rel| {
            let abs = self.repo_root.join(rel);
            let abs = abs.canonicalize().unwrap_or(abs);
            abs.starts_with(&project_root)
        })
    }

    /// True if any changed file is a lockfile.
    pub fn has_lockfile_changes(&self) -> bool {
        self.changed_files.iter().any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|name| LOCKFILE_NAMES.contains(&name))
                .unwrap_or(false)
        })
    }

    /// True if any root-level manifest or config file changed, meaning it is
    /// unsafe to skip any package — forces a full workspace/project run.
    pub fn forces_full_run(&self) -> bool {
        if self.has_lockfile_changes() {
            return true;
        }
        self.changed_files.iter().any(|p| {
            // Root-level manifest check: no parent component (or parent is "")
            let is_root = p.parent().map(|par| par == Path::new("")).unwrap_or(true);
            if is_root {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if ROOT_MANIFEST_NAMES.contains(&name) {
                        return true;
                    }
                }
            }

            // Shared Semgrep rule check: .barzel/rules/*.yml or .barzel/rules/*.yaml at
            // the repo root. A change here affects every package that inherits workspace
            // rules, so it is unsafe to skip any member.
            let is_root_barzel_rule = p.parent().map(|par| par == Path::new(".barzel/rules")).unwrap_or(false)
                && p.extension().and_then(|e| e.to_str()).map(|e| e == "yml" || e == "yaml").unwrap_or(false);
            is_root_barzel_rule
        })
    }

    /// Returns a human-readable summary for skip messages.
    pub fn summary(&self) -> String {
        format!("{} file(s) changed", self.changed_files.len())
    }
}

/// Run `git rev-parse --show-toplevel` to find the repo root.
/// Returns `None` if `dir` is not inside a git repository.
fn git_repo_root(dir: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() { None } else { Some(PathBuf::from(raw)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn git_init(dir: &Path) {
        Command::new("git").args(["init"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(dir).output().unwrap();
    }

    fn git_commit_all(dir: &Path, msg: &str) {
        Command::new("git").args(["add", "-A"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["commit", "-m", msg, "--allow-empty"]).current_dir(dir).output().unwrap();
    }

    #[test]
    fn non_git_dir_returns_none() {
        let dir = tempdir().unwrap();
        assert!(DiffContext::since(dir.path(), "HEAD").is_none());
    }

    #[test]
    fn invalid_rev_returns_none() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        git_commit_all(dir.path(), "init");
        assert!(DiffContext::since(dir.path(), "nonexistent-rev-xyz").is_none());
    }

    #[test]
    fn detects_changed_files_since_rev() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("a.txt"), b"first").unwrap();
        git_commit_all(dir.path(), "init");

        // Record rev after initial commit
        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        // Add a new file and commit
        fs::write(dir.path().join("b.txt"), b"second").unwrap();
        git_commit_all(dir.path(), "add b");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert_eq!(ctx.changed_files.len(), 1);
        assert_eq!(ctx.changed_files[0], PathBuf::from("b.txt"));
    }

    #[test]
    fn no_changes_since_rev_returns_empty_list() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.changed_files.is_empty());
    }

    #[test]
    fn affects_path_true_when_file_under_project_root() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        let pkg = dir.path().join("crates/foo");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("main.rs"), b"fn main() {}").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(pkg.join("lib.rs"), b"pub fn f() {}").unwrap();
        git_commit_all(dir.path(), "change foo");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.affects_path(&pkg));
        // sibling crate is not affected
        let other = dir.path().join("crates/bar");
        fs::create_dir_all(&other).unwrap();
        assert!(!ctx.affects_path(&other));
    }

    #[test]
    fn has_lockfile_changes_detected() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("app.rs"), b"fn main() {}").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(dir.path().join("Cargo.lock"), b"# lock").unwrap();
        git_commit_all(dir.path(), "update lock");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.has_lockfile_changes());
    }

    #[test]
    fn has_lockfile_changes_false_for_source_only() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("app.rs"), b"fn main() {}").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(dir.path().join("lib.rs"), b"pub fn f() {}").unwrap();
        git_commit_all(dir.path(), "add lib");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(!ctx.has_lockfile_changes());
    }

    #[test]
    fn root_cargo_toml_forces_full_run() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"a\"").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"a\"\nversion=\"2\"").unwrap();
        git_commit_all(dir.path(), "bump version");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.forces_full_run(), "root Cargo.toml change must force full run");
    }

    #[test]
    fn root_package_json_forces_full_run() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("package.json"), br#"{"name":"root"}"#).unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(dir.path().join("package.json"), br#"{"name":"root","version":"2"}"#).unwrap();
        git_commit_all(dir.path(), "bump version");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.forces_full_run(), "root package.json change must force full run");
    }

    #[test]
    fn nested_package_json_does_not_force_full_run() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        let pkg = dir.path().join("packages/a");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("package.json"), br#"{"name":"a"}"#).unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(pkg.join("package.json"), br#"{"name":"a","version":"2"}"#).unwrap();
        git_commit_all(dir.path(), "bump a");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(!ctx.forces_full_run(), "nested package.json must not force full run");
        // But it DOES affect the package path
        assert!(ctx.affects_path(&pkg));
    }

    #[test]
    fn root_barzel_rules_change_forces_full_run() {
        // Changing .barzel/rules/*.yml at the repo root must force a full workspace run
        // because workspace-root rules are inherited by every member package.
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("Cargo.toml"), b"[workspace]\nmembers=[\"crates/a\"]\n").unwrap();
        let rules = dir.path().join(".barzel/rules");
        fs::create_dir_all(&rules).unwrap();
        fs::write(rules.join("shared.yml"), b"rules: []").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        // Modify the shared rule file
        fs::write(rules.join("shared.yml"), b"rules: [updated]").unwrap();
        git_commit_all(dir.path(), "update shared rule");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(ctx.forces_full_run(),
            ".barzel/rules/*.yml change at repo root must force a full workspace run");
    }

    #[test]
    fn package_local_barzel_rules_do_not_force_full_run() {
        // A .barzel/rules change inside a package subtree is handled via affects_path,
        // not forces_full_run — it should only run that package, not everything.
        let dir = tempdir().unwrap();
        git_init(dir.path());
        let pkg_rules = dir.path().join("packages/api/.barzel/rules");
        fs::create_dir_all(&pkg_rules).unwrap();
        fs::write(pkg_rules.join("api-rule.yml"), b"rules: []").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        fs::write(pkg_rules.join("api-rule.yml"), b"rules: [updated]").unwrap();
        git_commit_all(dir.path(), "update package rule");

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(!ctx.forces_full_run(),
            "package-local .barzel/rules change must not force a full run (handled by affects_path)");
        // But it does affect the package path
        assert!(ctx.affects_path(&dir.path().join("packages/api")));
    }

    #[test]
    fn untracked_files_included_in_changed() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();
        git_commit_all(dir.path(), "init");

        let rev = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap().stdout
        ).unwrap().trim().to_string();

        // Write a new file but do NOT commit it (untracked)
        fs::write(dir.path().join("new_module.rs"), b"pub fn f() {}").unwrap();

        let ctx = DiffContext::since(dir.path(), &rev).unwrap();
        assert!(
            ctx.changed_files.iter().any(|p| p == &PathBuf::from("new_module.rs")),
            "untracked new_module.rs must appear in changed_files"
        );
    }
}
