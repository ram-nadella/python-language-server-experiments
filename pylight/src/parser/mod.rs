//! Python parsing module with multiple backend support

pub mod extractor;
pub mod python_parser;
pub mod ruff;
pub mod r#trait;
pub mod tree_sitter;

pub use python_parser::PythonParser;
pub use r#trait::Parser;
pub use ruff::RuffParser;
pub use tree_sitter::TreeSitterParser;

use crate::Result;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserBackend {
    TreeSitter,
    Ruff,
}

impl FromStr for ParserBackend {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tree-sitter" | "treesitter" => Ok(Self::TreeSitter),
            "ruff" => Ok(Self::Ruff),
            _ => Err(format!("Invalid parser backend: {s}")),
        }
    }
}

/// Create a parser instance based on the specified backend
pub fn create_parser(backend: ParserBackend) -> Result<Arc<dyn Parser>> {
    match backend {
        ParserBackend::TreeSitter => Ok(Arc::new(TreeSitterParser::new()?)),
        ParserBackend::Ruff => Ok(Arc::new(RuffParser::new())),
    }
}

#[cfg(test)]
mod tests;
