use anyhow::Result;
use arc_swap::{ArcSwap, Guard};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use crate::search::search_symbols;
use crate::symbols::{PathRegistry, Symbol, SymbolType};
use lsp_types::{Location, Position, Range, SymbolInformation, SymbolKind};
use url::Url;

/// Symbol data container for the symbol store
#[derive(Debug, Clone)]
pub struct SymbolData {
    pub functions: HashSet<Symbol>,
    pub classes: HashSet<Symbol>,
    pub path_registry: PathRegistry,
}

/// Immutable symbol database that can be atomically swapped
#[derive(Debug, Clone)]
pub struct SymbolDatabase {
    pub functions: Arc<HashSet<Symbol>>,
    pub classes: Arc<HashSet<Symbol>>,
    pub path_registry: Arc<PathRegistry>,
    pub version: u64,
}

impl SymbolDatabase {
    /// Create an empty database
    pub fn empty() -> Self {
        Self {
            functions: Arc::new(HashSet::new()),
            classes: Arc::new(HashSet::new()),
            path_registry: Arc::new(PathRegistry::new()),
            version: 0,
        }
    }

    /// Create a new database from symbol data
    pub fn from_symbol_data(data: SymbolData, version: u64) -> Self {
        Self {
            functions: Arc::new(data.functions),
            classes: Arc::new(data.classes),
            path_registry: Arc::new(data.path_registry),
            version,
        }
    }

    /// Get the total number of symbols
    pub fn symbol_count(&self) -> usize {
        self.functions.len() + self.classes.len()
    }
}

/// Update request types for the symbol store
#[derive(Debug)]
pub enum UpdateRequest {
    /// Complete replacement of all symbols (used for bootstrap)
    Batch(BatchUpdate),
    /// Incremental update with specific file changes
    Incremental(IncrementalUpdate),
    /// Shutdown the writer thread
    Shutdown,
}

/// Batch update request
#[derive(Debug)]
pub struct BatchUpdate {
    pub functions: HashSet<Symbol>,
    pub classes: HashSet<Symbol>,
    pub path_registry: PathRegistry,
    pub response_channel: oneshot::Sender<Result<u64>>,
}

/// Incremental update request
#[derive(Debug)]
pub struct IncrementalUpdate {
    pub added_files: Vec<PathBuf>,
    pub removed_files: Vec<PathBuf>,
    pub parsed_symbols: SymbolData,
    pub response_channel: oneshot::Sender<Result<u64>>,
}

/// Lock-free symbol store using atomic Arc swapping
pub struct SymbolStore {
    /// Current database version - can be read lock-free
    current: Arc<ArcSwap<SymbolDatabase>>,
    /// Channel for sending update requests to the writer thread
    update_channel: mpsc::UnboundedSender<UpdateRequest>,
    /// Receiver for the writer thread (stored until writer is started)
    update_receiver: Option<mpsc::UnboundedReceiver<UpdateRequest>>,
    /// Base directory for resolving relative paths
    base_directory: Arc<PathBuf>,
}

impl SymbolStore {
    /// Create a new symbol store without starting the writer thread
    pub fn new(base_directory: PathBuf) -> Self {
        let (update_sender, update_receiver) = mpsc::unbounded_channel();
        let current = Arc::new(ArcSwap::from_pointee(SymbolDatabase::empty()));

        Self {
            current,
            update_channel: update_sender,
            update_receiver: Some(update_receiver),
            base_directory: Arc::new(base_directory),
        }
    }

    /// Start the writer thread - must be called from within a tokio runtime
    /// Can only be called once, panics if called multiple times
    pub fn start_writer(&mut self) -> tokio::task::JoinHandle<()> {
        let update_receiver = self
            .update_receiver
            .take()
            .expect("Writer thread already started or receiver was consumed");

        let writer_current = self.current.clone();
        let writer_base_dir = self.base_directory.clone();
        tokio::spawn(async move {
            database_writer_task(update_receiver, writer_current, writer_base_dir).await;
        })
    }

    /// Create a new symbol store and start the writer thread in one call
    /// Must be called from within a tokio runtime context
    pub fn new_with_writer(base_directory: PathBuf) -> (Self, tokio::task::JoinHandle<()>) {
        let mut store = Self::new(base_directory);
        let handle = store.start_writer();
        (store, handle)
    }

