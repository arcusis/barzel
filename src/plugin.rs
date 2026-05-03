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
