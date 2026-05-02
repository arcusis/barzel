# Barzel v1 Architecture

**Runtime:** Rust 1.85+ (2024 edition)

**CLI Framework:** `clap` 4.x with derive API + `clap_complete`

**Error Handling:** `thiserror` for domain errors + `anyhow` for orchestration

**Configuration:** `toml` + `serde` (`.barzel.toml` at project root)

**Output Modes:**
- Human (colored, progress bars via `indicatif`)
- Machine (`--stdio` → strict JSON on stdout, errors on stderr)

**Workspace Structure (planned):**
```
barzel/
├── Cargo.toml (workspace)
├── crates/
│   ├── barzel-cli/          # binary
│   ├── barzel-core/         # orchestrator, config, detection
│   ├── barzel-protocol/     # JSON request/response types
│   ├── barzel-report/       # unified reporting
│   ├── barzel-logic/        # PBT + DbC adapters
│   ├── barzel-structural/   # mutation + coverage
│   └── barzel-hostile/      # fuzz + sast
```

**First Crate:** `barzel-cli` (single binary for M0)

**Key External Tools (orchestrated, not reimplemented):**
- Logic: `proptest`, `quickcheck`, `contracts`
- Structural: `cargo-mutants`, `stryker-cli`
- Hostile: `cargo-fuzz`, `semgrep`
- Reporting: custom JSON + optional JUnit/SARIF

**Security Posture:**
- No `unsafe` in production code paths
- All subprocess calls use `std::process::Command` with explicit args
- No dynamic code execution from user input
- Input validation on every stdio message