    /// Get a lock-free snapshot of the current database
    pub fn read(&self) -> Guard<Arc<SymbolDatabase>> {
        self.current.load()
    }

    /// Check if the store is ready (has symbols loaded)
    pub fn is_ready(&self) -> bool {
        let db = self.read();
        db.version > 0 && db.symbol_count() > 0
    }

    /// Get current version number
    pub fn version(&self) -> u64 {
        self.read().version
    }

    /// Get current symbol count
    pub fn symbol_count(&self) -> usize {
        self.read().symbol_count()
    }

    /// Get the base directory
    pub fn base_directory(&self) -> &Arc<PathBuf> {
        &self.base_directory
    }

    /// Perform a symbol search without any locking
    pub fn search(&self, query: &str) -> Vec<(Symbol, i64)> {
        if query.is_empty() {
            return Vec::new();
        }

        let db = self.read();
        tracing::debug!(
            "Searching for '{}' in {} functions and {} classes (version {})",
            query,
            db.functions.len(),
            db.classes.len(),
            db.version
        );

        let (results, _metrics) =
            search_symbols(query, &db.functions, &db.classes, &db.path_registry, false);

        tracing::debug!("Search for '{}' returned {} results", query, results.len());

        results
    }

    /// Search and convert directly to LSP format
    pub fn search_to_lsp(&self, query: &str, max_results: usize) -> Vec<SymbolInformation> {
        if query.is_empty() {
            return Vec::new();
        }

        let db = self.read();
        let (results, _metrics) =
            search_symbols(query, &db.functions, &db.classes, &db.path_registry, false);

        results
            .into_iter()
            .filter_map(|(symbol, score)| {
                to_lsp_symbol_information(&symbol, &db.path_registry, score, &self.base_directory)
            })
            .take(max_results)
            .collect()
    }

    /// Perform a batch update (replace all symbols)
    pub async fn batch_update(&self, symbols: SymbolData) -> Result<u64> {
        let (tx, rx) = oneshot::channel();

        let request = UpdateRequest::Batch(BatchUpdate {
            functions: symbols.functions,
            classes: symbols.classes,
            path_registry: symbols.path_registry,
            response_channel: tx,
        });

        self.update_channel
            .send(request)
            .map_err(|_| anyhow::anyhow!("Failed to send batch update request"))?;

        rx.await
            .map_err(|_| anyhow::anyhow!("Failed to receive batch update response"))?
    }

    /// Perform an incremental update
    pub async fn incremental_update(
        &self,
        added_files: Vec<PathBuf>,
        removed_files: Vec<PathBuf>,
        parsed_symbols: SymbolData,
    ) -> Result<u64> {
        let (tx, rx) = oneshot::channel();

        let request = UpdateRequest::Incremental(IncrementalUpdate {
            added_files,
            removed_files,
            parsed_symbols,
            response_channel: tx,
        });

        self.update_channel
            .send(request)
            .map_err(|_| anyhow::anyhow!("Failed to send incremental update request"))?;

        rx.await
            .map_err(|_| anyhow::anyhow!("Failed to receive incremental update response"))?
    }

    /// Shutdown the writer thread
    pub async fn shutdown(&self) -> Result<()> {
        self.update_channel
            .send(UpdateRequest::Shutdown)
            .map_err(|_| anyhow::anyhow!("Failed to send shutdown request"))?;
        Ok(())
    }
}

impl Clone for SymbolStore {
    fn clone(&self) -> Self {
        // Can only clone if the writer thread has already been started
        // (receiver has been consumed)
        if self.update_receiver.is_some() {
            panic!("Cannot clone SymbolStore before writer thread is started. Call start_writer() first.");
        }

        Self {
            current: self.current.clone(),
            update_channel: self.update_channel.clone(),
            update_receiver: None,
            base_directory: self.base_directory.clone(),
        }
    }
}

