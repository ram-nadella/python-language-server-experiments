//! Tree-sitter based Python parser implementation

use crate::{Error, Result, Symbol};
use std::path::Path;
use std::sync::Mutex;
use tree_sitter::Parser as TSParser;

use super::extractor::SymbolExtractor;
use super::r#trait::Parser;

pub struct TreeSitterParser {
    parser: Mutex<TSParser>,
}

impl TreeSitterParser {
    pub fn new() -> Result<Self> {
        let mut parser = TSParser::new();
        parser
            .set_language(tree_sitter_python::language())
            .map_err(|e| Error::Parse(format!("Failed to set language: {e}")))?;
        Ok(Self {
            parser: Mutex::new(parser),
        })
    }
}

impl Parser for TreeSitterParser {
    fn parse_file(&self, path: &Path, content: &str) -> Result<Vec<Symbol>> {
        let mut parser = self.parser.lock().unwrap();

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| Error::Parse("Failed to parse file".to_string()))?;

        let mut symbols = Vec::new();
        let mut extractor =
            SymbolExtractor::new(content.as_bytes(), path.to_path_buf(), &mut symbols);

        extractor.visit_node(tree.root_node())?;
        Ok(symbols)
    }

    fn backend_name(&self) -> &'static str {
        "tree-sitter"
    }
}
