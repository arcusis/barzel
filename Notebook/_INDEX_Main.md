# Barzel — The Unbreakable Testing CLI

**Status:** Full development in progress (M3 next)

Barzel is a Rust CLI designed to be the ultimate testing orchestrator for AI coding agents and human engineers who demand the highest possible software integrity.

It implements four verification layers:
- Logic (Property-Based Testing + Design by Contract + Formal Verification)
- Structural (MC/DC + Mutation Testing ≥95%)
- Hostile (Fuzzing + Contract Testing + SAST/DAST)
- Operational (Chaos Engineering + Shadowing)

Primary consumers: OpenCode, Claude Code, Cursor, and other AI tools via stdio JSON protocol.

## Core Principles
- Zero panics in production paths
- Every layer produces machine-readable evidence
- Small, correct patches only
- Typecheck → targeted tests → lint before every commit

## Current Milestone
M0 — Foundation (CLI skeleton, stdio protocol, project detection, `init` command)

## Index
- [[barzel-vision]]
- [[barzel-architecture-v1]]
- [[barzel-protocol]]
- [[barzel-m0-foundation]]
- [[barzel-m0-complete]]
- [[barzel-m1-plan]]
- [[barzel-m1-complete]]
- [[barzel-m2-plan]]
- [[barzel-m2-complete]]
- [[barzel-roadmap]]
