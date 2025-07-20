//! Symbol definitions and types

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    NestedFunction,
    NestedClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: Arc<str>,
    pub line: usize,
    pub column: usize,
    pub container_name: Option<String>,
    pub module_path: Arc<str>,
}

// Custom Serialize/Deserialize to handle Arc<str>
impl Serialize for Symbol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Symbol", 7)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("kind", &self.kind)?;
        state.serialize_field("file_path", &self.file_path.as_ref())?;
        state.serialize_field("line", &self.line)?;
        state.serialize_field("column", &self.column)?;
        state.serialize_field("container_name", &self.container_name)?;
        state.serialize_field("module_path", &self.module_path.as_ref())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SymbolData {
            name: String,
            kind: SymbolKind,
            file_path: String,
            line: usize,
            column: usize,
            container_name: Option<String>,
            module_path: String,
        }

        let data = SymbolData::deserialize(deserializer)?;
        Ok(Symbol {
            name: data.name,
            kind: data.kind,
            file_path: Arc::from(data.file_path.as_str()),
            line: data.line,
            column: data.column,
            container_name: data.container_name,
            module_path: Arc::from(data.module_path.as_str()),
        })
    }
}

impl Symbol {
    pub fn new(
        name: String,
        kind: SymbolKind,
        file_path: String,
        line: usize,
        column: usize,
    ) -> Self {
        Self {
            name,
            kind,
            file_path: Arc::from(file_path),
            line,
            column,
            container_name: None,
            module_path: Arc::from(""),
        }
    }

    pub fn with_container(mut self, container: String) -> Self {
        self.container_name = Some(container);
        self
    }

    pub fn with_module(mut self, module: String) -> Self {
        self.module_path = Arc::from(module.as_str());
        self
    }
}