/// Single writer thread that handles all database updates
async fn database_writer_task(
    mut update_receiver: mpsc::UnboundedReceiver<UpdateRequest>,
    current_db: Arc<ArcSwap<SymbolDatabase>>,
    base_directory: Arc<PathBuf>,
) {
    info!("Database writer thread started");

    while let Some(request) = update_receiver.recv().await {
        match request {
            UpdateRequest::Batch(batch_update) => {
                info!(
                    "Processing batch update with {} functions, {} classes",
                    batch_update.functions.len(),
                    batch_update.classes.len()
                );

                let new_version = current_db.load().version + 1;
                let new_db = SymbolDatabase {
                    functions: Arc::new(batch_update.functions),
                    classes: Arc::new(batch_update.classes),
                    path_registry: Arc::new(batch_update.path_registry),
                    version: new_version,
                };

                // Atomic swap - readers see new data instantly
                current_db.store(Arc::new(new_db));

                info!("Batch update completed, new version: {}", new_version);
                batch_update.response_channel.send(Ok(new_version)).ok();
            }

            UpdateRequest::Incremental(inc_update) => {
                info!(
                    "Processing incremental update: {} added files, {} removed files",
                    inc_update.added_files.len(),
                    inc_update.removed_files.len()
                );

                let result = apply_incremental_update(
                    &current_db,
                    inc_update.added_files,
                    inc_update.removed_files,
                    inc_update.parsed_symbols,
                    base_directory.clone(),
                );
                inc_update.response_channel.send(result).ok();
            }

            UpdateRequest::Shutdown => {
                info!("Database writer thread shutting down");
                break;
            }
        }
    }

    info!("Database writer thread exited");
}

/// Apply incremental changes to create a new database version
fn apply_incremental_update(
    current_db: &Arc<ArcSwap<SymbolDatabase>>,
    added_files: Vec<PathBuf>,
    removed_files: Vec<PathBuf>,
    parsed_symbols: SymbolData,
    base_directory: Arc<PathBuf>,
) -> Result<u64> {
    let current = current_db.load();

    // Clone current data (cheap due to Arc sharing)
    let mut new_functions = (*current.functions).clone();
    let mut new_classes = (*current.classes).clone();
    let mut new_registry = (*current.path_registry).clone();

    // Remove symbols for deleted files
    for removed_file in &removed_files {
        remove_symbols_for_file(
            &mut new_functions,
            &mut new_classes,
            &new_registry,
            removed_file,
            &base_directory,
        );
    }

    // Remove old symbols for modified files (those in added_files that were already indexed)
    for added_file in &added_files {
        remove_symbols_for_file(
            &mut new_functions,
            &mut new_classes,
            &new_registry,
            added_file,
            &base_directory,
        );
    }

    // Create mapping from temporary path registry to main registry
    let mut index_mapping = std::collections::HashMap::new();
    for (temp_index, path) in parsed_symbols.path_registry.paths.iter().enumerate() {
        let main_index = new_registry.register_path(path.clone());
        index_mapping.insert(temp_index, main_index);
    }

    // Add new symbols with corrected path indexes
    let mut functions_added = 0;
    let mut classes_added = 0;

    for mut function in parsed_symbols.functions {
        if let Some(&new_index) = index_mapping.get(&function.context.file_path_index) {
            function.context.file_path_index = new_index;
            new_functions.insert(function);
            functions_added += 1;
        } else {
            error!(
                "Failed to map file path index for function: {}",
                function.name
            );
        }
    }

    for mut class in parsed_symbols.classes {
        if let Some(&new_index) = index_mapping.get(&class.context.file_path_index) {
            class.context.file_path_index = new_index;
            new_classes.insert(class);
            classes_added += 1;
        } else {
            error!("Failed to map file path index for class: {}", class.name);
        }
    }

    let new_version = current.version + 1;
    let new_db = SymbolDatabase {
        functions: Arc::new(new_functions),
        classes: Arc::new(new_classes),
        path_registry: Arc::new(new_registry),
        version: new_version,
    };

    // Atomic swap
    current_db.store(Arc::new(new_db));

    info!(
        "Incremental update completed: added {} functions, {} classes. Version: {}",
        functions_added, classes_added, new_version
    );

    Ok(new_version)
}

