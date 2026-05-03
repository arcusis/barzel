# Barzel Vision

Barzel exists because "it passes the tests" is no longer an acceptable definition of software quality.

**Goal:** Create a single command that any AI coding agent can run to obtain *proof* that a codebase meets the highest engineering standards across logic, structure, security, and operations.

**Non-negotiables:**
- Deterministic, reproducible results
- Structured JSON output that AI agents can parse without hallucination
- Graceful degradation when a layer cannot run (clear reason + exit code)
- Never silently succeed on weak evidence

**Success Metric for v1:**
An AI agent can run `barzel run --layer logic,structural` on a new Rust or TypeScript project and receive a report that includes:
- Property-based test results with 1M+ generated cases
- Mutation score ≥ 95%
- Zero critical SAST findings
- Clear, actionable remediation steps

**Long-term:** Barzel becomes the default "verify" step in every serious AI-driven development workflow.
