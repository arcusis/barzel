# M3 — Structural Layer (Mutation Testing)

**Goal:** Implement real mutation testing support so the Structural layer produces meaningful results.

**Tool:** `cargo-mutants` (best-in-class for Rust)

**Implementation plan:**
- Detect if `cargo-mutants` is available or can be run
- If the project has tests, attempt to run `cargo mutants -- --test` or similar
- Parse the mutation score from output
- If score < 95%, create a High severity finding
- Update report metrics with `mutation_score`

**Success Criteria:**
- On projects with good test coverage, mutation score is reported
- On this project (early stage), it gracefully reports that mutation testing was not run
- Report contains real `mutation_score` field when available

**Note:** Full Stryker integration for TypeScript will come later.