/// Remove all symbols that belong to a specific file
fn remove_symbols_for_file(
    functions: &mut HashSet<Symbol>,
    classes: &mut HashSet<Symbol>,
    path_registry: &PathRegistry,
    file_path: &PathBuf,
    base_directory: &Arc<PathBuf>,
) {
    let initial_function_count = functions.len();
    let initial_class_count = classes.len();

    tracing::info!(
        "Removing symbols for file: {} (initial: {} functions, {} classes)",
        file_path.display(),
        initial_function_count,
        initial_class_count
    );

    // Debug: Show all paths in the registry for comparison
    tracing::debug!(
        "Path registry contains {} paths:",
        path_registry.paths.len()
    );
    for (i, path) in path_registry.paths.iter().enumerate().take(5) {
        tracing::debug!("  [{}] {}", i, path.display());
    }
    tracing::debug!("Target file path: {}", file_path.display());

    functions
        .retain(|symbol| !symbol_belongs_to_file(symbol, file_path, path_registry, base_directory));
    classes
        .retain(|symbol| !symbol_belongs_to_file(symbol, file_path, path_registry, base_directory));

    let removed_functions = initial_function_count - functions.len();
    let removed_classes = initial_class_count - classes.len();

    tracing::info!(
        "Removed {} functions and {} classes for file: {}",
        removed_functions,
        removed_classes,
        file_path.display()
    );
}

/// Check if a symbol belongs to a specific file
fn symbol_belongs_to_file(
    symbol: &Symbol,
    file_path: &PathBuf,
    path_registry: &PathRegistry,
    base_directory: &Arc<PathBuf>,
) -> bool {
    let symbol_path = path_registry.get_path(symbol.context.file_path_index);

    // Normalize both paths to absolute paths for comparison
    let normalize_path = |path: &PathBuf| -> Option<PathBuf> {
        if path.is_absolute() {
            Some(path.clone())
        } else {
            Some(base_directory.join(path))
        }
    };

    let normalized_symbol_path = normalize_path(symbol_path);
    let normalized_target_path = normalize_path(file_path);

    let belongs = match (normalized_symbol_path, normalized_target_path) {
        (Some(sym_path), Some(target_path)) => {
            // Try to canonicalize paths to handle symlinks and .. components
            let sym_canonical = sym_path.canonicalize().unwrap_or(sym_path);
            let target_canonical = target_path.canonicalize().unwrap_or(target_path);
            sym_canonical == target_canonical
        }
        _ => {
            // Fallback to direct comparison if normalization fails
            symbol_path == file_path
        }
    };

    // Add debugging for path comparison
    if !belongs {
        tracing::debug!(
            "Symbol '{}' path '{}' != target path '{}' (belongs: {})",
            symbol.name,
            symbol_path.display(),
            file_path.display(),
            belongs
        );
    } else {
        tracing::debug!(
            "Symbol '{}' belongs to file '{}' - will be removed",
            symbol.name,
            file_path.display()
        );
    }

    belongs
}

