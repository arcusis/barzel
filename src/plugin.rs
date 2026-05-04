use crate::detect::ProjectInfo;
use crate::error::Result;
use crate::report::{Finding, LayerResult};

pub trait TestRunner: Send + Sync {
    fn name(&self) -> &'static str;
    fn layer(&self) -> Layer;
    fn is_available(&self, project: &ProjectInfo) -> bool;
    fn run(&self, project: &ProjectInfo) -> Result<LayerResult>;

    fn skip_message(&self) -> &'static str {
        "Runner not available — tool not installed"
    }

    #[allow(dead_code)]
    fn parse_results(&self, _raw_output: &str) -> Vec<Finding> {
        vec![]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Logic,
    Structural,
    Hostile,
    #[allow(dead_code)]
    Operational,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Logic => "logic",
            Layer::Structural => "structural",
            Layer::Hostile => "hostile",
            Layer::Operational => "operational",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::ProjectInfo;

    struct MinimalRunner;
    impl TestRunner for MinimalRunner {
        fn name(&self) -> &'static str {
            "minimal"
        }
        fn layer(&self) -> Layer {
            Layer::Logic
        }
        fn is_available(&self, _: &ProjectInfo) -> bool {
            false
        }
        fn run(&self, _: &ProjectInfo) -> Result<LayerResult> {
            unreachable!()
        }
        // Uses default skip_message and parse_results
    }

    #[test]
    fn layer_as_str_is_correct() {
        assert_eq!(Layer::Logic.as_str(), "logic");
        assert_eq!(Layer::Structural.as_str(), "structural");
        assert_eq!(Layer::Hostile.as_str(), "hostile");
        assert_eq!(Layer::Operational.as_str(), "operational");
    }

    #[test]
    fn default_skip_message_is_nonempty() {
        assert!(!MinimalRunner.skip_message().is_empty());
        assert!(MinimalRunner.skip_message().contains("not"));
    }

    #[test]
    fn default_parse_results_returns_empty() {
        let findings = MinimalRunner.parse_results("some output");
        assert!(findings.is_empty());
    }

    #[test]
    fn layer_equality() {
        assert_eq!(Layer::Logic, Layer::Logic);
        assert_ne!(Layer::Logic, Layer::Structural);
        assert_ne!(Layer::Hostile, Layer::Operational);
    }
}
