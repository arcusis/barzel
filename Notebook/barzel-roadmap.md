# Barzel Full Development Roadmap (2026)

**Current State:** M2 complete (Logic layer detects PBT)

**Remaining Milestones:**

### M3 — Structural Layer (Mutation Testing)
- Detect and run mutation testing (cargo-mutants for Rust, Stryker for TS)
- Parse mutation score
- Fail if score < 95% (configurable)
- Real metrics in report

### M4 — Hostile Layer (Fuzzing + SAST)
- Detect `cargo-fuzz` or `libFuzzer`
- Run Semgrep with curated rules (via subprocess)
- Report security findings with severity

### M5 — Polish & Production Readiness
- Add `.barzel.toml` example to repo
- Comprehensive README with usage for AI agents + humans
- `.gitignore` for `.barzel/` and reports
- Better error messages and auto-install guidance
- Version command and man-page style help
- Self-test: run `barzel run` on the Barzel project itself

### M6 — GitHub & Release
- Clean commit history with milestone tags
- GitHub Actions for CI (test + clippy + build)
- crates.io release (optional)
- Final validation on this project

**Core Principles (non-negotiable):**
- Small, correct patches only
- Typecheck → targeted tests → lint before every commit
- Zero panics in production paths
- Excellent stdio JSON for AI agents
- Beautiful human output

**Final Goal:** After M6, running `barzel run` on the Barzel repository itself should produce a clean, high-quality report with no critical issues.
