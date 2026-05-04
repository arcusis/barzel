/// Shared tool registry used by both human `barzel check` and stdio `{"command":"check"}`.
///
/// A single source of truth prevents the two paths from drifting out of sync.
/// Each entry carries name, availability check args, DAP layer, install guidance,
/// and an availability mode for tools that exit non-zero even when present.
use crate::config::BarzelConfig;
use crate::detect::{Language, ProjectInfo};
use crate::process::SubprocessRunner;
use std::path::Path;

/// How to interpret the subprocess result when checking tool availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityMode {
    /// Tool is available when the subprocess exits successfully (exit code 0).
    /// Used for most tools: `cargo --version`, `pytest --version`, etc.
    ExitSuccess,
    /// Tool is available when the subprocess spawns without an I/O error,
    /// regardless of exit code. Used for tools that exit non-zero even when
    /// present (e.g. `go-mutesting --help` exits 2 because it has no subcommand).
    SpawnOk,
}

#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Display name, e.g. `"cargo audit"`. The first word is the binary name.
    pub name: &'static str,
    /// Args passed to the binary to check availability, e.g. `&["audit", "--version"]`.
    pub check_args: &'static [&'static str],
    /// DAP layer this tool serves: `"core"`, `"logic"`, `"structural"`, `"hostile"`.
    pub layer: &'static str,
    /// Human-readable install guidance, included in stdio JSON so agents can act on it.
    pub install: &'static str,
    /// How to interpret the subprocess result when probing availability.
    pub mode: AvailabilityMode,
}

impl ToolEntry {
    const fn new(
        name: &'static str,
        check_args: &'static [&'static str],
        layer: &'static str,
        install: &'static str,
    ) -> Self {
        Self { name, check_args, layer, install, mode: AvailabilityMode::ExitSuccess }
    }

    const fn spawn_ok(
        name: &'static str,
        check_args: &'static [&'static str],
        layer: &'static str,
        install: &'static str,
    ) -> Self {
        Self { name, check_args, layer, install, mode: AvailabilityMode::SpawnOk }
    }
}

/// The canonical tool list. Keep sorted by layer then name within each group.
///
/// Design notes:
/// - `name` first word is the binary to exec.
/// - npm-audit, pnpm-audit, yarn-audit are not separate binaries; `NpmAuditRunner`
///   selects between `npm audit`, `pnpm audit`, and `yarn audit` based on lockfile.
///   We check all three package managers so agents know which ones are available.
/// - Health checks are configured, not tool-based; they do not appear here.
/// - go-mutesting uses SpawnOk because `go-mutesting --help` exits non-zero.
pub const TOOL_REGISTRY: &[ToolEntry] = &[
    // ── Core runtimes ─────────────────────────────────────────────────────────
    ToolEntry::new("cargo", &["--version"], "core", "https://rustup.rs"),
    ToolEntry::new("go",    &["version"],   "core", "https://go.dev/dl"),
    ToolEntry::new("node",  &["--version"], "core", "https://nodejs.org"),
    ToolEntry::new("npx",   &["--version"], "core", "https://nodejs.org"),
    // ── Logic layer ───────────────────────────────────────────────────────────
    ToolEntry::new("cargo kani", &["kani", "--version"], "logic", "cargo install --locked kani-verifier"),
    ToolEntry::new("mypy",   &["--version"], "logic", "pip install mypy"),
    ToolEntry::new("pytest", &["--version"], "logic", "pip install pytest"),
    // ── Structural layer ──────────────────────────────────────────────────────
    ToolEntry::new("cargo mutants", &["mutants", "--version"], "structural", "cargo install cargo-mutants"),
    // go-mutesting --help exits non-zero; use SpawnOk so a present binary is not
    // falsely reported as missing.
    ToolEntry::spawn_ok("go-mutesting", &["--help"], "structural", "go install github.com/zimmski/go-mutesting/cmd/go-mutesting@latest"),
    ToolEntry::new("mutmut", &["--version"], "structural", "pip install mutmut"),
    // ── Hostile layer ─────────────────────────────────────────────────────────
    ToolEntry::new("bandit",      &["--version"],          "hostile", "pip install bandit"),
    ToolEntry::new("cargo audit", &["audit", "--version"], "hostile", "cargo install cargo-audit"),
    ToolEntry::new("cargo fuzz",  &["fuzz", "--version"],  "hostile", "cargo install cargo-fuzz"),
    // npm-audit, pnpm-audit, yarn-audit are subcommands of the package manager,
    // not standalone binaries. NpmAuditRunner selects based on lockfile.
    ToolEntry::new("npm",     &["--version"], "hostile", "https://nodejs.org  (bundled with node)"),
    ToolEntry::new("pip-audit", &["--version"], "hostile", "pip install pip-audit"),
    ToolEntry::new("pnpm",    &["--version"], "hostile", "npm install -g pnpm  OR  https://pnpm.io/installation"),
    ToolEntry::new("semgrep", &["--version"], "hostile", "pip install semgrep  OR  brew install semgrep"),
    ToolEntry::new("yarn",    &["--version"], "hostile", "npm install -g yarn  OR  https://yarnpkg.com"),
];

