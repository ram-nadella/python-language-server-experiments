use std::path::{Path, PathBuf};
use std::collections::{HashMap, HashSet};
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use serde::{Serialize, Deserialize};
use anyhow::{Context as AnyhowContext, Result};
use tree_sitter::{Parser, Node};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::BufWriter;
use tracing::info;

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum SymbolType {
    Function,
    Class,
    Method,
    NestedFunction,
    NestedClass,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParentContext {
    pub name: String,
    pub symbol_type: SymbolType,
    pub line_number: usize,
}

#[derive(Debug, Clone, Default)]
pub struct PathRegistry {
    // Store all paths in a vector
    pub paths: Vec<PathBuf>,
    // Use a HashMap for fast lookups - maps paths to indices
    pub path_to_index: HashMap<PathBuf, usize>,
}

impl PathRegistry {
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            path_to_index: HashMap::new(),
        }
    }

    // Get or create an index for this path
    pub fn register_path(&mut self, path: PathBuf) -> usize {
        // Look up or insert path
        if let Some(&idx) = self.path_to_index.get(&path) {
            return idx;
        }
        
        // Add new path
        let new_idx = self.paths.len();
        self.paths.push(path.clone());
        self.path_to_index.insert(path, new_idx);
        
        new_idx
    }

    // Get path from index
    pub fn get_path(&self, index: usize) -> &PathBuf {
        &self.paths[index]
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.path_to_index.clear();
    }

    pub fn total_path_bytes(&self) -> usize {
        // Simple estimate of memory usage
        let paths_size = self.paths.capacity() * mem::size_of::<PathBuf>();
        let map_size = self.path_to_index.capacity() * (mem::size_of::<PathBuf>() + mem::size_of::<usize>());
        paths_size + map_size
    }

    pub fn print_stats(&self) {
        info!("  Unique paths: {}", self.paths.len());
        info!("  Path mapping entries: {}", self.path_to_index.len());
        info!("  Estimated memory usage: {} bytes", self.total_path_bytes());
    }

    // Add a method to help debug path issues
    pub fn debug_path_info(&self, file_path_index: usize) -> String {
        if file_path_index >= self.paths.len() {
            return format!("ERROR: Index {} out of bounds (max index: {})", 
                          file_path_index, self.paths.len().saturating_sub(1));
        }

        let path = &self.paths[file_path_index];
        let lookup_index = self.path_to_index.get(path);
        
        format!("Path[{}]: '{}', HashMap lookup index: {:?}", 
               file_path_index, path.display(), lookup_index)
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolContext {
    pub file_path_index: usize,  // Index into PathRegistry
    pub line_number: usize,
    pub module: String,
    pub fully_qualified_module: String,
    pub symbol_type: SymbolType,
    pub parent_context: Vec<ParentContext>,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub context: SymbolContext,
}

#[derive(Debug, Default)]
pub struct SymbolStats {
    pub functions: Arc<Mutex<HashSet<Symbol>>>,
    pub classes: Arc<Mutex<HashSet<Symbol>>>,
    pub syntax_errors: AtomicUsize,
    pub io_errors: AtomicUsize,
    pub other_errors: AtomicUsize,
    pub path_registry: Arc<Mutex<PathRegistry>>,
}

impl SymbolStats {
    pub fn new() -> Self {
        Self {
            functions: Arc::new(Mutex::new(HashSet::new())),
            classes: Arc::new(Mutex::new(HashSet::new())),
            syntax_errors: AtomicUsize::new(0),
            io_errors: AtomicUsize::new(0),
            other_errors: AtomicUsize::new(0),
            path_registry: Arc::new(Mutex::new(PathRegistry::new())),
        }
    }

    pub fn get_counts(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.functions.lock().unwrap().len(),
            self.classes.lock().unwrap().len(),
            self.syntax_errors.load(Ordering::Relaxed),
            self.io_errors.load(Ordering::Relaxed),
            self.other_errors.load(Ordering::Relaxed),
        )
    }
}

