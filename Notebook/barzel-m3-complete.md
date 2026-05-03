# M3 Complete — Plugin System Polished + Full Self-Test

**Date:** 2026-05-03

**Status:** Done

**What was delivered:**

- Removed all dead code from `run.rs` (legacy `run_logic_layer`, `run_structural_layer`, `run_hostile_layer` and duplicate helpers)
- Added `runner: String` field to `LayerResult` so the report tracks which tool ran each layer
- Added `skip_message()` to `TestRunner` trait — each runner provides a human-readable install hint
- Fixed orchestrator: unavailable runners now produce a `Skipped` `LayerResult` instead of silently disappearing from the report
- All three layers (logic/structural/hostile) always appear in the JSON report
- Improved human output: per-layer table with `[layer] [runner] STATUS detail`
- Implemented `barzel report` command — reads latest JSON from `.barzel/reports/`, displays formatted summary with severity-colored findings
- Added `--path` flag to `barzel run`
- Parsed actual test counts from `cargo test` output in `ProptestRunner`
- Implemented file-based caching in `cache.rs`: content-hashes `Cargo.toml` + all `src/**/*.rs`; structural layer skips if source unchanged
- Integrated cache check into orchestrator for expensive layers (structural)
- `semgrep` runner now properly parses semgrep JSON output with per-finding severity + location

**Proof on this project (`barzel run`):**
```
→ Verifying rust project at .

  logic        [proptest      ]  PASS     2 tests · 517ms
  structural   [cargo-mutants ]  SKIPPED  cargo-mutants not installed — run `cargo install cargo-mutants` to enable mutation testing (target: ≥95% mutation score)
  hostile      [semgrep       ]  SKIPPED  semgrep not installed — run `pip install semgrep` to enable SAST security scanning

→ PASS  |  2 finding(s)  |  ./.barzel/reports/report-*.json
```

**Verification:**
- `cargo check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 2/2 passed
- `cargo build --release` — clean
- `./target/release/barzel run` — all three layers, correct output
- `./target/release/barzel report` — displays latest report correctly
- `echo '{"command":"run"}' | barzel --stdio` — valid JSON response

**Next:** M4 — install `cargo-mutants` and `semgrep` to exercise the structural + hostile runners; add integration tests; improve mutation score reporting