/// Result of probing a single tool.
#[derive(Debug, Clone)]
pub struct ToolStatus {
    pub name: &'static str,
    pub layer: &'static str,
    pub available: bool,
    pub install: &'static str,
    /// True when this tool is relevant to the detected project/language.
    /// Always false until `apply_applicability()` is called.
    pub applicable: bool,
    /// True when applicable AND the tool's layer is enabled in config.
    /// Agents should install missing `required` tools before running.
    pub required: bool,
    /// Human-readable reason for the applicability decision.
    /// In workspace mode this names the member(s) that triggered applicability.
    pub reason: String,
}

/// Probe every entry in `TOOL_REGISTRY` using `proc`.
/// Returns statuses with `applicable = false`; call `apply_applicability()` to enrich.
pub fn probe_all(proc: &dyn SubprocessRunner) -> Vec<ToolStatus> {
    TOOL_REGISTRY.iter().map(|entry| {
        let binary = entry.name.split_whitespace().next().unwrap_or(entry.name);
        let available = match entry.mode {
            AvailabilityMode::ExitSuccess => proc.is_available(binary, entry.check_args),
            AvailabilityMode::SpawnOk => proc
                .run(binary, entry.check_args, std::path::Path::new("."))
                .is_ok(),
        };
        ToolStatus {
            name: entry.name,
            layer: entry.layer,
            available,
            install: entry.install,
            applicable: false,
            required: false,
            reason: String::new(),
        }
    }).collect()
}

/// Enrich statuses with applicability information based on project and config.
/// Modifies statuses in place; safe to call more than once (idempotent).
pub fn apply_applicability(statuses: &mut [ToolStatus], project: &ProjectInfo, config: &BarzelConfig) {
    let root = Path::new(&project.root);
    let ws_root = project.workspace_root.as_deref().map(Path::new);
    let enabled = &config.layers.enabled;

    for s in statuses.iter_mut() {
        let (applicable, reason) = tool_applicability(s.name, project.language, root, ws_root);
        let layer_enabled = enabled.iter().any(|e| e == s.layer || s.layer == "core");
        s.applicable = applicable;
        s.required = applicable && layer_enabled;
        s.reason = reason.to_string();
    }
}

/// Enrich statuses with workspace-aggregated applicability across all members.
/// `applicable = any member says applicable`, `required = any member says required`.
/// Each member's effective config is its local `.barzel.toml` when present, otherwise
/// the root config passed in. This matches the semantics used during `run`.
/// Reason strings identify the contributing member(s) by path and language.
pub fn apply_applicability_workspace(
    statuses: &mut [ToolStatus],
    members: &[(String, ProjectInfo)],
    config: &BarzelConfig,
) {
    for s in statuses.iter_mut() {
        let mut contributing: Vec<String> = Vec::new();
        let mut any_applicable = false;
        let mut any_required = false;

        for (rel_path, project) in members {
            let root = Path::new(&project.root);
            let ws_root = project.workspace_root.as_deref().map(Path::new);
            let (applicable, _) = tool_applicability(s.name, project.language, root, ws_root);
            if applicable {
                any_applicable = true;
                let effective_cfg = if root.join(".barzel.toml").exists() {
                    std::borrow::Cow::Owned(BarzelConfig::load_for_project(root))
                } else {
                    std::borrow::Cow::Borrowed(config)
                };
                let layer_enabled = effective_cfg.layers.enabled.iter().any(|e| e == s.layer || s.layer == "core");
                if layer_enabled { any_required = true; }
                let lang = project.language.to_string().to_lowercase();
                contributing.push(format!("{} ({})", rel_path, lang));
            }
        }

        s.applicable = any_applicable;
        s.required = any_required;
        s.reason = if contributing.is_empty() {
            "not applicable to any workspace member".to_string()
        } else {
            contributing.join(", ")
        };
    }
}

