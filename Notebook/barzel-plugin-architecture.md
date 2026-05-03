# Plugin Architecture — TestRunner Trait

**Requirement:** Every external testing tool must implement a common Rust trait so the orchestrator remains generic.

**Proposed Trait (v1):**

```rust
pub trait TestRunner: Send + Sync {
    fn name(&self) -> &str;
    fn layer(&self) -> Layer;
    fn is_available(&self, project: &ProjectInfo) -> bool;
    fn run(&self, project: &ProjectInfo, config: &LayerConfig) -> Result<LayerResult>;
    fn parse_results(&self, raw_output: &str) -> Vec<Finding>;
}
```

**Benefits:**
- Orchestrator only knows about `TestRunner`
- Easy to add new tools (new language, new fuzzer, new SAST) without touching core logic
- AI agents can even register custom runners via future plugin system

**Caching Strategy (v1):**
- Hash of `Cargo.toml` + `src/**/*.rs` (or equivalent for other languages)
- If hash unchanged since last run → skip expensive layers (fuzzing, mutation)
- Store last successful report + hash in `.barzel/cache/`
- Configurable TTL per layer

**Language Agnostic Goal:**
- The trait + detection system should eventually support:
  - Rust (proptest, cargo-mutants, cargo-fuzz, semgrep)
  - TypeScript (fast-check, Stryker, Semgrep)
  - Go (quickcheck, go-mutesting, go-fuzz)
  - Python, etc.

This architecture makes Barzel the "universal testing orchestrator".
