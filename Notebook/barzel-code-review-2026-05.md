# Barzel Code Review 2026-05

## Current Shape
The codebase is a single Rust binary with module boundaries that are still healthy for the current scale:

- `src/main.rs` owns CLI entry, stdio request/response, exit code mapping, `check`, and `report`.
- `src/run.rs` owns language detection, config loading, runner construction, layer filtering, orchestration, and output mode handling.
- `src/orchestrator.rs` owns execution order, cache checks, fail-fast behavior, and aggregation into `BarzelReport`.
- `src/detect.rs` owns single-project language/framework detection.
- `src/report.rs` owns report schema, summary aggregation, local report persistence, and report lookup by ID prefix.
- `src/process.rs` provides `SubprocessRunner` and mock subprocess injection.
- `src/runners/*` implement concrete runners around external tools.

## Strengths
The most important architectural win is the `TestRunner` plus `SubprocessRunner` split. Runners can exercise real parser and status logic without spawning tools in unit tests.

The report model is simple and agent-friendly. `action_items` are flattened in stdio mode and sorted by severity priority in `build_run_data`.

The runner matrix is centralized in `run.rs`, which keeps initial behavior easy to reason about. This will become pressure as dependency scanning and monorepos add more runners, but it is not yet a problem that requires a crate split.

Report persistence already exists in `.barzel/reports`, and reports have IDs. This supports future `report --compare` without adding a service.

## Current Gaps
`VerificationOrchestrator::run_with_progress` executes runners sequentially. This directly blocks the P0 parallel execution goal.

`Finding.reproduce_cmd` is optional and several generated findings rely on `Default::default()`. This weakens the invariant that agents never guess reproduction steps. The first enforcement should be a test/helper policy, not a schema-breaking change.

`BarzelConfig::load_for_project` silently falls back to defaults on invalid TOML. That is friendly but risky for enterprise policy because a malformed config can accidentally run with default thresholds.

`cache::compute_project_hash` only hashes `Cargo.toml` and `src/`. This is acceptable for current Rust structural caching but wrong for TypeScript, Python, Go, lockfiles, tests, and monorepos.

`detect_project` returns one `ProjectInfo`. It cannot represent workspace members, package managers, root config inheritance, or per-package reports.

`check` duplicates tool knowledge in `main.rs` instead of asking runner definitions or a shared registry. This will grow stale as dependency audit runners are added.

## Directional Guidance
Do not split into multiple crates yet. The next three PRs should add focused internal seams:

- A runner execution planner in `orchestrator.rs` for parallel groups.
- A Hostile dependency audit runner family in `src/runners`.
- A workspace detection model beside, not instead of, `ProjectInfo`.

Prefer schema additions over rewrites. The stdio schema can change, but agents benefit from stable core fields.

Keep `run.rs` as the composition root until monorepo support lands. After monorepos, extract runner selection into a registry-like module if the language matrix becomes too large.

Related notes: [[barzel-decision-record-2026-05]], [[barzel-pr1-parallel-layer-execution-plan]], [[barzel-pr2-dependency-vulnerability-scanning-plan]], [[barzel-pr3-monorepo-workspace-support-plan]]
