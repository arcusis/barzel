# Barzel — The Unbreakable Testing CLI

Barzel is a local Rust CLI for running a full verification suite against a project and producing structured, machine-readable reports. It is designed to be driven by AI coding agents via a stdio JSON protocol; human output is a secondary interface.

**Product decisions:** local binary only, report-only (no `--fix`, no code mutation), no cloud API, no server. The stdio schema is pre-1.0 and may change.

## Installation

```bash
# From source (only option until crates.io release)
git clone https://github.com/arcusis/barzel
cd barzel
cargo build --release
# Binary is at target/release/barzel
```

## Verification layers

| Layer | What runs |
|-------|-----------|
| **logic** | proptest, fast-check, cargo-kani (formal verification), pytest, jest, go test, mypy |
| **structural** | cargo-mutants, stryker, mutmut, go-mutesting |
| **hostile** | cargo-fuzz, cargo-audit, npm-audit, pip-audit, semgrep, bandit, eslint, aisec |
| **operational** | HTTP health checks, custom commands (configured in `.barzel.toml`) |

Language matrix: Rust, TypeScript, Python, Go. `semgrep` runs on all languages. `aisec` runs on any project with detected AI framework dependencies.

## Human CLI

```bash
barzel init .                          # write .barzel.toml with defaults
barzel check                           # show tool availability and required installs
barzel run                             # run all layers, human output
barzel run --layer logic,hostile       # run specific layers
barzel run --json                      # run all layers, output final JSON report to stdout
barzel run --since HEAD~1              # only run packages with changes since a git ref
barzel run --no-cache                  # bypass structural cache
barzel run --fail-fast                 # stop on first critical finding
barzel report                          # print the latest report
barzel report <id-prefix>              # print a specific report by ID prefix
barzel report --compare <base> <head>  # compare two reports for regressions
```

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Pass — no findings at or above the `fail_on` threshold |
| `1` | Fail — one or more findings at or above the `fail_on` threshold |
| `2` | Partial — run completed but findings exist below the `fail_on` threshold, or some layers are partial without triggering the threshold |

The `fail_on` threshold maps to the `reporting.fail_on` config key (default `high`). `fail_on=low` excludes `info` findings. `fail_on=any` includes `info` findings.

## AI agent mode (stdio JSON)

Send a single JSON object to stdin; barzel writes newline-delimited JSON to stdout and exits.

```bash
echo '{"command":"init","project_path":"."}' | barzel --stdio
echo '{"command":"check","project_path":"."}' | barzel --stdio
echo '{"command":"run"}' | barzel --stdio
echo '{"command":"run","layers":["logic"],"fail_fast":true}' | barzel --stdio
echo '{"command":"run","since":"HEAD~1"}' | barzel --stdio
echo '{"command":"report"}' | barzel --stdio
echo '{"command":"report","id":"abc123"}' | barzel --stdio
echo '{"command":"report","compare":["baseline-id","head-id"]}' | barzel --stdio
echo '{"command":"history","limit":10,"package_path":"crates/api"}' | barzel --stdio
```

### Request fields

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `command` | string | required | `init`, `check`, `run`, `report`, `history` |
| `request_id` | string | auto-generated | echoed in every response line |
| `project_path` | string | `"."` | absolute or relative path |
| `layers` | string[] | all enabled | subset of `logic`, `structural`, `hostile`, `operational` |
| `no_cache` | bool | false | bypass structural layer cache |
| `fail_fast` | bool | false | stop on first failure |
| `since` | string | — | git ref for diff mode (e.g. `"HEAD~1"`, `"main"`) |
| `id` | string | `"latest"` | report ID prefix or `"latest"` (for `report` command) |
| `compare` | string[2] | — | `[baseline-id, head-id]` (for `report --compare`) |
| `limit` | int | 20 | max history entries (cap 200, 0 returns empty) |
| `package_path` | string | — | filter history by workspace member path |
| `language` | string | — | filter history by language |

### Response envelope

Every JSON line — progress events and the final response — shares this envelope:

```json
{
  "status": "progress" | "success" | "error",
  "request_id": "...",
  "timestamp": "2026-05-04T12:00:00Z",
  "version": "1",
  "data": { ... },
  "error": null
}
```

### Progress events (run command only)

Before the final `success` or `error` response, the `run` command emits one progress line per runner that actually executes (unavailable and cached runners emit no events):

```json
{"status":"progress","request_id":"...","timestamp":"...","version":"1","data":{
  "event": "runner_started",
  "runner": "cargo-mutants",
  "layer": "structural",
  "package_path": "crates/api",
  "runner_status": null,
  "duration_ms": null
}}
```

```json
{"status":"progress","request_id":"...","timestamp":"...","version":"1","data":{
  "event": "runner_completed",
  "runner": "cargo-mutants",
  "layer": "structural",
  "package_path": "crates/api",
  "runner_status": "pass",
  "duration_ms": 4210
}}
```

