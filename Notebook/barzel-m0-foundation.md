# M0 — Foundation

**Goal:** A working `barzel` binary that can be invoked by both humans and AI agents, performs project detection, and scaffolds a configuration file.

**Deliverables:**
- `cargo new` style project with proper workspace layout
- `barzel --version` and `barzel --help`
- `barzel init [path]` command
- `--stdio` mode that accepts JSON and returns JSON
- Project language detection (Rust, TypeScript, Go)
- `.barzel.toml` written with sensible defaults
- Zero panics, `cargo clippy -- -D warnings` clean
- Targeted unit tests for detection and config writing

**Success Criteria:**
- `barzel init .` creates `.barzel.toml` in current directory
- `echo '{"command":"init"}' | barzel --stdio` returns valid JSON
- `cargo test --package barzel-cli` passes
- `cargo clippy` passes with no warnings

**Out of Scope for M0:**
- Actual test execution
- Any external tool integration
- Report generation beyond init confirmation

**Next Milestone:** M1 — Protocol hardening + reporting foundation
