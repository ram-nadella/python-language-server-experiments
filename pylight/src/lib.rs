//! Pylight - A high-performance Python symbol search language server
//!
//! This crate provides fast workspace-wide symbol search capabilities for Python code
//! using tree-sitter for parsing and fuzzy matching for search.

pub mod error;
pub mod file_filter;
pub mod index;
pub mod lsp;
pub mod parser;
pub mod search;
pub mod string_cache;
pub mod symbols;
pub mod watcher;

pub use error::{Error, Result};
pub use index::SymbolIndex;
pub use lsp::LspServer;
pub use parser::PythonParser;
pub use search::SearchEngine;
pub use symbols::{Symbol, SymbolKind};
