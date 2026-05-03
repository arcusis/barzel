# PR1 Parallel Layer Execution Plan

## Goal
Run independent layers concurrently to avoid wasting local AI-agent time. The immediate target is parallel execution for Logic and Hostile runners while preserving deterministic reporting, caching, fail-fast behavior, and progress callbacks.

## Current Code
`VerificationOrchestrator::run_with_progress` loops over `self.runners` sequentially. It handles unavailable runners, structural cache hits, `runner.run(project)`, cache writes, report aggregation, and fail-fast in one loop.

`TestRunner: Send + Sync`, so runner trait objects are already thread-compatible.

`BarzelReport::add_layer` mutates summary state, so report aggregation should stay on one thread after runner results are collected.

## Design
Add an internal execution unit:

```rust
struct RunnerExecution {
    index: usize,
    runner_name: String,
    layer: Layer,
    result: LayerResult,
}
```

Keep availability checks and cache checks deterministic in the orchestrator before spawning. Spawn only runnable, non-cached runners.

Group runnable runners by scheduling policy:

- Logic and Hostile can run concurrently.
- Structural stays after Logic for now because mutation testing is expensive and semantically depends on tests being meaningful.
- Operational can stay after Logic/Structural until that layer is real.
- With `fail_fast`, prefer current sequential behavior for the first PR unless a precise cross-thread cancellation policy is implemented.

Use `std::thread::scope` so borrowed `&dyn TestRunner` values can be used without requiring `'static` runner ownership. Each spawned job returns a `LayerResult`; errors are converted into the existing `RUNNER_FAILED` critical layer result.

Sort completed results by original runner index before `report.add_layer` so JSON output remains stable.

## Implementation Steps
1. In `src/orchestrator.rs`, extract the repeated unavailable/cache/error result construction into small helper functions.
2. Add a sequential path for `fail_fast == true` that preserves current behavior exactly.
3. Add a parallel path for `fail_fast == false`.
4. In the parallel path, emit `on_start` before each spawned runner. For human mode this means the spinner shows starts, not completion.
5. Run available, uncached Logic and Hostile runners in scoped threads.
6. Run Structural and Operational sequentially after those results are added or as a second phase.
7. Save structural cache hashes only after successful non-fail results, preserving current behavior.
8. Add tests with mock runners that record start/end timing or block on a barrier to prove Logic and Hostile overlap.

## Tests
Add tests in `src/orchestrator.rs`:

- `logic_and_hostile_run_in_parallel_without_fail_fast`
- `parallel_results_are_reported_in_original_runner_order`
- `fail_fast_preserves_sequential_stop_after_first_failure`
- `unavailable_runner_still_produces_skipped_result`
- `cached_structural_runner_is_not_spawned`
- `runner_error_in_parallel_becomes_critical_finding`
- `structural_runs_after_logic_hostile_phase`

Use mock runners with `Arc<Barrier>`, `Arc<AtomicBool>`, or timestamp channels. Do not spawn real subprocesses.

## Acceptance Criteria
`cargo test --bins` passes.

`cargo clippy -- -D warnings` passes.

Report layer order is deterministic.

`action_items` remain sorted by priority in stdio output.

No runner loses its `reproduce_cmd` on failure.

Related notes: [[barzel-code-review-2026-05]], [[barzel-pr2-dependency-vulnerability-scanning-plan]]
