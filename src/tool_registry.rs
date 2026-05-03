/// Shared tool registry used by both human `barzel check` and stdio `{"command":"check"}`.
///
/// A single source of truth prevents the two paths from drifting out of sync.
/// Each entry carries name, availability check args, DAP layer, install guidance,
/// and an availability mode for tools that exit non-zero even when present.
use crate::process::SubprocessRunner;

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
}

/// Probe every entry in `TOOL_REGISTRY` using `proc` and return the results.
pub fn probe_all(proc: &dyn SubprocessRunner) -> Vec<ToolStatus> {
    TOOL_REGISTRY.iter().map(|entry| {
        let binary = entry.name.split_whitespace().next().unwrap_or(entry.name);
        let available = match entry.mode {
            AvailabilityMode::ExitSuccess => proc.is_available(binary, entry.check_args),
            AvailabilityMode::SpawnOk => proc
                .run(binary, entry.check_args, std::path::Path::new("."))
                .is_ok(),
        };
        ToolStatus { name: entry.name, layer: entry.layer, available, install: entry.install }
    }).collect()
}

/// Serialize a slice of `ToolStatus` to the agent-facing JSON shape.
/// Shared between the stdio handler and any future callers so the shape stays consistent.
pub fn tool_statuses_to_json(statuses: &[ToolStatus]) -> Vec<serde_json::Value> {
    statuses.iter().map(|s| serde_json::json!({
        "name":      s.name,
        "layer":     s.layer,
        "available": s.available,
        "install":   s.install,
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

    #[test]
    fn tool_statuses_to_json_shape() {
        let statuses = vec![
            ToolStatus { name: "cargo", layer: "core", available: true,  install: "https://rustup.rs" },
            ToolStatus { name: "semgrep", layer: "hostile", available: false, install: "pip install semgrep" },
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