/// Convert a Symbol to an LSP SymbolInformation
pub fn to_lsp_symbol_information(
    symbol: &Symbol,
    path_registry: &PathRegistry,
    score: i64,
    base_directory: &Arc<PathBuf>,
) -> Option<SymbolInformation> {
    let file_path: &PathBuf = path_registry.get_path(symbol.context.file_path_index);

    // Add debugging for file path conversion
    tracing::debug!(
        "Converting symbol '{}' with file path: {}",
        symbol.name,
        file_path.display()
    );

    // Convert to absolute path if it's relative
    let absolute_path = if file_path.is_absolute() {
        file_path.clone()
    } else {
        base_directory.join(file_path)
    };

    let url = match Url::from_file_path(&absolute_path) {
        Ok(url) => {
            tracing::debug!("Successfully converted to URL: {}", url);
            url
        }
        Err(e) => {
            tracing::error!(
                "Failed to convert path to URL: {} (error: {:?})",
                absolute_path.display(),
                e
            );
            return None;
        }
    };

    let uri = match url.as_str().parse() {
        Ok(url) => url,
        Err(e) => {
            tracing::error!("Failed to convert URL to URI: {} (error: {})", url, e);
            return None;
        }
    };

    // Determine symbol kind based on the symbol type
    let symbol_kind = match symbol.context.symbol_type {
        SymbolType::Class | SymbolType::NestedClass => SymbolKind::CLASS,
        SymbolType::Function | SymbolType::Method => SymbolKind::FUNCTION,
        _ => SymbolKind::VARIABLE, // Default fallback
    };

    // Create the symbol location
    let location = Location {
        uri,
        range: Range {
            start: Position {
                line: (symbol.context.line_number as u32).saturating_sub(1),
                character: 0,
            },
            end: Position {
                line: (symbol.context.line_number as u32).saturating_sub(1),
                character: 0,
            },
        },
    };

    // Build the container name from the parent context or module
    let container_name = if !symbol.context.parent_context.is_empty() {
        symbol
            .context
            .parent_context
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
            .join(".")
    } else {
        symbol.context.fully_qualified_module.clone()
    };

    // Include score in the symbol details for debugging
    let name_with_score = if cfg!(debug_assertions) {
        format!("{} ({})", symbol.name, score)
    } else {
        symbol.name.clone()
    };

    tracing::debug!(
        "Successfully converted symbol '{}' to LSP format",
        symbol.name
    );

    #[allow(deprecated)]
    Some(SymbolInformation {
        name: name_with_score,
        kind: symbol_kind,
        tags: None,
        location,
        container_name: Some(container_name),
        deprecated: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{PathRegistry, Symbol, SymbolContext, SymbolType};
    use std::collections::HashSet;
    use std::time::Duration;

    fn create_test_symbol(
        name: &str,
        symbol_type: SymbolType,
        line: usize,
        file_index: usize,
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            context: SymbolContext {
                symbol_type,
                line_number: line,
                file_path_index: file_index,
                parent_context: vec![],
                fully_qualified_module: "test".to_string(),
                module: "test".to_string(),
            },
        }
    }

    fn create_test_symbol_data() -> SymbolData {
        let mut path_registry = PathRegistry::new();
        // Use an absolute path for LSP conversion to work
        let test_file_path = std::env::current_dir().unwrap().join("test.py");
        let file_index = path_registry.register_path(test_file_path);

        let mut functions = HashSet::new();
        functions.insert(create_test_symbol(
            "test_function",
            SymbolType::Function,
            1,
            file_index,
        ));
        functions.insert(create_test_symbol(
            "another_function",
            SymbolType::Function,
            10,
            file_index,
        ));

        let mut classes = HashSet::new();
        classes.insert(create_test_symbol(
            "TestClass",
            SymbolType::Class,
            5,
            file_index,
        ));

        SymbolData {
            functions,
            classes,
            path_registry,
        }
    }

    #[test]
    fn test_symbol_store_creation_without_runtime() {
        // This should work - creating store without starting writer
        let _store = SymbolStore::new(PathBuf::new());
    }

    #[test]
    #[should_panic(expected = "Writer thread already started")]
    fn test_start_writer_twice_panics() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut store = SymbolStore::new(PathBuf::new());
            let _handle1 = store.start_writer();
            let _handle2 = store.start_writer(); // Should panic
        });
    }

    #[test]
    #[should_panic(expected = "Cannot clone SymbolStore before writer thread is started")]
    fn test_clone_before_writer_started_panics() {
        let store = SymbolStore::new(PathBuf::new());
        let _cloned = store.clone(); // Should panic
    }

    #[tokio::test]
    async fn test_symbol_store_basic_functionality() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        // Test initial state
        assert!(!store.is_ready());
        assert_eq!(store.version(), 0);
        assert_eq!(store.symbol_count(), 0);

        // Test empty search
        let results = store.search("test");
        assert!(results.is_empty());

        // Test batch update
        let test_data = create_test_symbol_data();
        let version = store.batch_update(test_data).await.unwrap();
        assert_eq!(version, 1);

        // Test state after update
        assert!(store.is_ready());
        assert_eq!(store.version(), 1);
        assert_eq!(store.symbol_count(), 3); // 2 functions + 1 class

        // Test search functionality
        let results = store.search("test");
        assert!(!results.is_empty());

        let function_results = store.search("function");
        assert_eq!(function_results.len(), 2);

        let class_results = store.search("TestClass");
        assert_eq!(class_results.len(), 1);
    }

    #[tokio::test]
    async fn test_symbol_store_lsp_conversion() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        let test_data = create_test_symbol_data();
        store.batch_update(test_data).await.unwrap();

        // Test LSP conversion
        let lsp_results = store.search_to_lsp("test", 10);
        assert!(!lsp_results.is_empty());

        // Test max results limiting
        let limited_results = store.search_to_lsp("function", 1);
        assert_eq!(limited_results.len(), 1);
    }

    #[tokio::test]
    async fn test_symbol_store_incremental_updates() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        // Initial batch update
        let test_data = create_test_symbol_data();
        let version1 = store.batch_update(test_data).await.unwrap();
        assert_eq!(version1, 1);
        assert_eq!(store.symbol_count(), 3);

        // Incremental update - add new symbols
        let mut new_path_registry = PathRegistry::new();
        let new_file_path = std::env::current_dir().unwrap().join("new_file.py");
        let new_file_index = new_path_registry.register_path(new_file_path.clone());

        let mut new_functions = HashSet::new();
        new_functions.insert(create_test_symbol(
            "new_function",
            SymbolType::Function,
            1,
            new_file_index,
        ));

        let new_symbol_data = SymbolData {
            functions: new_functions,
            classes: HashSet::new(),
            path_registry: new_path_registry,
        };

        let version2 = store
            .incremental_update(vec![new_file_path], vec![], new_symbol_data)
            .await
            .unwrap();

        assert_eq!(version2, 2);
        assert_eq!(store.symbol_count(), 4); // 3 original + 1 new

        // Test that new symbol is searchable
        let results = store.search("new_function");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_symbol_store_concurrent_reads() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        // Load some test data
        let test_data = create_test_symbol_data();
        store.batch_update(test_data).await.unwrap();

        // Test concurrent reads - focused on correctness, not stress testing
        let mut handles = vec![];
        for i in 0..5 {
            let store_clone = store.clone();
            let handle = tokio::spawn(async move {
                let results = store_clone.search("test");
                assert!(!results.is_empty(), "Thread {} got empty results", i);

                // Verify specific expected results
                let function_results = store_clone.search("function");
                assert_eq!(function_results.len(), 2);

                results.len()
            });
            handles.push(handle);
        }

        // Wait for all searches to complete
        for handle in handles {
            let result_count = handle.await.unwrap();
            assert!(result_count > 0);
        }
    }

    #[tokio::test]
    async fn test_symbol_store_empty_queries() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        let test_data = create_test_symbol_data();
        store.batch_update(test_data).await.unwrap();

        // Test empty query
        let results = store.search("");
        assert!(results.is_empty());

        let lsp_results = store.search_to_lsp("", 10);
        assert!(lsp_results.is_empty());
    }

    #[tokio::test]
    async fn test_symbol_store_concurrent_reads_with_update() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        // Load initial data
        let test_data = create_test_symbol_data();
        store.batch_update(test_data).await.unwrap();

        // Start concurrent readers
        let mut read_handles = vec![];
        for i in 0..3 {
            let store_clone = store.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..5 {
                    let results = store_clone.search("test");
                    assert!(!results.is_empty(), "Reader {} got empty results", i);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
            read_handles.push(handle);
        }

        // Perform an update while reads are happening
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut new_path_registry = PathRegistry::new();
        let update_file_path = std::env::current_dir().unwrap().join("concurrent_test.py");
        let update_file_index = new_path_registry.register_path(update_file_path.clone());

        let mut update_functions = HashSet::new();
        update_functions.insert(create_test_symbol(
            "concurrent_function",
            SymbolType::Function,
            1,
            update_file_index,
        ));

        let update_data = SymbolData {
            functions: update_functions,
            classes: HashSet::new(),
            path_registry: new_path_registry,
        };

        let version = store
            .incremental_update(vec![update_file_path], vec![], update_data)
            .await
            .unwrap();

        assert_eq!(version, 2);

        // Wait for all readers to complete
        for handle in read_handles {
            handle.await.unwrap();
        }

        // Verify the update was applied
        let results = store.search("concurrent_function");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_symbol_store_version_tracking() {
        let (store, _writer_handle) = SymbolStore::new_with_writer(PathBuf::new());

        assert_eq!(store.version(), 0);

        let test_data = create_test_symbol_data();
        let version1 = store.batch_update(test_data).await.unwrap();
        assert_eq!(version1, 1);
        assert_eq!(store.version(), 1);

        // Another update should increment version
        let empty_data = SymbolData {
            functions: HashSet::new(),
            classes: HashSet::new(),
            path_registry: PathRegistry::new(),
        };
        let version2 = store.batch_update(empty_data).await.unwrap();
        assert_eq!(version2, 2);
        assert_eq!(store.version(), 2);
    }

    #[test]
    fn test_symbol_database_creation() {
        let db = SymbolDatabase::empty();
        assert_eq!(db.version, 0);
        assert_eq!(db.symbol_count(), 0);

        let test_data = create_test_symbol_data();
        let db2 = SymbolDatabase::from_symbol_data(test_data, 42);
        assert_eq!(db2.version, 42);
        assert_eq!(db2.symbol_count(), 3);
    }
}