// Versioned data structures
#[derive(Serialize, Deserialize)]
pub struct SymbolDataV1 {
    pub version: u32,
    pub functions: Vec<Symbol>,
    pub classes: Vec<Symbol>,
    pub paths: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
pub enum SymbolData {
    V1(SymbolDataV1),
}

impl SymbolData {
    pub fn new(functions: Vec<Symbol>, classes: Vec<Symbol>, paths: Vec<PathBuf>) -> Self {
        SymbolData::V1(SymbolDataV1 {
            version: 1,
            functions,
            classes,
            paths,
        })
    }

    pub fn into_symbols(self) -> (Vec<Symbol>, Vec<Symbol>, Vec<PathBuf>) {
        match self {
            SymbolData::V1(data) => (data.functions, data.classes, data.paths),
        }
    }
}

pub fn get_module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

pub fn get_fully_qualified_module(path: &Path, base_dir: &Path) -> String {
    path.strip_prefix(base_dir)
        .ok()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().replace('/', "."))
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn get_node_text(node: Node, source: &str) -> String {
    source[node.byte_range()].to_string()
}

pub fn collect_symbols(
    node: Node, 
    source: &str, 
    file_path: &Path,
    base_dir: &Path,
    current_parents: &[ParentContext],
    path_registry: &mut PathRegistry,
) -> (Vec<Symbol>, Vec<Symbol>) {
    let mut function_symbols = Vec::new();
    let mut class_symbols = Vec::new();
    let module = get_module_name(file_path);
    let fully_qualified_module = get_fully_qualified_module(file_path, base_dir);
    let file_path_index = path_registry.register_path(file_path.to_path_buf());

    let mut cursor = node.walk();
    cursor.goto_first_child();

    loop {
        let node = cursor.node();
        match node.kind() {
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = get_node_text(name_node, source);
                    let symbol_type = if current_parents.is_empty() {
                        SymbolType::Function
                    } else if current_parents.last().map_or(false, |p| matches!(p.symbol_type, SymbolType::Class)) {
                        SymbolType::Method
                    } else {
                        SymbolType::NestedFunction
                    };
                    
                    let context = SymbolContext {
                        file_path_index,
                        line_number: node.start_position().row + 1,
                        module: module.clone(),
                        fully_qualified_module: fully_qualified_module.clone(),
                        symbol_type: symbol_type.clone(),
                        parent_context: current_parents.to_vec(),
                    };
                    function_symbols.push(Symbol { name: name.clone(), context });

                    // Check for nested functions
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut new_context = current_parents.to_vec();
                        new_context.push(ParentContext {
                            name,
                            symbol_type,
                            line_number: node.start_position().row + 1,
                        });
                        
                        let (nested_functions, nested_classes) = collect_symbols(
                            body,
                            source,
                            file_path,
                            base_dir,
                            &new_context,
                            path_registry,
                        );
                        function_symbols.extend(nested_functions);
                        class_symbols.extend(nested_classes);
                    }
                }
            }
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = get_node_text(name_node, source);
                    let symbol_type = if current_parents.is_empty() {
                        SymbolType::Class
                    } else {
                        SymbolType::NestedClass
                    };
                    
                    let context = SymbolContext {
                        file_path_index,
                        line_number: node.start_position().row + 1,
                        module: module.clone(),
                        fully_qualified_module: fully_qualified_module.clone(),
                        symbol_type: symbol_type.clone(),
                        parent_context: current_parents.to_vec(),
                    };
                    class_symbols.push(Symbol { name: name.clone(), context });

                    // Recursively collect symbols from the class body with this class as parent
                    let mut parent_stack = current_parents.to_vec();
                    parent_stack.push(ParentContext {
                        name,
                        symbol_type,
                        line_number: node.start_position().row + 1,
                    });
                    
                    if let Some(body) = node.child_by_field_name("body") {
                        let (nested_functions, nested_classes) = collect_symbols(
                            body,
                            source,
                            file_path,
                            base_dir,
                            &parent_stack,
                            path_registry,
                        );
                        function_symbols.extend(nested_functions);
                        class_symbols.extend(nested_classes);
                    }
                }
            }
            _ => {}
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }

    (function_symbols, class_symbols)
}

