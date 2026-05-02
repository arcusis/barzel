# Barzel — The Unbreakable Testing CLI

Barzel is a Rust CLI designed to be the ultimate testing orchestrator for AI coding agents and human engineers who demand the highest possible software integrity.

It implements four verification layers:

- **Logic** — Property-Based Testing + Design by Contract
- **Structural** — Mutation Testing (≥95% target) + MC/DC
- **Hostile** — Continuous Fuzzing + SAST/DAST
- **Operational** — Chaos Engineering + Shadowing (coming soon)

## Installation

```bash
cargo install barzel
```

Or build from source:

```bash
git clone https://github.com/arcusis/barzel
cd barzel
cargo build --release
```

## Usage

### Human Mode

```bash
# Initialize Barzel in your project
barzel init .

# Run the full verification suite
barzel run

# Run specific layers
barzel run --layer logic,structural
```

### AI Agent Mode (stdio JSON)

Barzel is designed to be driven by AI coding tools via a strict JSON protocol:

```bash
echo '{"command":"init","project_path":"."}' | barzel --stdio
echo '{"command":"run","layers":["logic","structural"]}' | barzel --stdio
```

Every response includes `request_id`, `timestamp`, and `version` for reliable correlation.

## Configuration

Barzel creates a `.barzel.toml` file with sensible defaults:

```toml
[project]
name = "my-project"
language = "rust"

[layers]
enabled = ["logic", "structural", "hostile"]

[layers.logic]
property_based = true
design_by_contract = true

[layers.structural]
mutation_testing = true
mcdc_coverage = true

[layers.hostile]
fuzzing = false
sast = true
```

## Layers

### Logic Layer
- Detects `proptest` usage
- Runs property-based tests when present
- Provides clear guidance when PBT is missing

### Structural Layer
- Detects `cargo-mutants`
- Reports mutation score
- Fails builds below 95% mutation score (configurable)

### Hostile Layer
- Integrates Semgrep for SAST
- Reports security findings with severity

## Exit Codes

- `0` — Success / all checks passed
- `1` — Failures or errors found
- `2` — Usage error

## Contributing

Barzel follows strict engineering standards:
- Small, correct patches only
- `cargo clippy -- -D warnings` must pass
- Targeted tests required for new functionality

## License

Apache-2.0