/// Probe all tools and immediately apply project/config applicability.
pub fn probe_all_with_context(
    proc: &dyn SubprocessRunner,
    project: &ProjectInfo,
    config: &BarzelConfig,
) -> Vec<ToolStatus> {
    let mut statuses = probe_all(proc);
    apply_applicability(&mut statuses, project, config);
    statuses
}

/// Probe tools once and aggregate applicability across all workspace members.
pub fn probe_all_with_workspace_context(
    proc: &dyn SubprocessRunner,
    members: &[(String, ProjectInfo)],
    config: &BarzelConfig,
) -> Vec<ToolStatus> {
    let mut statuses = probe_all(proc);
    apply_applicability_workspace(&mut statuses, members, config);
    statuses
}

/// Compute applicability for a single tool given language, package root, and optional workspace root.
/// The workspace root is checked as a lockfile fallback for package-manager tools.
/// Returns `(applicable, reason)`.
fn tool_applicability(
    name: &str,
    language: Language,
    root: &Path,
    workspace_root: Option<&Path>,
) -> (bool, &'static str) {
    // Lockfile detection: check package root first, then workspace root as fallback.
    // This handles the common monorepo pattern where lockfiles live at the repo root.
    let find_lock = |filename: &str| -> bool {
        root.join(filename).exists()
            || workspace_root.is_some_and(|ws| ws.join(filename).exists())
    };
    let has_pnpm_lock  = find_lock("pnpm-lock.yaml");
    let has_npm_lock   = find_lock("package-lock.json");
    let has_yarn_lock  = find_lock("yarn.lock");
    let no_ts_lockfile = !has_pnpm_lock && !has_npm_lock && !has_yarn_lock;

    match name {
        // ── Core runtimes ─────────────────────────────────────────────────────
        "cargo" => (language == Language::Rust, "Rust project"),
        "go"    => (language == Language::Go,   "Go project"),
        "node" | "npx" => (language == Language::TypeScript, "TypeScript project"),

        // ── Logic ─────────────────────────────────────────────────────────────
        "cargo kani"  => (language == Language::Rust,   "Rust project"),
        "pytest"      => (language == Language::Python, "Python project"),
        "mypy"        => (language == Language::Python, "Python project"),

        // ── Structural ────────────────────────────────────────────────────────
        "cargo mutants"  => (language == Language::Rust, "Rust project"),
        "go-mutesting"   => (language == Language::Go,   "Go project"),
        "mutmut"         => (language == Language::Python, "Python project"),

        // ── Hostile ───────────────────────────────────────────────────────────
        "cargo audit" => (language == Language::Rust,   "Rust project"),
        "cargo fuzz"  => (language == Language::Rust,   "Rust project"),
        "bandit"      => (language == Language::Python, "Python project"),
        "pip-audit"   => (language == Language::Python, "Python project"),
        "semgrep"     => (true, "all projects (cross-language SAST)"),

        // Package-manager audit: lockfile detection, language-gated so non-TS projects
        // with a stray lockfile are never told to install npm/pnpm/yarn.
        "pnpm" => (
            language == Language::TypeScript && has_pnpm_lock,
            if has_pnpm_lock { "pnpm-lock.yaml detected" } else { "not detected" },
        ),
        "npm"  => (
            language == Language::TypeScript && (has_npm_lock || no_ts_lockfile),
            if has_npm_lock { "package-lock.json detected" }
            else if language == Language::TypeScript { "TypeScript project (default package manager)" }
            else { "not detected" },
        ),
        "yarn" => (
            language == Language::TypeScript && has_yarn_lock,
            if has_yarn_lock { "yarn.lock detected" } else { "not detected" },
        ),

        _ => (false, "not applicable to detected project"),
    }
}

