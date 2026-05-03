# PR3 Monorepo Workspace Support Plan

## Goal
Detect enterprise monorepos and run Barzel per workspace member instead of treating the repository as a single project. The report should preserve a repo-level pass/fail while showing package-scoped results and action items.

## Current Code
`detect_project(path)` returns one `ProjectInfo`. `run_verification` then builds one runner matrix from that single language. This cannot represent pnpm workspaces, npm workspaces, lerna, turborepo, Cargo workspaces, or mixed-language repos.

The cache currently hashes only `Cargo.toml` and `src/`, so package-scoped structural caching needs a stronger hash strategy before it is reliable in workspaces.

## Product Semantics
Report both repo-level and package-level status.

The stdio `action_items` must include package context so agents know where to act. Add fields such as:

```json
{
  "package": "web",
  "package_path": "apps/web",
  "layer": "logic",
  "runner": "tsc"
}
```

Keep existing fields intact where possible. Schema can change pre-`1.0`, but avoid making agents relearn basic finding semantics.

## Data Model
Add workspace types in or near `src/detect.rs`:

```rust
pub struct WorkspaceInfo {
    pub root: String,
    pub kind: WorkspaceKind,
    pub members: Vec<ProjectInfo>,
}

pub enum WorkspaceKind {
    SingleProject,
    Cargo,
    Pnpm,
    Npm,
    Lerna,
    Turbo,
    Mixed,
}
```

Extend `ProjectInfo` with optional package metadata only if needed:

- `workspace_member_name`
- `workspace_relative_path`
- `package_manager`

Prefer additive fields with `#[serde(default)]` for compatibility.

## Detection Rules
Cargo:

- Root `Cargo.toml` with `[workspace]`.
- Parse `members = [...]` conservatively. Support explicit paths first; glob support can be a follow-up.

pnpm:

- Root `pnpm-workspace.yaml`.
- Parse `packages` globs conservatively or use a small YAML parser only if already acceptable. If avoiding dependencies, support common `apps/*` and `packages/*` patterns first with tests.

npm:

- Root `package.json` with `workspaces`.

lerna:

- `lerna.json` with `packages`.

turborepo:

- `turbo.json` plus package-manager workspaces. Turbo alone does not define members; use npm/pnpm/yarn workspace config.

## Execution Strategy
Add `detect_workspace(path) -> Result<WorkspaceInfo>`.

For `SingleProject`, keep current behavior.

For workspaces, run `run_verification`-equivalent logic per member. Do not recursively call the public `run_verification` if it causes repeated human output or report saving. Extract a lower-level function:

```rust
fn run_project_verification(project: ProjectInfo, cfg: &BarzelConfig, options: RunOptions) -> Result<BarzelReport>
```

Aggregate member reports into a repo report. Two options:

- Extend `BarzelReport` with `workspace_members: Vec<BarzelReport>`.
- Or add package path/name fields to every `LayerResult`.

Prefer `workspace_members` for clarity, then flatten action items in stdio.

## Config Policy
Root `.barzel.toml` applies to all workspace members by default.

Member `.barzel.toml` overrides root config for that member.

Do not invent a complex inheritance system in the first PR. Load root config, then if member config exists use member config entirely.

## Cache Policy
Structural cache should be scoped by member path and runner:

`.barzel/cache/<member-path>/<runner>.hash`

Update hashing to include language-appropriate files:

- Rust: `Cargo.toml`, `Cargo.lock`, `src/`, `tests/`
- TypeScript: `package.json`, lockfile, `src/`, `tests/`, `app/`, `pages/`, `e2e/`
- Python: `pyproject.toml`, `requirements*.txt`, `src/`, package dir, `tests/`

If full hash generalization is too large, disable structural caching for workspace members in the first monorepo PR and document the follow-up.

## Tests
Detection tests in `src/detect.rs`:

- single-project behavior unchanged
- Cargo workspace with two members
- npm workspaces from `package.json`
- pnpm workspace from `pnpm-workspace.yaml`
- lerna packages
- mixed-language members
- member AI framework detection still works

Run/orchestration tests:

- workspace result aggregates member failures
- action items include `package_path`
- root config applies to members
- member config overrides root config
- report persistence writes one repo-level report

## Acceptance Criteria
Existing single-project CLI behavior remains unchanged.

Stdio output gives agents enough package path context to reproduce every finding.

No runner body changes should be needed for basic workspace support; runners should receive member `ProjectInfo.root`.

`cargo test --bins` passes.

`cargo clippy -- -D warnings` passes.

Related notes: [[barzel-code-review-2026-05]], [[barzel-decision-record-2026-05]]
