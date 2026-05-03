# Barzel Decision Record 2026-05

## Product Scope
Barzel is local-agent infrastructure first. CI/CD support matters only when an AI agent is in the loop. The CLI remains a single local binary with no server, no cloud API, and no hosted report service.

Enterprise teams and open-source users are the target audience. The stdio schema can still change before `1.0`, so near-term work may reshape JSON if it improves agent usability.

## Agent Boundary
Barzel reports evidence. It does not repair code, write fix plans, or mutate a project. There will be no `barzel run --fix`.

The report contract is the product: findings, severity, layer, runner, location when known, reproduce command, suggestion, metrics, status, report ID, and exit code.

## DAP Policy
All four layers remain the target: Logic, Structural, Hostile, Operational. Missing tools and missing layers are user configuration choices via `.barzel.toml`; Barzel should not override the user's agent policy.

Thresholds are configurable. Defaults should be opinionated but not hard-coded as universal truth.

## Runner Policy
Depth wins over breadth for Rust, TypeScript, and Python. Go remains supported, but the next expansion should strengthen the first three languages before adding Ruby, Java, or Kotlin.

Dependency vulnerability scanning belongs in the Hostile layer.

All official runners live inside the binary for trust, portability, and simple installation.

## Next Three PRs
1. [[barzel-pr1-parallel-layer-execution-plan]]
2. [[barzel-pr2-dependency-vulnerability-scanning-plan]]
3. [[barzel-pr3-monorepo-workspace-support-plan]]

Related notes: [[barzel-code-review-2026-05]], [[barzel-protocol]], [[barzel-architecture-v1]]
