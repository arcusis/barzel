# M4 — Hostile Layer (Fuzzing + SAST)

**Goal:** Add basic security scanning support.

**Tools:**
- Semgrep (language-agnostic SAST, excellent rules)
- cargo-fuzz / libFuzzer for Rust fuzzing

**M4 Scope:**
- Detect if `semgrep` is available
- Run a quick Semgrep scan with default rules
- Report any findings with severity
- Fuzzing detection (future)

This gives immediate security value.
