/// Shared tool registry used by both human `barzel check` and stdio `{"command":"check"}`.
///
/// A single source of truth prevents the two paths from drifting out of sync.
/// Each entry carries name, availability check args, DAP layer, and install guidance
/// for agents and humans alike.
use crate::process::SubprocessRunner;

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
}

/// The canonical tool list. Keep sorted by layer then name.
///
/// Rules:
/// - `name` first word must be the binary to exec.
/// - npm-audit is not a separate binary; npm availability implies it.
/// - Health checks are configured, not tool-based; they do not appear here.
pub const TOOL_REGISTRY: &[ToolEntry] = &[
    // ── Core runtimes ─────────────────────────────────────────────────────────
    ToolEntry { name: "cargo",          check_args: &["--version"],          layer: "core",       install: "https://rustup.rs" },
    ToolEntry { name: "go",             check_args: &["version"],            layer: "core",       install: "https://go.dev/dl" },
    ToolEntry { name: "node",           check_args: &["--version"],          layer: "core",       install: "https://nodejs.org" },
    ToolEntry { name: "npx",            check_args: &["--version"],          layer: "core",       install: "https://nodejs.org" },
    // ── Logic layer ───────────────────────────────────────────────────────────
    ToolEntry { name: "cargo kani",     check_args: &["kani", "--version"],  layer: "logic",      install: "cargo install --locked kani-verifier" },
    ToolEntry { name: "mypy",           check_args: &["--version"],          layer: "logic",      install: "pip install mypy" },
    ToolEntry { name: "pytest",         check_args: &["--version"],          layer: "logic",      install: "pip install pytest" },
    // ── Structural layer ──────────────────────────────────────────────────────
    ToolEntry { name: "cargo mutants",  check_args: &["mutants", "--version"], layer: "structural", install: "cargo install cargo-mutants" },
    ToolEntry { name: "go-mutesting",   check_args: &["--help"],             layer: "structural", install: "go install github.com/zimmski/go-mutesting/cmd/go-mutesting@latest" },
    ToolEntry { name: "mutmut",         check_args: &["--version"],          layer: "structural", install: "pip install mutmut" },
    // ── Hostile layer ─────────────────────────────────────────────────────────
    ToolEntry { name: "bandit",         check_args: &["--version"],          layer: "hostile",    install: "pip install bandit" },
    ToolEntry { name: "cargo audit",    check_args: &["audit", "--version"], layer: "hostile",    install: "cargo install cargo-audit" },
    ToolEntry { name: "cargo fuzz",     check_args: &["fuzz", "--version"],  layer: "hostile",    install: "cargo install cargo-fuzz" },
    ToolEntry { name: "pip-audit",      check_args: &["--version"],          layer: "hostile",    install: "pip install pip-audit" },
    ToolEntry { name: "semgrep",        check_args: &["--version"],          layer: "hostile",    install: "pip install semgrep  OR  brew install semgrep" },
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
/// Availability check: run `<first_word_of_name> <check_args>` and treat success as available.
pub fn probe_all(proc: &dyn SubprocessRunner) -> Vec<ToolStatus> {
    TOOL_REGISTRY.iter().map(|entry| {
        let binary = entry.name.split_whitespace().next().unwrap_or(entry.name);
        let available = proc.is_available(binary, entry.check_args);
        ToolStatus { name: entry.name, layer: entry.layer, available, install: entry.install }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::MockProcessRunner;

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
    fn probe_all_available_when_all_pass() {
        let statuses = probe_all(&MockProcessRunner::passing("ok"));
        assert_eq!(statuses.len(), TOOL_REGISTRY.len());
        assert!(statuses.iter().all(|s| s.available));
    }

    #[test]
    fn probe_all_unavailable_when_all_fail() {
        let statuses = probe_all(&MockProcessRunner::unavailable());
        assert!(statuses.iter().all(|s| !s.available));
    }

    #[test]
    fn health_checks_not_in_registry() {
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|e| e.name).collect();
        assert!(!names.iter().any(|n| n.contains("health")),
            "health checks are configured, not tool-based — must not appear in registry");
    }

    #[test]
    fn npm_audit_not_a_separate_entry() {
        let names: Vec<&str> = TOOL_REGISTRY.iter().map(|e| e.name).collect();
        assert!(!names.contains(&"npm-audit") && !names.contains(&"npm audit"),
            "npm-audit is backed by npm/node — use node/npx availability instead");
    }
}