`package_path` is the workspace member path (e.g. `"crates/api"`) for workspace runs, or `null` for single-project runs. Agents that read only the final response line continue to work without changes.

### Run response (`data` shape)

Top-level fields (always present):

```json
{
  "passed":         false,
  "overall_status": "fail",
  "total_findings": 1,
  "critical":       0,
  "high":           1,
  "medium":         0,
  "low":            0,
  "report_id":      "...",
  "timestamp":      "2026-05-04T12:00:00Z",
  "layers": [
    {
      "name":           "hostile",
      "runner":         "semgrep",
      "status":         "fail",
      "tests_run":      0,
      "passed":         0,
      "failed":         1,
      "mutation_score": null,
      "duration_ms":    310,
      "findings":       [ { "severity": "high", "code": "HARDCODED_SECRET", "message": "...", "location": "src/config.rs:12", "reproduce_cmd": "...", "suggestion": "..." } ]
    }
  ],
  "action_items": [
    {
      "priority":     1,
      "layer":        "hostile",
      "runner":       "semgrep",
      "severity":     "high",
      "code":         "HARDCODED_SECRET",
      "message":      "...",
      "location":     "src/config.rs:12",
      "reproduce_cmd": "semgrep --config auto src/",
      "suggestion":   "..."
    }
  ]
}
```

For **single-project** runs: `action_items` do not include `package_path`. For **workspace** runs, these additional fields are present:

```json
{
  "is_workspace": true,
  "packages": [
    {
      "package_path": "crates/api",
      "language":     "rust",
      "status":       "fail",
      "summary": { "total_findings": 1, "critical": 0, "high": 1, "medium": 0, "low": 0, "overall_status": "fail" },
      "layers":   [ { "name": "hostile", "runner": "semgrep", "status": "fail", "tests_run": 0, "passed": 0, "failed": 1, "mutation_score": null, "duration_ms": 310, "findings": [] } ]
    }
  ]
}
```

And workspace `action_items` include `"package_path": "crates/api"` on each entry.

`action_items` are sorted by severity priority (critical first). Every `action_item` has a non-empty `reproduce_cmd`. Info-severity findings are excluded from `action_items`. `layers` at the top level is always present and contains the aggregate across all members.

### History response (`data` shape)

```json
{
  "entries": [
    {
      "report_id": "...",
      "timestamp": "...",
      "project": "myapp",
      "package_path": "crates/api",
      "language": "rust",
      "status": "pass",
      "layers": [
        { "runner": "cargo-mutants", "status": "pass", "mutation_score": 96.2, "coverage": null }
      ]
    }
  ],
  "total": 3
}
```

## .barzel.toml reference

`barzel init` writes a valid config. Keys that matter to agents:

```toml
[layers]
# Which layers to run. Valid values: logic, structural, hostile, operational.
enabled = ["logic", "structural", "hostile", "operational"]

[layers.logic]
property_based = true
formal_verification = true
# Minimum coverage % to pass the logic layer (0–100). Omit to disable.
# min_coverage = 80.0

[layers.structural]
mutation_testing = true
# Minimum mutation score % to pass (0.0–100.0). Default: 95.0.
mutation_threshold = 95.0

[layers.hostile]
fuzzing = true
sast = true

[[layers.operational.health_checks]]
name = "api-health"
url = "http://localhost:8080/health"
# expected_status = 200  # default

[[layers.operational.commands]]
name = "db-migrate-check"
cmd = "python"
args = ["manage.py", "migrate", "--check"]
cwd = "backend"          # relative to project root; omit to use project root
timeout_ms = 10000       # default: 30000

[reporting]
format = "json"
# Minimum severity to fail: critical, high, medium, low, any. Default: high.
# "low" fails on low/medium/high/critical but not info.
# "any" fails on everything including info.
fail_on = "high"

[history]
enabled = true
max_entries_per_package = 50
# Regression tolerance as a fraction (0.0–1.0). 0.0 = any drop fires a finding.
# 0.02 means tolerate drops up to 2 percentage points before firing.
# Values above 1.0 are clamped to 1.0 (suppresses all regression findings).
coverage_regression_tolerance = 0.02
mutation_regression_tolerance = 0.02
```

All config values are validated before any runner fires. Invalid TOML or out-of-range thresholds return a structured error response.

## Workspace support

Barzel detects Cargo workspaces, pnpm workspaces, npm workspaces, lerna, and turborepo. For each detected member, it runs the appropriate language runner matrix and produces per-package results in `workspace_members`. A member-local `.barzel.toml` overrides the root config for that member only.

`barzel run --since HEAD~1` limits execution to members with changed files, always including security/audit runners when lockfiles change.

## Report storage

Reports are saved to `.barzel/reports/` as JSON files. Use `barzel report` or `{"command":"report"}` to retrieve them. IDs support prefix lookup; `"latest"` resolves to the most recent.

## Contributing

- `cargo clippy -- -D warnings` must pass
- `cargo test --bins` must pass
- No `cargo fmt` / no repo-wide formatting
- Small, correct patches only

## License

Apache-2.0
