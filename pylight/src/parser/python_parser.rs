//! Python parser implementation

use crate::{Error, Result, Symbol};
use std::path::Path;
use tree_sitter::Parser;

use super::extractor::SymbolExtractor;

pub struct PythonParser {
    parser: Parser,
}

impl PythonParser {
    pub fn new() -> Result<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(tree_sitter_python::language())
            .map_err(|e| Error::Parse(format!("Failed to set language: {e}")))?;
        Ok(Self { parser })
    }

    pub fn parse_file(&mut self, path: &Path, content: &str) -> Result<Vec<Symbol>> {
        let tree = self
            .parser
            .parse(content, None)
            .ok_or_else(|| Error::Parse("Failed to parse file".to_string()))?;

        let mut symbols = Vec::new();
        let mut extractor =
            SymbolExtractor::new(content.as_bytes(), path.to_path_buf(), &mut symbols);

        extractor.visit_node(tree.root_node())?;
        Ok(symbols)
    }
}
