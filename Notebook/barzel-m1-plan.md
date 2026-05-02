# M1 — Protocol Hardening + Reporting Foundation

**Goal:** Make the stdio protocol and reporting system production-grade so that AI agents can reliably drive Barzel and get structured, parseable results.

**Deliverables:**
- Versioned JSON protocol (`v1` in responses)
- `request_id` (UUID) + `timestamp` on every response
- Strong `BarzelReport` model (layers, findings, metrics, summary)
- `barzel run` produces a real report (even with stub layer results)
- `barzel report latest` shows the last report in human or JSON form
- All responses are deterministic and machine-friendly
- Targeted tests for report serialization and protocol

**Success Criteria:**
- `barzel run` creates a `.barzel/reports/` directory with timestamped JSON reports
- `echo '{"command":"run"}' | barzel --stdio` returns a full report object
- `barzel report latest` works
- `cargo clippy -- -D warnings` clean
- No dead code warnings on new types

**Out of Scope for M1:**
- Actual execution of property-based tests, mutation testing, etc. (those come in M2+)
- Chaos engineering / operational layer

**Next after M1:** M2 — Logic Layer (real Property-Based Testing integration)
