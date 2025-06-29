//! Ruff-based Python parser implementation

use crate::{Error, Result, Symbol, SymbolKind};
use ruff_python_ast::{
    visitor::{self, Visitor},
    Mod, Stmt,
};
use ruff_python_parser::{parse, Mode};
use ruff_source_file::{LineIndex, SourceCode};
use std::path::{Path, PathBuf};

use super::r#trait::Parser;

pub struct RuffParser;

impl RuffParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuffParser {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
enum Context {
    Class(String),
    Function(String),
}

struct SymbolExtractor<'a> {
    symbols: &'a mut Vec<Symbol>,
    file_path: PathBuf,
    context_stack: Vec<Context>,
    source_code: SourceCode<'a, 'a>,
}

impl<'a> SymbolExtractor<'a> {
    fn new(
        symbols: &'a mut Vec<Symbol>,
        file_path: PathBuf,
        source: &'a str,
        line_index: &'a LineIndex,
    ) -> Self {
        Self {
            symbols,
            file_path,
            context_stack: Vec::new(),
            source_code: SourceCode::new(source, line_index),
        }
    }

    fn determine_function_kind(&self) -> SymbolKind {
        // Check if we're inside a class context
        for ctx in &self.context_stack {
            if matches!(ctx, Context::Class(_)) {
                return SymbolKind::Method;
            }
        }

        // Check if we're inside a function context (nested function)
        if self
            .context_stack
            .iter()
            .any(|ctx| matches!(ctx, Context::Function(_)))
        {
            SymbolKind::NestedFunction
        } else {
            SymbolKind::Function
        }
    }

    fn determine_class_kind(&self) -> SymbolKind {
        if self.context_stack.is_empty() {
            SymbolKind::Class
        } else {
            SymbolKind::NestedClass
        }
    }

    fn build_container_name(&self) -> Option<String> {
        if self.context_stack.is_empty() {
            None
        } else {
            Some(
                self.context_stack
                    .iter()
                    .map(|ctx| match ctx {
                        Context::Class(name) | Context::Function(name) => name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("."),
            )
        }
    }

    fn get_line_column(&self, offset: u32) -> (usize, usize) {
        let location = self
            .source_code
            .source_location(offset.into(), ruff_source_file::PositionEncoding::Utf8);
        // Both line and column are 1-based in Ruff
        (location.line.get(), location.character_offset.get())
    }
}

impl<'a> Visitor<'a> for SymbolExtractor<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(func_def) => {
                let name_str = func_def.name.to_string();
                // Note: func_def.is_async is available to check if it's async
                let kind = self.determine_function_kind();
                let container = self.build_container_name();
                // Use the name's range to get the line of the actual 'def' keyword
                let (line, column) = self.get_line_column(func_def.name.range.start().to_u32());

                // Get module name from file path
                let module_path = self
                    .file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                let mut symbol =
                    Symbol::new(name_str.clone(), kind, self.file_path.clone(), line, column);

                if let Some(container) = container {
                    symbol = symbol.with_container(container);
                }

                symbol = symbol.with_module(module_path);

                self.symbols.push(symbol);

                self.context_stack.push(Context::Function(name_str));
                visitor::walk_stmt(self, stmt);
                self.context_stack.pop();
            }
            Stmt::ClassDef(class_def) => {
                let name_str = class_def.name.to_string();
                let kind = self.determine_class_kind();
                let container = self.build_container_name();
                // Use the name's range to get the line of the actual 'class' keyword
                let (line, column) = self.get_line_column(class_def.name.range.start().to_u32());

                // Get module name from file path
                let module_path = self
                    .file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                let mut symbol =
                    Symbol::new(name_str.clone(), kind, self.file_path.clone(), line, column);

                if let Some(container) = container {
                    symbol = symbol.with_container(container);
                }

                symbol = symbol.with_module(module_path);

                self.symbols.push(symbol);

                self.context_stack.push(Context::Class(name_str));
                visitor::walk_stmt(self, stmt);
                self.context_stack.pop();
            }
            _ => visitor::walk_stmt(self, stmt),
        }
    }
}

impl Parser for RuffParser {
    fn parse_file(&self, file_path: &Path, source: &str) -> Result<Vec<Symbol>> {
        let line_index = LineIndex::from_source_text(source);

        let parsed = parse(source, Mode::Module.into())
            .map_err(|e| Error::Parse(format!("Ruff parse error: {e:?}")))?;

        let mut symbols = Vec::new();
        let mut extractor =
            SymbolExtractor::new(&mut symbols, file_path.to_path_buf(), source, &line_index);

        match parsed.syntax() {
            Mod::Module(module) => {
                for stmt in &module.body {
                    extractor.visit_stmt(stmt);
                }
            }
            Mod::Expression(_) => {
                // Not handling expression mode
            }
        }

        Ok(symbols)
    }

    fn backend_name(&self) -> &'static str {
        "ruff"
    }
}