pub fn parse_python_file(
    parser: &mut Parser, 
    path: &Path, 
    base_dir: &Path,
    path_registry: &mut PathRegistry,
) -> Result<(Vec<Symbol>, Vec<Symbol>)> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    
    let tree = parser.parse(&source, None)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    
    let (functions, classes) = collect_symbols(
        tree.root_node(),
        &source,
        path,
        base_dir,
        &[],
        path_registry,
    );
    
    Ok((functions, classes))
}

pub fn save_symbols(path: &Path, stats: &SymbolStats) -> Result<()> {
    let path_registry = stats.path_registry.lock().unwrap();
    
    // Convert HashSets to Vecs
    let functions = stats.functions.lock().unwrap();
    let classes = stats.classes.lock().unwrap();
    
    let functions_vec: Vec<Symbol> = functions.iter().cloned().collect();
    let classes_vec: Vec<Symbol> = classes.iter().cloned().collect();
    
    let symbol_data = SymbolData::new(
        functions_vec,
        classes_vec,
        path_registry.paths.clone(),
    );
    
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let encoder = GzEncoder::new(writer, Compression::default());
    bincode::serialize_into(encoder, &symbol_data)?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_path_registry_register_and_get() {
        let mut registry = PathRegistry::new();
        
        // Register a path
        let path1 = PathBuf::from("/test/path/file.py");
        let index1 = registry.register_path(path1.clone());
        
        // Register another path
        let path2 = PathBuf::from("/test/another/file.py");
        let index2 = registry.register_path(path2.clone());
        
        // Verify paths can be retrieved correctly
        assert_eq!(registry.get_path(index1), &path1);
        assert_eq!(registry.get_path(index2), &path2);
        
        // Register same path again, should return same index
        let index3 = registry.register_path(path1.clone());
        assert_eq!(index1, index3);
    }

    #[test]
    fn test_symbol_context_creation() {
        let mut path_registry = PathRegistry::new();
        let file_path = PathBuf::from("/test/module/file.py");
        let file_path_index = path_registry.register_path(file_path);
        
        let context = SymbolContext {
            file_path_index,
            line_number: 42,
            module: "file".to_string(),
            fully_qualified_module: "module.file".to_string(),
            symbol_type: SymbolType::Function,
            parent_context: vec![],
        };
        
        assert_eq!(context.line_number, 42);
        assert_eq!(context.module, "file");
        assert_eq!(context.fully_qualified_module, "module.file");
        assert_eq!(context.symbol_type, SymbolType::Function);
        assert_eq!(*path_registry.get_path(context.file_path_index), PathBuf::from("/test/module/file.py"));
    }

    #[test]
    fn test_symbol_creation() {
        let mut path_registry = PathRegistry::new();
        let file_path = PathBuf::from("/test/module/file.py");
        let file_path_index = path_registry.register_path(file_path);
        
        let context = SymbolContext {
            file_path_index,
            line_number: 42,
            module: "file".to_string(),
            fully_qualified_module: "module.file".to_string(),
            symbol_type: SymbolType::Function,
            parent_context: vec![],
        };
        
        let symbol = Symbol {
            name: "test_function".to_string(),
            context,
        };
        
        assert_eq!(symbol.name, "test_function");
        assert_eq!(symbol.context.line_number, 42);
        assert_eq!(*path_registry.get_path(symbol.context.file_path_index), PathBuf::from("/test/module/file.py"));
    }

    #[test]
    fn test_get_module_name() {
        assert_eq!(get_module_name(Path::new("/path/to/module.py")), "module");
        assert_eq!(get_module_name(Path::new("relative/path/to/file.py")), "file");
        assert_eq!(get_module_name(Path::new("just_filename.py")), "just_filename");
    }

    #[test]
    fn test_get_fully_qualified_module() {
        let base_dir = Path::new("/base/dir");
        
        assert_eq!(
            get_fully_qualified_module(Path::new("/base/dir/module/file.py"), base_dir),
            "module"
        );
        
        assert_eq!(
            get_fully_qualified_module(Path::new("/base/dir/deeply/nested/module/file.py"), base_dir),
            "deeply.nested.module"
        );
        
        // Test with non-matching base dir
        assert_eq!(
            get_fully_qualified_module(Path::new("/different/path/file.py"), base_dir),
            "unknown"
        );
    }
} 