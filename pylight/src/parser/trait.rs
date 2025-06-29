//! Parser trait abstraction for different Python parsing backends

use crate::{Result, Symbol};
use std::path::Path;

/// Trait for Python parsers that can extract symbols from source code
pub trait Parser: Send + Sync {
    /// Parse a Python file and extract symbols
    fn parse_file(&self, path: &Path, content: &str) -> Result<Vec<Symbol>>;

    /// Get the name of the parser backend
    fn backend_name(&self) -> &'static str;
}
