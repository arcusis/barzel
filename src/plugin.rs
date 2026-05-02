use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::report::{Finding, LayerResult};

/// The core trait that every testing tool/plugin must implement.
///
/// This allows the orchestrator to treat all tools (proptest, cargo-mutants,
/// semgrep, Stryker, etc.) in a uniform way, regardless of language or
/// underlying implementation.
pub trait TestRunner: Send + Sync {
    /// Human-readable name of the runner (e.g., "proptest", "cargo-mutants")
    fn name(&self) -> &'static str;

    /// Which verification layer this runner belongs to
    #[allow(dead_code)]
    fn layer(&self) -> Layer;

    /// Quick check whether this runner can operate on the given project
    fn is_available(&self, project: &ProjectInfo) -> bool;

    /// Execute the test runner and return structured results
    fn run(&self, project: &ProjectInfo) -> Result<LayerResult>;

    /// Optional: parse raw tool output into Findings (for tools that output text)
    #[allow(dead_code)]
    fn parse_results(&self, _raw_output: &str) -> Vec<Finding> {
        vec![]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Layer {
    Logic,
    Structural,
    Hostile,
    Operational,
}
