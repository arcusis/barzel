# M0 Complete — Foundation Delivered

**Date:** 2026-05-02

**Status:** Done

**What was built:**
- Full Rust CLI skeleton with `clap`
- `barzel init` (human + stdio JSON)
- Project language detection (Rust / TS / Go / Unknown)
- `.barzel.toml` scaffolding with sensible defaults for all 4 layers
- Strict stdio JSON protocol (single request → single response)
- Zero panics, clean `cargo clippy -- -D warnings`
- 2 targeted unit tests for detection

**Verification performed:**
- `cargo check` — clean
- `cargo test` (targeted) — 2/2 passed
- `cargo clippy -- -D warnings` — clean

**Next:** M1 — Protocol hardening + unified reporting foundation

**Linked notes:** [[barzel-m0-foundation]]