/// Serialize a slice of `ToolStatus` to the agent-facing JSON shape.
/// Includes `applicable`, `required`, and `reason` when applicability has been computed.
pub fn tool_statuses_to_json(statuses: &[ToolStatus]) -> Vec<serde_json::Value> {
    statuses.iter().map(|s| serde_json::json!({
        "name":       s.name,
        "layer":      s.layer,
        "available":  s.available,
        "install":    s.install,
        "applicable": s.applicable,
        "required":   s.required,
        "reason":     s.reason,
    })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{MockProcessRunner, ProcessOutput};

    #[test]
    fn registry_contains_expected_tools() {
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|e| e.name).collect();
        // Core runtimes
        assert!(names.contains(&"cargo"));
        assert!(names.contains(&"go"));
        assert!(names.contains(&"node"));
        assert!(names.contains(&"npx"));
        // Logic
        assert!(names.contains(&"cargo kani"));
        assert!(names.contains(&"pytest"));
        assert!(names.contains(&"mypy"));
        // Structural
        assert!(names.contains(&"cargo mutants"));
        assert!(names.contains(&"go-mutesting"), "go-mutesting must be in registry");
        assert!(names.contains(&"mutmut"));
        // Hostile
        assert!(names.contains(&"cargo audit"), "cargo audit must be in registry");
        assert!(names.contains(&"pip-audit"), "pip-audit must be in registry");
        assert!(names.contains(&"semgrep"));
        assert!(names.contains(&"bandit"));
        assert!(names.contains(&"cargo fuzz"));
        assert!(names.contains(&"npm"),  "npm must be in registry (backs npm audit)");
        assert!(names.contains(&"pnpm"), "pnpm must be in registry (backs pnpm audit)");
        assert!(names.contains(&"yarn"), "yarn must be in registry (backs yarn audit)");
    }

    #[test]
    fn every_entry_has_nonempty_install_guidance() {
        for entry in TOOL_REGISTRY {
            assert!(!entry.install.is_empty(),
                "entry '{}' has empty install guidance", entry.name);
        }
    }

    #[test]
    fn every_entry_has_valid_layer() {
        let valid = ["core", "logic", "structural", "hostile", "operational"];
        for entry in TOOL_REGISTRY {
            assert!(valid.contains(&entry.layer),
                "entry '{}' has unknown layer '{}'", entry.name, entry.layer);
        }
    }

    #[test]
    fn go_mutesting_uses_spawn_ok_mode() {
        let entry = TOOL_REGISTRY.iter().find(|e| e.name == "go-mutesting").unwrap();
        assert_eq!(entry.mode, AvailabilityMode::SpawnOk,
            "go-mutesting must use SpawnOk because --help exits non-zero");
    }

    #[test]
    fn spawn_ok_available_when_spawn_succeeds_but_exit_nonzero() {
        // A mock that returns Ok(ProcessOutput { success: false }) simulates
        // go-mutesting --help: the process spawned, but exited non-zero.
        struct NonZeroExitProc;
        impl crate::process::SubprocessRunner for NonZeroExitProc {
            fn run(&self, cmd: &str, _args: &[&str], _cwd: &std::path::Path)
                -> std::io::Result<ProcessOutput>
            {
                if cmd == "go-mutesting" {
                    Ok(ProcessOutput { success: false, stdout: String::new(), stderr: "usage".into() })
                } else {
                    // All other tools exit successfully
                    Ok(ProcessOutput { success: true, stdout: "ok".into(), stderr: String::new() })
                }
            }
        }
        let statuses = probe_all(&NonZeroExitProc);
        let go_mut = statuses.iter().find(|s| s.name == "go-mutesting").unwrap();
        assert!(go_mut.available, "go-mutesting must be available when spawn succeeds (SpawnOk mode)");
        // A tool with ExitSuccess mode that exits non-zero is NOT available
        let cargo = statuses.iter().find(|s| s.name == "cargo").unwrap();
        assert!(cargo.available, "cargo with ExitSuccess sees success=true → available");
    }

    #[test]
    fn exit_success_unavailable_when_exit_nonzero() {
        // Non-zero exit on a normal tool → not available
        let statuses = probe_all(&MockProcessRunner::failing("error"));
        let cargo = statuses.iter().find(|s| s.name == "cargo").unwrap();
        assert!(!cargo.available);
        // go-mutesting with SpawnOk is still available (spawn succeeded)
        let go_mut = statuses.iter().find(|s| s.name == "go-mutesting").unwrap();
        assert!(go_mut.available, "SpawnOk tool available when proc.run returns Ok(_)");
    }

    #[test]
    fn probe_all_available_when_all_pass() {
        let statuses = probe_all(&MockProcessRunner::passing("ok"));
        assert_eq!(statuses.len(), TOOL_REGISTRY.len());
        assert!(statuses.iter().all(|s| s.available));
    }

    #[test]
    fn probe_all_all_unavailable_when_io_error() {
        // When run() returns io::Error (binary not on PATH), even SpawnOk tools
        // are unavailable — the process could not be spawned at all.
        struct IoErrorProc;
        impl crate::process::SubprocessRunner for IoErrorProc {
            fn run(&self, _: &str, _: &[&str], _: &std::path::Path) -> std::io::Result<ProcessOutput> {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
            }
        }
        let statuses = probe_all(&IoErrorProc);
        assert!(statuses.iter().all(|s| !s.available),
            "io::Error on run() must mark every tool unavailable, including SpawnOk tools");
    }

    #[test]
    fn health_checks_not_in_registry() {
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|e| e.name).collect();
        assert!(!names.iter().any(|n| n.contains("health")),
            "health checks are configured, not tool-based — must not appear in registry");
    }

    #[test]
    fn npm_audit_not_a_separate_binary_entry() {
        // npm-audit is a subcommand of npm — there is no standalone binary.
        // The registry covers the package managers (npm, pnpm, yarn) instead.
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|e| e.name).collect();
        assert!(!names.contains(&"npm-audit") && !names.contains(&"npm audit"),
            "npm-audit is a subcommand, not a standalone binary");
        assert!(names.contains(&"npm") && names.contains(&"pnpm") && names.contains(&"yarn"),
            "npm, pnpm, and yarn must be present to cover NpmAuditRunner's three modes");
    }

    fn make_status(name: &'static str, layer: &'static str, available: bool, install: &'static str) -> ToolStatus {
        ToolStatus { name, layer, available, install, applicable: false, required: false, reason: String::new() }
    }

    // ── applicability ─────────────────────────────────────────────────────────

    fn rust_project(root: &str) -> ProjectInfo {
        ProjectInfo {
            language: Language::Rust,
            root: root.to_string(),
            has_tests: true,
            package_name: Some("myapp".to_string()),
            frameworks: crate::detect::ProjectFrameworks::default(),
            workspace_root: None,
        }
    }

    fn ts_project(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::TypeScript, ..rust_project(root) }
    }

    fn python_project(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::Python, ..rust_project(root) }
    }

    fn go_project(root: &str) -> ProjectInfo {
        ProjectInfo { language: Language::Go, ..rust_project(root) }
    }

    fn default_config() -> crate::config::BarzelConfig {
        crate::config::BarzelConfig::default()
    }

    fn applicable_names(statuses: &[ToolStatus]) -> Vec<&str> {
        statuses.iter().filter(|s| s.applicable).map(|s| s.name).collect()
    }

    fn missing_required_count(statuses: &[ToolStatus]) -> usize {
        statuses.iter().filter(|s| s.required && !s.available).count()
    }

    #[test]
    fn rust_project_marks_cargo_tools_applicable() {
        let dir = tempfile::tempdir().unwrap();
        let p = rust_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"cargo"),          "cargo applicable for Rust");
        assert!(names.contains(&"cargo kani"),     "cargo kani applicable for Rust");
        assert!(names.contains(&"cargo mutants"),  "cargo mutants applicable for Rust");
        assert!(names.contains(&"cargo audit"),    "cargo audit applicable for Rust");
        assert!(names.contains(&"cargo fuzz"),     "cargo fuzz applicable for Rust");
        assert!(names.contains(&"semgrep"),        "semgrep always applicable");
        assert!(!names.contains(&"pytest"),        "pytest not applicable for Rust");
        assert!(!names.contains(&"npm"),           "npm not applicable for Rust");
        assert!(!names.contains(&"pnpm"),          "pnpm not applicable for Rust");
        assert!(!names.contains(&"yarn"),          "yarn not applicable for Rust");
    }

    #[test]
    fn python_project_marks_python_tools_applicable() {
        let dir = tempfile::tempdir().unwrap();
        let p = python_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"pytest"),    "pytest applicable for Python");
        assert!(names.contains(&"mypy"),      "mypy applicable for Python");
        assert!(names.contains(&"mutmut"),    "mutmut applicable for Python");
        assert!(names.contains(&"bandit"),    "bandit applicable for Python");
        assert!(names.contains(&"pip-audit"), "pip-audit applicable for Python");
        assert!(!names.contains(&"cargo"),    "cargo not applicable for Python");
    }

    #[test]
    fn go_project_marks_go_tools_applicable() {
        let dir = tempfile::tempdir().unwrap();
        let p = go_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"go"),           "go applicable for Go");
        assert!(names.contains(&"go-mutesting"), "go-mutesting applicable for Go");
        assert!(!names.contains(&"cargo"),       "cargo not applicable for Go");
        assert!(!names.contains(&"pytest"),      "pytest not applicable for Go");
    }

    #[test]
    fn ts_project_with_pnpm_lock_marks_pnpm_applicable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"").unwrap();
        let p = ts_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"pnpm"), "pnpm applicable with pnpm-lock.yaml");
        assert!(names.contains(&"node"), "node applicable for TypeScript");
    }

    #[test]
    fn ts_project_with_package_lock_marks_npm_applicable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        let p = ts_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"npm"), "npm applicable with package-lock.json");
    }

    #[test]
    fn ts_project_with_yarn_lock_marks_yarn_applicable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        let p = ts_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"yarn"), "yarn applicable with yarn.lock");
    }

    #[test]
    fn disabled_layer_makes_tools_not_required_but_still_applicable() {
        let dir = tempfile::tempdir().unwrap();
        let p = rust_project(dir.path().to_str().unwrap());
        let mut cfg = default_config();
        // Disable structural layer
        cfg.layers.enabled = vec!["logic".to_string(), "hostile".to_string(), "operational".to_string()];
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &cfg);

        let mutants = statuses.iter().find(|s| s.name == "cargo mutants").unwrap();
        // Still applicable (Rust project) but not required (structural disabled)
        assert!(mutants.applicable, "cargo mutants still applicable for Rust even when layer disabled");
        assert!(!mutants.required,  "cargo mutants must not be required when structural layer is disabled");

        let audit = statuses.iter().find(|s| s.name == "cargo audit").unwrap();
        assert!(audit.applicable, "cargo audit applicable for Rust");
        assert!(audit.required,   "cargo audit required when hostile is enabled");
    }

    #[test]
    fn ts_project_no_lockfile_defaults_only_npm_applicable() {
        let dir = tempfile::tempdir().unwrap();
        // No lockfile files written — default to npm only
        let p = ts_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(names.contains(&"npm"),  "npm is the default applicable package manager");
        assert!(!names.contains(&"pnpm"), "pnpm not applicable without pnpm-lock.yaml");
        assert!(!names.contains(&"yarn"), "yarn not applicable without yarn.lock");
    }

    #[test]
    fn semgrep_applicable_for_all_languages() {
        let dir = tempfile::tempdir().unwrap();
        for lang in [Language::Rust, Language::Python, Language::TypeScript, Language::Go] {
            let p = ProjectInfo { language: lang, ..rust_project(dir.path().to_str().unwrap()) };
            let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
            apply_applicability(&mut statuses, &p, &default_config());
            assert!(
                statuses.iter().find(|s| s.name == "semgrep").map(|s| s.applicable).unwrap_or(false),
                "semgrep must be applicable for {:?}", lang
            );
        }
    }

    #[test]
    fn rust_project_with_package_lock_does_not_mark_npm_applicable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        let p = rust_project(dir.path().to_str().unwrap());
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability(&mut statuses, &p, &default_config());
        let names = applicable_names(&statuses);
        assert!(!names.contains(&"npm"),  "npm must not be applicable for Rust even with package-lock.json");
        assert!(!names.contains(&"pnpm"), "pnpm must not be applicable for Rust");
        assert!(!names.contains(&"yarn"), "yarn must not be applicable for Rust");
    }

    #[test]
    fn missing_required_count_only_counts_required_and_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let p = rust_project(dir.path().to_str().unwrap());
        // available=false for all tools
        let mut statuses = probe_all(&MockProcessRunner::unavailable());
        apply_applicability(&mut statuses, &p, &default_config());

        let count = missing_required_count(&statuses);
        // Only required (applicable + layer enabled) AND unavailable tools should count
        let expected = statuses.iter().filter(|s| s.required && !s.available).count();
        assert_eq!(count, expected);
        // Sanity: applicable-but-not-required tools do not inflate the count
        let applicable_unavailable = statuses.iter().filter(|s| s.applicable && !s.available).count();
        assert!(count <= applicable_unavailable, "count must not exceed applicable-unavailable");
    }

    // ── workspace aggregation ─────────────────────────────────────────────────

    fn members_rust_and_ts(rust_root: &str, ts_root: &str) -> Vec<(String, crate::detect::ProjectInfo)> {
        vec![
            ("crates/api".to_string(), rust_project(rust_root)),
            ("apps/web".to_string(),   ts_project(ts_root)),
        ]
    }

    #[test]
    fn workspace_aggregates_required_from_rust_and_ts_members() {
        let rust_dir = tempfile::tempdir().unwrap();
        let ts_dir   = tempfile::tempdir().unwrap();
        let members  = members_rust_and_ts(
            rust_dir.path().to_str().unwrap(),
            ts_dir.path().to_str().unwrap(),
        );
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        // Rust tools must be applicable/required
        let cargo = statuses.iter().find(|s| s.name == "cargo").unwrap();
        assert!(cargo.applicable && cargo.required, "cargo required for rust member");
        assert!(cargo.reason.contains("crates/api"), "reason names the rust member");

        // TS tools must be applicable/required
        let node = statuses.iter().find(|s| s.name == "node").unwrap();
        assert!(node.applicable && node.required, "node required for ts member");
        assert!(node.reason.contains("apps/web"), "reason names the ts member");

        // semgrep required for both
        let semgrep = statuses.iter().find(|s| s.name == "semgrep").unwrap();
        assert!(semgrep.applicable && semgrep.required, "semgrep required for all members");
        assert!(semgrep.reason.contains("crates/api") && semgrep.reason.contains("apps/web"),
            "semgrep reason lists all members");

        // Python tools not applicable in this workspace
        let pytest = statuses.iter().find(|s| s.name == "pytest").unwrap();
        assert!(!pytest.applicable, "pytest not applicable in rust+ts workspace");
    }

    #[test]
    fn workspace_rust_member_with_package_lock_does_not_mark_npm_required() {
        let rust_dir = tempfile::tempdir().unwrap();
        std::fs::write(rust_dir.path().join("package-lock.json"), b"{}").unwrap();
        let ts_dir = tempfile::tempdir().unwrap();
        let members = members_rust_and_ts(
            rust_dir.path().to_str().unwrap(),
            ts_dir.path().to_str().unwrap(),
        );
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        // npm is applicable only because of the TS member (no-lockfile → npm default),
        // not because of the Rust member's stray package-lock.json
        let npm = statuses.iter().find(|s| s.name == "npm").unwrap();
        assert!(npm.applicable, "npm applicable due to TS member");
        assert!(npm.reason.contains("apps/web"), "npm reason must reference TS member");
        assert!(!npm.reason.contains("crates/api"), "npm reason must not reference Rust member");
    }

    #[test]
    fn workspace_disabled_layer_means_applicable_but_not_required() {
        let rust_dir = tempfile::tempdir().unwrap();
        let ts_dir   = tempfile::tempdir().unwrap();
        let members  = members_rust_and_ts(
            rust_dir.path().to_str().unwrap(),
            ts_dir.path().to_str().unwrap(),
        );
        let mut cfg = default_config();
        cfg.layers.enabled = vec!["logic".to_string(), "hostile".to_string(), "operational".to_string()];
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &cfg);

        let mutants = statuses.iter().find(|s| s.name == "cargo mutants").unwrap();
        assert!(mutants.applicable, "cargo mutants still applicable for rust member");
        assert!(!mutants.required, "cargo mutants not required when structural layer disabled");
    }

    #[test]
    fn workspace_missing_count_only_counts_required_and_unavailable() {
        let rust_dir = tempfile::tempdir().unwrap();
        let ts_dir   = tempfile::tempdir().unwrap();
        let members  = members_rust_and_ts(
            rust_dir.path().to_str().unwrap(),
            ts_dir.path().to_str().unwrap(),
        );
        let mut statuses = probe_all(&MockProcessRunner::unavailable());
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        let count = missing_required_count(&statuses);
        let expected = statuses.iter().filter(|s| s.required && !s.available).count();
        assert_eq!(count, expected);
        let applicable_unavailable = statuses.iter().filter(|s| s.applicable && !s.available).count();
        assert!(count <= applicable_unavailable);
    }

    #[test]
    fn workspace_root_lockfile_fallback_marks_pnpm_applicable_for_ts_member() {
        let ws_root = tempfile::tempdir().unwrap();
        // lockfile is at workspace root, not package root
        std::fs::write(ws_root.path().join("pnpm-lock.yaml"), b"").unwrap();
        let pkg_dir = tempfile::tempdir().unwrap();
        let mut member_info = ts_project(pkg_dir.path().to_str().unwrap());
        member_info.workspace_root = Some(ws_root.path().to_str().unwrap().to_string());
        let members = vec![("apps/web".to_string(), member_info)];

        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        let pnpm = statuses.iter().find(|s| s.name == "pnpm").unwrap();
        assert!(pnpm.applicable, "pnpm applicable via workspace-root pnpm-lock.yaml");
    }

    // ── member-local config honoured for required calculation ─────────────────

    /// Minimal complete .barzel.toml with `structural` disabled.
    /// Partial TOML falls back to defaults in load_for_project, so all sections needed.
    fn write_structural_disabled_config(dir: &std::path::Path) {
        let toml = r#"
[project]
name = "member"
language = "rust"

[layers]
enabled = ["logic", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = false

[layers.structural]
mutation_testing = false
mutation_threshold = 95.0

[layers.hostile]
fuzzing = false
sast = true

[reporting]
format = "json"
fail_on = "high"
"#;
        std::fs::write(dir.join(".barzel.toml"), toml).unwrap();
    }

    #[test]
    fn member_local_config_disabling_structural_makes_mutants_not_required() {
        let member_dir = tempfile::tempdir().unwrap();
        write_structural_disabled_config(member_dir.path());

        let members = vec![
            ("crates/api".to_string(), rust_project(member_dir.path().to_str().unwrap())),
        ];
        // Root config still has structural enabled
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        let mutants = statuses.iter().find(|s| s.name == "cargo mutants").unwrap();
        assert!(mutants.applicable, "cargo mutants still applicable for Rust member");
        assert!(!mutants.required,  "cargo mutants must not be required when member local config disables structural");
    }

    #[test]
    fn structural_remains_required_when_second_member_still_enables_it() {
        let disabled_dir = tempfile::tempdir().unwrap();
        write_structural_disabled_config(disabled_dir.path());

        // Second member has no local config — uses root config which has structural enabled
        let enabled_dir = tempfile::tempdir().unwrap();

        let members = vec![
            ("crates/disabled".to_string(), rust_project(disabled_dir.path().to_str().unwrap())),
            ("crates/enabled".to_string(),  rust_project(enabled_dir.path().to_str().unwrap())),
        ];
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        let mutants = statuses.iter().find(|s| s.name == "cargo mutants").unwrap();
        assert!(mutants.applicable, "cargo mutants applicable (both members are Rust)");
        assert!(mutants.required,   "cargo mutants required because crates/enabled still has structural enabled");
    }

    #[test]
    fn member_without_local_config_uses_root_config() {
        let member_dir = tempfile::tempdir().unwrap();
        // No .barzel.toml — should fall through to root config

        let members = vec![
            ("crates/api".to_string(), rust_project(member_dir.path().to_str().unwrap())),
        ];
        // Root config has structural enabled (default)
        let mut statuses = probe_all(&MockProcessRunner::passing("ok"));
        apply_applicability_workspace(&mut statuses, &members, &default_config());

        let mutants = statuses.iter().find(|s| s.name == "cargo mutants").unwrap();
        assert!(mutants.applicable, "cargo mutants applicable for Rust");
        assert!(mutants.required,   "cargo mutants required when root config has structural enabled and no local override");
    }

    #[test]
    fn tool_statuses_to_json_shape() {
        let statuses = vec![
            make_status("cargo",   "core",    true,  "https://rustup.rs"),
            make_status("semgrep", "hostile", false, "pip install semgrep"),
        ];
        let json = tool_statuses_to_json(&statuses);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["name"].as_str(), Some("cargo"));
        assert_eq!(json[0]["layer"].as_str(), Some("core"));
        assert_eq!(json[0]["available"].as_bool(), Some(true));
        assert_eq!(json[0]["install"].as_str(), Some("https://rustup.rs"));
        assert_eq!(json[1]["available"].as_bool(), Some(false));
        assert!(!json[1]["install"].as_str().unwrap_or("").is_empty(),
            "unavailable tool must include non-empty install guidance");
    }
}
