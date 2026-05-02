# M2 — Logic Layer (Real Property-Based Testing)

**Goal:** Make the Logic layer actually execute Property-Based Testing instead of stubs.

**Approach for v1:**
- Detect whether the target project already uses `proptest`
- If yes: run `cargo test` (or `cargo test --test proptest`) and parse results
- If no: generate a clear "No PBT found" finding + recommendation to add `proptest`
- Capture real metrics: tests_run, passed, failed, duration
- Still keep other layers as stubs (they will be implemented in later milestones)

**Why this approach:**
- Non-invasive (we don't modify the target project)
- Works immediately on real projects that already have PBT
- Provides clear value and guidance for projects that don't

**Success Criteria:**
- On a project with `proptest` tests, `barzel run --layer logic` reports real pass/fail counts
- On this project (Barzel), it correctly reports "no property tests found" with helpful guidance
- Report JSON contains meaningful `metrics` for the logic layer
- Still passes `cargo clippy -- -D warnings`

**Next:** M3 — Structural Layer (Mutation Testing integration)
