//! Symbol extraction from parsed Python AST

use crate::{Error, Result, Symbol, SymbolKind};
use std::path::PathBuf;
use tree_sitter::Node;

pub struct SymbolExtractor<'a> {
    source: &'a [u8],
    path: PathBuf,
    symbols: &'a mut Vec<Symbol>,
    context_stack: Vec<Context>,
}

#[derive(Clone)]
struct Context {
    name: String,
    kind: ContextKind,
}

#[derive(Clone, Copy, PartialEq)]
enum ContextKind {
    Class,
    Function,
}

impl<'a> SymbolExtractor<'a> {
    pub fn new(source: &'a [u8], path: PathBuf, symbols: &'a mut Vec<Symbol>) -> Self {
        Self {
            source,
            path,
            symbols,
            context_stack: Vec::new(),
        }
    }

    pub fn visit_node(&mut self, node: Node) -> Result<()> {
        match node.kind() {
            "function_definition" | "async_function_definition" => {
                self.extract_function(node, false)?;
            }
            "class_definition" => {
                self.extract_class(node)?;
            }
            "decorated_definition" => {
                self.extract_decorated(node)?;
            }
            _ => {
                // Recursively visit children for other node types
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.visit_node(child)?;
                }
            }
        }
        Ok(())
    }

    fn extract_function(&mut self, node: Node, _is_async: bool) -> Result<()> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| Error::Parse("Function without name".to_string()))?;

        let name = self.get_node_text(name_node);
        let line = name_node.start_position().row + 1;
        let column = name_node.start_position().column;

        // Determine the symbol kind based on context
        let kind = match self.context_stack.last() {
            Some(ctx) if ctx.kind == ContextKind::Class => SymbolKind::Method,
            Some(ctx) if ctx.kind == ContextKind::Function => SymbolKind::NestedFunction,
            None => SymbolKind::Function,
            _ => SymbolKind::Function,
        };

        let mut symbol = Symbol::new(name.clone(), kind, self.path.clone(), line, column);

        // Set container name if we're inside another context
        if !self.context_stack.is_empty() {
            let container = self
                .context_stack
                .iter()
                .map(|ctx| ctx.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            symbol = symbol.with_container(container);
        }

        // Set module path
        let module = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        symbol = symbol.with_module(module);

        self.symbols.push(symbol);

        // Visit function body with updated context
        self.context_stack.push(Context {
            name: name.clone(),
            kind: ContextKind::Function,
        });

        // Visit function body
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_node(body)?;
        }

        self.context_stack.pop();
        Ok(())
    }

    fn extract_class(&mut self, node: Node) -> Result<()> {
        let name_node = node
            .child_by_field_name("name")
            .ok_or_else(|| Error::Parse("Class without name".to_string()))?;

        let name = self.get_node_text(name_node);
        let line = name_node.start_position().row + 1;
        let column = name_node.start_position().column;

        // Determine if this is a nested class
        let kind = if self
            .context_stack
            .iter()
            .any(|ctx| ctx.kind == ContextKind::Class)
        {
            SymbolKind::NestedClass
        } else {
            SymbolKind::Class
        };

        let mut symbol = Symbol::new(name.clone(), kind, self.path.clone(), line, column);

        // Set container name if we're inside another context
        if !self.context_stack.is_empty() {
            let container = self
                .context_stack
                .iter()
                .map(|ctx| ctx.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            symbol = symbol.with_container(container);
        }

        // Set module path
        let module = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        symbol = symbol.with_module(module);

        self.symbols.push(symbol);

        // Visit class body with updated context
        self.context_stack.push(Context {
            name: name.clone(),
            kind: ContextKind::Class,
        });

        // Visit class body
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_node(body)?;
        }

        self.context_stack.pop();
        Ok(())
    }

    fn extract_decorated(&mut self, node: Node) -> Result<()> {
        // For decorated definitions, we need to extract the actual definition
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "function_definition" => {
                    self.extract_function(child, false)?;
                    break;
                }
                "async_function_definition" => {
                    self.extract_function(child, true)?;
                    break;
                }
                "class_definition" => {
                    self.extract_class(child)?;
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn get_node_text(&self, node: Node) -> String {
        std::str::from_utf8(&self.source[node.byte_range()])
            .unwrap_or("")
            .to_string()
    }
}
