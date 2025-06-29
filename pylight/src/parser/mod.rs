//! Python parsing module using tree-sitter

pub mod extractor;
pub mod python_parser;

pub use python_parser::PythonParser;

#[cfg(test)]
mod tests;
