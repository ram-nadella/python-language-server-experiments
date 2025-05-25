use anyhow::Result;
use clap::Parser as ClapParser;
use std::collections::{HashMap, HashSet};
use std::io::stderr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use url::Url;

use lsp_server::{Connection, ErrorCode, Message, Response, ResponseError};
use lsp_types::{
    Location, OneOf, Position, Range, ServerCapabilities, SymbolInformation, SymbolKind,
    WorkspaceSymbolParams,
};
use serde_json::{self, Value};

// File watching imports
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;
use symbol_experiments::search::search_symbols;
use symbol_experiments::symbols::{PathRegistry, Symbol, SymbolStats, SymbolType};

/// Shared state for the LSP server that tracks symbol loading status
#[derive(Debug)]
struct ServerState {
    /// The loaded symbol data, None if still loading
    symbol_data: Option<SymbolData>,
    /// Whether the initial indexing is complete
    indexing_complete: bool,
}

/// Container for all symbol data
#[derive(Debug, Clone)]
struct SymbolData {
    functions: HashSet<Symbol>,
    classes: HashSet<Symbol>,
    path_registry: PathRegistry,
}

impl ServerState {
    fn new() -> Self {
        Self {
            symbol_data: None,
            indexing_complete: false,
        }
    }

    fn set_symbols(&mut self, data: SymbolData) {
        self.symbol_data = Some(data);
        self.indexing_complete = true;
    }

    fn is_ready(&self) -> bool {
        self.indexing_complete && self.symbol_data.is_some()
    }

    /// Apply incremental updates to the symbol data
    /// This method expects the new symbols to already be parsed
    fn apply_parsed_changes(&mut self, parsed_changes: ParsedFileChanges) -> Result<()> {
        let operation_start = Instant::now();

        if let Some(ref mut data) = self.symbol_data {
            info!(
                "📝 Starting symbol updates ({} functions, {} classes currently)",
                data.functions.len(),
                data.classes.len()
            );

            // Remove symbols for deleted files
            let delete_start = Instant::now();
            for deleted_path in &parsed_changes.deleted_files {
                Self::remove_symbols_for_file_static(
                    deleted_path,
                    &mut data.functions,
                    &mut data.classes,
                    &data.path_registry,
                );
            }
            let delete_time = delete_start.elapsed();

            // Remove old symbols for modified/created files (use a separate list to avoid borrow issues)
            let remove_start = Instant::now();
            let files_to_remove: Vec<PathBuf> =
                parsed_changes.new_symbols.path_registry.paths.clone();
            for path in &files_to_remove {
                Self::remove_symbols_for_file_static(
                    path,
                    &mut data.functions,
                    &mut data.classes,
                    &data.path_registry,
                );
            }
            let remove_time = remove_start.elapsed();

            // Create a mapping from temporary path registry indexes to main path registry indexes
            let mapping_start = Instant::now();
            let mut index_mapping: HashMap<usize, usize> = HashMap::new();
            for (temp_index, path) in parsed_changes
                .new_symbols
                .path_registry
                .paths
                .iter()
                .enumerate()
            {
                let main_index = data.path_registry.register_path(path.clone());
                index_mapping.insert(temp_index, main_index);
            }
            let mapping_time = mapping_start.elapsed();

            // Add new symbols with corrected path indexes
            let insert_start = Instant::now();
            let mut functions_added = 0;
            let mut classes_added = 0;

            for mut function in parsed_changes.new_symbols.functions {
                // Fix the file_path_index to use the main path registry
                if let Some(&new_index) = index_mapping.get(&function.context.file_path_index) {
                    function.context.file_path_index = new_index;
                    data.functions.insert(function);
                    functions_added += 1;
                } else {
                    error!(
                        "Failed to map file path index for function: {}",
                        function.name
                    );
                }
            }
            for mut class in parsed_changes.new_symbols.classes {
                // Fix the file_path_index to use the main path registry
                if let Some(&new_index) = index_mapping.get(&class.context.file_path_index) {
                    class.context.file_path_index = new_index;
                    data.classes.insert(class);
                    classes_added += 1;
                } else {
                    error!("Failed to map file path index for class: {}", class.name);
                }
            }
            let insert_time = insert_start.elapsed();

            let total_time = operation_start.elapsed();
            info!("📝 Symbol update breakdown: delete={}ms, remove={}ms, mapping={}ms, insert={}ms, total={}ms", 
                delete_time.as_millis(), remove_time.as_millis(), mapping_time.as_millis(),
                insert_time.as_millis(), total_time.as_millis());
            info!(
                "📝 Added {} functions, {} classes. Total now: {} functions, {} classes",
                functions_added,
                classes_added,
                data.functions.len(),
                data.classes.len()
            );
        }
        Ok(())
    }

    /// Remove all symbols that belong to a specific file
    fn remove_symbols_for_file_static(
        file_path: &PathBuf,
        functions: &mut HashSet<Symbol>,
        classes: &mut HashSet<Symbol>,
        path_registry: &PathRegistry,
    ) {
        // We need to find symbols that belong to this file
        // This is a bit tricky because we need to compare file paths
        functions.retain(|symbol| {
            !Self::symbol_belongs_to_file_static(symbol, file_path, path_registry)
        });
        classes.retain(|symbol| {
            !Self::symbol_belongs_to_file_static(symbol, file_path, path_registry)
        });
    }

    /// Check if a symbol belongs to a specific file
    fn symbol_belongs_to_file_static(
        symbol: &Symbol,
        file_path: &PathBuf,
        path_registry: &PathRegistry,
    ) -> bool {
        let symbol_path = path_registry.get_path(symbol.context.file_path_index);
        symbol_path == file_path
    }
}

/// File change event types for our file watcher
#[derive(Debug, Clone)]
enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

/// Represents a file change event with path and change type
#[derive(Debug, Clone)]
struct FileChange {
    path: PathBuf,
    change_type: FileChangeType,
}

/// Represents the results of parsing file changes
#[derive(Debug)]
struct ParsedFileChanges {
    deleted_files: Vec<PathBuf>,
    new_symbols: SymbolData,
}

#[derive(ClapParser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Listen on this TCP port instead of using stdio
    #[arg(long)]
    port: Option<u16>,
}

/// Process file changes in parallel (much faster than processing one by one)
async fn process_file_changes_parallel(
    changes: &[FileChange],
    base_dir: &PathBuf,
) -> Result<ParsedFileChanges> {
    let mut deleted_files = Vec::new();
    let mut files_to_parse = Vec::new();

    // Separate deleted files from files that need parsing
    for change in changes {
        match change.change_type {
            FileChangeType::Deleted => {
                deleted_files.push(change.path.clone());
            }
            FileChangeType::Created | FileChangeType::Modified => {
                if change.path.extension().and_then(|s| s.to_str()) == Some("py") {
                    files_to_parse.push(change.path.clone());
                }
            }
        }
    }

    info!(
        "Processing {} deleted files and {} files to parse",
        deleted_files.len(),
        files_to_parse.len()
    );

    // Parse all modified/created files in parallel (this is the key performance improvement)
    let new_symbols = if !files_to_parse.is_empty() {
        let stats = SymbolStats::new();

        // Use the existing parallel parsing function - this is much faster than processing files one by one
        parse_python_files_parallel(&files_to_parse, base_dir, &stats)?;

        let functions = stats.functions.lock().unwrap().clone();
        let classes = stats.classes.lock().unwrap().clone();
        let path_registry = stats.path_registry.lock().unwrap().clone();

        SymbolData {
            functions,
            classes,
            path_registry,
        }
    } else {
        // No files to parse, create empty symbol data
        SymbolData {
            functions: HashSet::new(),
            classes: HashSet::new(),
            path_registry: PathRegistry::new(),
        }
    };

    Ok(ParsedFileChanges {
        deleted_files,
        new_symbols,
    })
}

/// Convert notify events to our FileChange events
fn notify_event_to_file_change(event: Event) -> Vec<FileChange> {
    let mut changes = Vec::new();

    match event.kind {
        EventKind::Create(_) => {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("py") {
                    changes.push(FileChange {
                        path,
                        change_type: FileChangeType::Created,
                    });
                }
            }
        }
        EventKind::Modify(_) => {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("py") {
                    changes.push(FileChange {
                        path,
                        change_type: FileChangeType::Modified,
                    });
                }
            }
        }
        EventKind::Remove(_) => {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("py") {
                    changes.push(FileChange {
                        path,
                        change_type: FileChangeType::Deleted,
                    });
                }
            }
        }
        _ => {
            // Ignore other event types like access events
        }
    }

    changes
}

/// File watcher task that monitors for changes and sends them to a channel
async fn file_watcher_task(
    directory: PathBuf,
    follow_links: bool,
    change_sender: mpsc::UnboundedSender<FileChange>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Create the file watcher
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| match res {
            Ok(event) => {
                let changes = notify_event_to_file_change(event);
                for change in changes {
                    if let Err(e) = tx.send(change) {
                        error!("Failed to send file change event: {}", e);
                    }
                }
            }
            Err(e) => error!("File watcher error: {}", e),
        },
        Config::default(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create file watcher: {}", e))?;

    // Start watching the directory
    let mode = if follow_links {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::Recursive // We still want recursive, but notify will handle symlinks based on the config
    };

    watcher
        .watch(&directory, mode)
        .map_err(|e| anyhow::anyhow!("Failed to start watching directory: {}", e))?;

    info!(
        "File watcher started for directory: {}",
        directory.display()
    );

    // Forward events from the watcher to our channel
    // Keep the watcher alive by storing it in a variable
    let _watcher_handle = watcher;
    while let Some(change) = rx.recv().await {
        info!(
            "File watcher received change: {:?} - {:?}",
            change.change_type, change.path
        );
        if let Err(e) = change_sender.send(change) {
            error!("Failed to send change to processor: {}", e);
            break;
        }
    }

    info!("File watcher task exiting");
    Ok(())
}

/// Debounced change processor that accumulates changes and processes them in batches
async fn debounced_change_processor(
    mut change_receiver: mpsc::UnboundedReceiver<FileChange>,
    server_state: Arc<RwLock<ServerState>>,
    base_dir: PathBuf,
    debounce_duration: Duration,
) -> Result<()> {
    let mut pending_changes: HashMap<PathBuf, FileChange> = HashMap::new();
    let mut last_change_time = None;

    loop {
        // Wait for changes or timeout
        let change_result = if pending_changes.is_empty() {
            // No pending changes, wait indefinitely for the first change
            change_receiver.recv().await
        } else {
            // We have pending changes, wait with timeout
            let remaining_time = last_change_time
                .map(|t: Instant| {
                    let elapsed = t.elapsed();
                    if elapsed >= debounce_duration {
                        Duration::from_millis(0) // Process immediately
                    } else {
                        debounce_duration - elapsed
                    }
                })
                .unwrap_or(Duration::from_millis(0));

            if remaining_time.is_zero() {
                None // Process pending changes
            } else {
                match timeout(remaining_time, change_receiver.recv()).await {
                    Ok(change) => change,
                    Err(_) => None, // Timeout occurred
                }
            }
        };

        match change_result {
            Some(change) => {
                // New change received
                info!(
                    "File change detected: {:?} - {:?}",
                    change.change_type, change.path
                );

                // Update pending changes (latest change for each path wins)
                pending_changes.insert(change.path.clone(), change);
                last_change_time = Some(Instant::now());
            }
            None => {
                // Timeout or channel closed - process pending changes
                if !pending_changes.is_empty() {
                    let changes: Vec<FileChange> = pending_changes.values().cloned().collect();
                    pending_changes.clear();
                    last_change_time = None;

                    info!("Processing {} debounced file changes", changes.len());
                    let process_start = Instant::now();

                    // Parse the files in parallel OUTSIDE the lock (this is the key improvement)
                    let parse_start = Instant::now();
                    match process_file_changes_parallel(&changes, &base_dir).await {
                        Ok(parsed_changes) => {
                            let parse_time = parse_start.elapsed();
                            info!(
                                "Parsed {} files in {}ms",
                                changes.len(),
                                parse_time.as_millis()
                            );

                            // Now quickly apply the parsed changes with minimal lock time
                            let apply_start = Instant::now();
                            info!("📝 Attempting to acquire write lock for applying changes");
                            let write_lock_start = Instant::now();

                            match server_state.write() {
                                Ok(mut state) => {
                                    let write_lock_time = write_lock_start.elapsed();
                                    info!(
                                        "📝 Acquired write lock in {}ms",
                                        write_lock_time.as_millis()
                                    );

                                    // Use panic protection to ensure lock is always released
                                    let apply_result =
                                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                            || state.apply_parsed_changes(parsed_changes),
                                        ));

                                    match apply_result {
                                        Ok(Ok(())) => {
                                            let apply_time = apply_start.elapsed();
                                            info!("✅ Applied {} file changes in {}ms (parse: {}ms, write_lock_wait: {}ms, apply: {}ms)", 
                                                changes.len(),
                                                process_start.elapsed().as_millis(),
                                                parse_time.as_millis(),
                                                write_lock_time.as_millis(),
                                                apply_time.as_millis()
                                            );
                                        }
                                        Ok(Err(e)) => {
                                            error!("❌ Failed to apply parsed changes: {}", e);
                                        }
                                        Err(panic_info) => {
                                            error!(
                                                "❌ PANIC during apply_parsed_changes: {:?}",
                                                panic_info
                                            );
                                            error!("❌ This would have poisoned the RwLock!");
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        "❌ Failed to acquire write lock for applying changes: {}",
                                        e
                                    );
                                    // Check if the lock is poisoned
                                    if server_state.is_poisoned() {
                                        error!("❌ RwLock is POISONED! A previous thread panicked while holding the lock.");
                                        error!("❌ This explains why read locks are hanging!");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to parse file changes: {}", e);
                        }
                    }
                } else {
                    // Channel closed and no pending changes
                    break;
                }
            }
        }
    }

    info!("Debounced change processor exiting");
    Ok(())
}

/// Convert a Symbol to an LSP SymbolInformation
fn to_lsp_symbol_information(
    symbol: &Symbol,
    path_registry: &PathRegistry,
    score: i64,
) -> Option<SymbolInformation> {
    // Return Option
    let file_path: &PathBuf = path_registry.get_path(symbol.context.file_path_index);
    let url = Url::from_file_path(file_path).ok()?; // Convert PathBuf to Url (Uri)
    let uri = match url.as_str().parse() {
        Ok(url) => url,
        Err(_) => {
            tracing::error!("Failed to convert path to URI: {}", file_path.display());
            return None; // Skip this symbol if conversion fails
        }
    };

    // Determine symbol kind based on the symbol type
    let symbol_kind = match symbol.context.symbol_type {
        SymbolType::Class | SymbolType::NestedClass => SymbolKind::CLASS,
        SymbolType::Function | SymbolType::Method => SymbolKind::FUNCTION,
        _ => SymbolKind::VARIABLE, // Default fallback
    };

    // Create the symbol location - we only have line number, so both start and end positions use the same line
    let location = Location {
        uri,
        range: Range {
            start: Position {
                line: (symbol.context.line_number as u32).saturating_sub(1), // Convert to 0-based indexing
                character: 0,
            },
            end: Position {
                line: (symbol.context.line_number as u32).saturating_sub(1),
                character: 0, // Keep character 0 for simplicity
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

    // Replace deprecated field with tags, but keep deprecated field as None
    #[allow(deprecated)]
    Some(SymbolInformation {
        name: name_with_score,
        kind: symbol_kind,
        tags: None, // Use tags instead of deprecated field
        location,
        container_name: Some(container_name),
        deprecated: None, // Field is deprecated but still required
    })
}

/// Handle a workspace symbol request from the LSP client asynchronously
async fn handle_workspace_symbol_request_async(
    params: WorkspaceSymbolParams,
    server_state: Arc<RwLock<ServerState>>,
) -> Vec<SymbolInformation> {
    info!(
        "Handling workspace symbol request asynchronously: query='{}'",
        params.query
    );

    // If the query is empty, return an empty result
    if params.query.is_empty() {
        return Vec::new();
    }

    // Perform the search directly with a read lock to avoid expensive cloning
    info!("🔍 Attempting to acquire read lock for symbol search");
    let lock_start = Instant::now();

    let search_result = match server_state.read() {
        Ok(state) => {
            let lock_time = lock_start.elapsed();
            info!("🔍 Acquired read lock in {}ms", lock_time.as_millis());

            // Warn if lock acquisition was slow
            if lock_time.as_millis() > 100 {
                error!(
                    "⚠️ Read lock acquisition was slow ({}ms) - possible write lock contention!",
                    lock_time.as_millis()
                );
            }

            if !state.is_ready() {
                info!("🔍 Symbols not ready yet, returning empty result (indexing_complete: {}, symbol_data: {})", 
                    state.indexing_complete, state.symbol_data.is_some());
                return Vec::new();
            }

            match state.symbol_data.as_ref() {
                Some(data) => {
                    info!(
                        "🔍 Starting search with symbol data ({} functions, {} classes)",
                        data.functions.len(),
                        data.classes.len()
                    );

                    // Perform the search while holding the lock (this is fast)
                    let search_start = Instant::now();
                    let (results, metrics) = search_symbols(
                        &params.query,
                        &data.functions,
                        &data.classes,
                        &data.path_registry,
                        false,
                    );
                    let search_time = search_start.elapsed();

                    let result_count = results.len();
                    info!(
                        "🔍 Search completed: found {} results in {}ms",
                        result_count,
                        search_time.as_millis()
                    );
                    info!(
                        "🔍 Search metrics: matcher_init={}ms, search={}ms, sort={}ms, total={}ms",
                        metrics.matcher_init_time_ms,
                        metrics.search_time_ms,
                        metrics.sort_time_ms,
                        metrics.total_time_ms
                    );

                    // Clone only the path registry (much smaller) and the minimal results
                    let path_registry = data.path_registry.clone();
                    Some((results, path_registry))
                }
                None => {
                    info!("🔍 Symbol data is None despite is_ready() returning true");
                    None
                }
            }
        }
        Err(e) => {
            error!("❌ Failed to acquire read lock for symbol search: {}", e);

            // Check for lock poisoning
            if server_state.is_poisoned() {
                error!("❌ RwLock is POISONED! This explains the read lock failure.");
                error!("❌ A previous thread must have panicked while holding a write lock.");
            }

            None
        }
    };

    // Process results outside the lock
    let (results, path_registry) = match search_result {
        Some(data) => data,
        None => return Vec::new(),
    };

    // truncate results to 100 symbols
    let max_results = 100;
    let result_count = results.len();
    if result_count > max_results {
        info!("🔍 Truncating results to {} symbols", max_results);
    }

    // Convert the results to LSP format, filtering out None values from conversion errors
    let convert_start = Instant::now();
    let lsp_symbols: Vec<SymbolInformation> = results
        .iter()
        .filter_map(|(symbol, score)| to_lsp_symbol_information(symbol, &path_registry, *score)) // Use filter_map
        .take(max_results)
        .collect();

    let convert_time = convert_start.elapsed();
    info!(
        "🔍 Converted {} symbols to LSP format in {}ms",
        lsp_symbols.len(),
        convert_time.as_millis()
    );
    lsp_symbols
}

/// Main LSP server loop
fn run_server(server_state: Arc<RwLock<ServerState>>, port: Option<u16>) -> Result<()> {
    info!("Starting LSP server");

    // Create a tokio runtime for handling async tasks
    let rt = Runtime::new()?;

    // Create the LSP connection based on whether a port is specified
    let (connection, io_threads) = if let Some(port) = port {
        info!("Starting LSP server on port {}", port);
        let addr = format!("127.0.0.1:{}", port);
        Connection::listen(addr)?
    } else {
        info!("Starting LSP server on stdio");
        Connection::stdio()
    };

    info!("LSP connection established");

    // Handle the initialize request from the client
    let server_capabilities = serde_json::to_value(ServerCapabilities {
        workspace_symbol_provider: Some(OneOf::Left(true)), // Indicate we support workspace symbol requests
        // We're not handling other capabilities
        ..ServerCapabilities::default()
    })?;

    // Process initialize request
    let _initialize_result = connection.initialize(server_capabilities)?;
    info!("LSP server initialized successfully");

    // Main message loop
    info!("Entering main message loop");

    // Clone connection.sender for use in async tasks
    let sender = connection.sender.clone();

    let mut message_count = 0;
    for msg in &connection.receiver {
        message_count += 1;
        info!("📨 LSP: Received message #{} from client", message_count);

        match msg {
            Message::Request(req) => {
                info!(
                    "📨 LSP: Message #{} is a REQUEST: method='{}', id={:?}",
                    message_count, req.method, req.id
                );

                if connection.handle_shutdown(&req)? {
                    info!("📨 LSP: Shutdown request received, exiting...");
                    return Ok(());
                }

                // Handle different LSP requests
                match req.method.as_str() {
                    // Workspace symbol request - this is the main functionality we're providing
                    "workspace/symbol" => {
                        info!(
                            "🔍 LSP: Received workspace/symbol request with id: {:?}",
                            req.id
                        );
                        let request_start = Instant::now();

                        // Clone the necessary data for the async task
                        let server_state_clone = server_state.clone();
                        let sender_clone = sender.clone();
                        let req_id = req.id.clone();

                        match serde_json::from_value::<WorkspaceSymbolParams>(req.params) {
                            Ok(params) => {
                                info!(
                                    "🔍 LSP: Processing workspace/symbol request id={:?} query='{}'",
                                    req_id, params.query
                                );

                                // Spawn an async task to handle the request
                                rt.spawn(async move {
                                    let task_start = Instant::now();
                                    info!("🔍 LSP: Starting async task for request id={:?}", req_id);

                                    // Add timeout protection (30 seconds should be plenty)
                                    let symbols = match timeout(
                                        Duration::from_secs(30),
                                        handle_workspace_symbol_request_async(params, server_state_clone)
                                    ).await {
                                        Ok(symbols) => symbols,
                                        Err(_) => {
                                            error!("❌ LSP: Request id={:?} timed out after 30 seconds", req_id);
                                            let resp = Response {
                                                id: req_id,
                                                result: None,
                                                error: Some(ResponseError {
                                                    code: ErrorCode::RequestFailed as i32,
                                                    message: "Request timed out".to_string(),
                                                    data: None,
                                                }),
                                            };
                                            if let Err(e) = sender_clone.send(Message::Response(resp)) {
                                                error!("❌ LSP: Failed to send timeout response: {}", e);
                                            }
                                            return;
                                        }
                                    };

                                    let symbol_count = symbols.len();
                                    let task_duration = task_start.elapsed();
                                    info!("🔍 LSP: Async search completed for id={:?} with {} results in {}ms", 
                                        req_id, symbol_count, task_duration.as_millis());

                                    // Create and send the response
                                    let serialize_start = Instant::now();
                                    match serde_json::to_value(symbols) {
                                        Ok(symbols_value) => {
                                            let serialize_time = serialize_start.elapsed();
                                            info!("🔍 LSP: Serialized {} symbols in {}ms for id={:?}", 
                                                symbol_count, serialize_time.as_millis(), req_id);

                                            let resp = Response {
                                                id: req_id.clone(),
                                                result: Some(symbols_value),
                                                error: None,
                                            };

                                            let send_start = Instant::now();
                                            match sender_clone.send(Message::Response(resp)) {
                                                Ok(_) => {
                                                    let send_time = send_start.elapsed();
                                                    let total_time = request_start.elapsed();
                                                    info!("✅ LSP: Successfully sent response for id={:?} with {} symbols (send: {}ms, total: {}ms)", 
                                                        req_id, symbol_count, send_time.as_millis(), total_time.as_millis());
                                                }
                                                Err(e) => {
                                                    error!("❌ LSP: Failed to send response for id={:?}: {}", req_id, e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("❌ LSP: Failed to serialize symbols for id={:?}: {}", req_id, e);
                                            let resp = Response {
                                                id: req_id.clone(),
                                                result: None,
                                                error: Some(ResponseError {
                                                    code: ErrorCode::InternalError as i32,
                                                    message: format!("Serialization error: {}", e),
                                                    data: None,
                                                }),
                                            };
                                            if let Err(e2) = sender_clone.send(Message::Response(resp)) {
                                                error!("❌ LSP: Failed to send error response for id={:?}: {}", req_id, e2);
                                            }
                                        }
                                    }
                                });

                                info!("🔍 LSP: Spawned async task for workspace/symbol request id={:?}", req.id);
                            }
                            Err(e) => {
                                tracing::error!("Failed to parse workspace/symbol params: {}", e);
                                let resp = Response {
                                    id: req.id,
                                    result: None,
                                    error: Some(ResponseError {
                                        code: ErrorCode::InvalidParams as i32,
                                        message: format!("Invalid params: {}", e),
                                        data: None,
                                    }),
                                };
                                connection.sender.send(Message::Response(resp))?;
                            }
                        }
                    }

                    // For any other requests we don't handle, respond with null
                    _ => {
                        info!("📨 LSP: Received unsupported request: {}", req.method);
                        let resp = Response {
                            id: req.id.clone(),
                            result: Some(Value::Null),
                            error: None,
                        };
                        info!(
                            "📤 LSP: Sending null response for unsupported request id={:?}",
                            req.id
                        );
                        connection.sender.send(Message::Response(resp))?;
                        info!("📤 LSP: Null response sent successfully");
                    }
                }
            }
            Message::Response(resp) => {
                info!(
                    "📨 LSP: Message #{} is a RESPONSE: id={:?}",
                    message_count, resp.id
                );
            }
            Message::Notification(not) => {
                info!(
                    "📨 LSP: Message #{} is a NOTIFICATION: method='{}'",
                    message_count, not.method
                );

                // Log important notifications
                match not.method.as_str() {
                    "$/cancelRequest" => {
                        info!("📨 LSP: Client cancelled a request: {:?}", not.params);
                    }
                    "textDocument/didOpen"
                    | "textDocument/didChange"
                    | "textDocument/didSave"
                    | "textDocument/didClose" => {
                        info!("📨 LSP: Document change notification: {}", not.method);
                    }
                    _ => {
                        info!("📨 LSP: Other notification: {}", not.method);
                    }
                }
            }
        }
    }

    // Wait for the io threads to finish
    io_threads.join()?;
    info!("LSP server shutting down");

    Ok(())
}

/// Background task to bootstrap symbol loading
async fn bootstrap_symbols(args: Args, server_state: Arc<RwLock<ServerState>>) -> Result<()> {
    let start = Instant::now();

    info!("Starting symbol bootstrap process");
    info!("Scanning directory: {}", args.directory.display());

    // Find all Python files
    let python_files: Vec<PathBuf> =
        list_python_files(&args.directory, args.follow_links).collect();
    info!("Found {} Python files", python_files.len());

    // Parse Python files and collect symbols
    let stats = SymbolStats::new();
    parse_python_files_parallel(&python_files, &args.directory, &stats)?;

    let functions = stats.functions.lock().unwrap().clone();
    let classes = stats.classes.lock().unwrap().clone();
    let path_registry = stats.path_registry.lock().unwrap().clone();

    info!(
        "Symbol loading complete in {}ms",
        start.elapsed().as_millis()
    );
    info!(
        "Found {} functions and {} classes",
        functions.len(),
        classes.len()
    );

    // Update the server state with loaded symbols
    {
        let mut state = server_state.write().unwrap();
        state.set_symbols(SymbolData {
            functions,
            classes,
            path_registry,
        });
    }

    info!("Symbol bootstrap process completed");
    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing to write to stderr
    // Default to INFO level if RUST_LOG environment variable is not set.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_writer(stderr) // Write logs to stderr
        .with_env_filter(env_filter) // Use the determined filter
        .with_ansi(false) // Disable ANSI escape sequences for cleaner output in VS Code
        .init();

    let args = Args::parse();

    info!("🚀 Starting LSP server with args: {:?}", args);

    // Create shared server state
    let server_state = Arc::new(RwLock::new(ServerState::new()));

    // Create a tokio runtime for the bootstrap process with monitoring
    info!("🔧 Creating tokio runtime");
    let rt = Runtime::new()?;
    info!("✅ Tokio runtime created successfully");

    // Start the bootstrap process in the background
    let bootstrap_state = server_state.clone();
    let bootstrap_args = args.clone();
    rt.spawn(async move {
        if let Err(e) = bootstrap_symbols(bootstrap_args, bootstrap_state).await {
            tracing::error!("Bootstrap process failed: {}", e);
        }
    });

    // Start file watching (always enabled)
    let (change_sender, change_receiver) = mpsc::unbounded_channel();
    let debounce_duration = Duration::from_millis(500); // Fixed 500ms debounce

    // Start the file watcher task with restart logic
    let watcher_directory = args.directory.clone();
    let watcher_follow_links = args.follow_links;
    let watcher_change_sender = change_sender.clone();
    rt.spawn(async move {
        loop {
            match file_watcher_task(
                watcher_directory.clone(),
                watcher_follow_links,
                watcher_change_sender.clone(),
            )
            .await
            {
                Ok(_) => {
                    info!("File watcher task completed normally");
                    break;
                }
                Err(e) => {
                    error!(
                        "File watcher task failed: {}, restarting in 5 seconds...",
                        e
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Start the debounced change processor (no restart since it shouldn't fail)
    let processor_state = server_state.clone();
    let processor_base_dir = args.directory.clone();
    rt.spawn(async move {
        if let Err(e) = debounced_change_processor(
            change_receiver,
            processor_state,
            processor_base_dir,
            debounce_duration,
        )
        .await
        {
            error!("Debounced change processor failed: {}", e);
        }
    });

    info!("File watching enabled with 500ms debounce");

    // Start the LSP server immediately (non-blocking)
    run_server(server_state, args.port)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range, SymbolKind, Uri};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use symbol_experiments::symbols::{ParentContext, SymbolContext, SymbolType};

    // Helper function to create a simple PathRegistry for tests
    fn create_test_path_registry() -> PathRegistry {
        let mut registry = PathRegistry::new();
        registry.register_path(PathBuf::from("/test/path/file1.py"));
        registry.register_path(PathBuf::from("/test/path/file2.py"));
        registry
    }

    // Helper function to create a sample symbol
    fn create_test_symbol(
        name: &str,
        kind: SymbolType,
        line: usize,
        file_index: usize,
        parent: Option<&str>,
        module: &str,
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            context: SymbolContext {
                symbol_type: kind,
                line_number: line,
                file_path_index: file_index,
                parent_context: parent
                    .map(|p| {
                        vec![ParentContext {
                            name: p.to_string(),
                            line_number: 0,
                            symbol_type: SymbolType::Function,
                        }]
                    })
                    .unwrap_or_default(),
                fully_qualified_module: module.to_string(),
                module: module.to_string(),
            },
        }
    }

    #[test]
    fn test_to_lsp_symbol_information_conversion() -> Result<(), Box<dyn std::error::Error>> {
        let registry = create_test_path_registry();
        let symbol = create_test_symbol("my_function", SymbolType::Function, 10, 0, None, "file1");
        let score = 100;

        let lsp_info_opt = to_lsp_symbol_information(&symbol, &registry, score);

        assert!(lsp_info_opt.is_some());
        let lsp_info = lsp_info_opt.unwrap();

        let expected_name = if cfg!(debug_assertions) {
            "my_function (100)".to_string()
        } else {
            "my_function".to_string()
        };
        assert_eq!(lsp_info.name, expected_name);
        assert_eq!(lsp_info.kind, SymbolKind::FUNCTION);
        let expected_uri: Uri = Url::from_file_path(registry.get_path(0))
            .unwrap()
            .as_str()
            .parse()?;
        assert_eq!(lsp_info.location.uri, expected_uri);
        assert_eq!(
            lsp_info.location.range,
            Range {
                start: Position {
                    line: 9,
                    character: 0
                },
                end: Position {
                    line: 9,
                    character: 0
                },
            }
        );
        assert_eq!(lsp_info.container_name, Some("file1".to_string()));
        assert!(lsp_info.tags.is_none());
        Ok(())
    }

    #[test]
    fn test_to_lsp_symbol_information_class_conversion() -> Result<(), Box<dyn std::error::Error>> {
        let registry = create_test_path_registry();
        let symbol = create_test_symbol("MyClass", SymbolType::Class, 25, 1, None, "file2");
        let score = 50;

        let lsp_info_opt = to_lsp_symbol_information(&symbol, &registry, score);

        assert!(lsp_info_opt.is_some());
        let lsp_info = lsp_info_opt.unwrap();

        let expected_name = if cfg!(debug_assertions) {
            "MyClass (50)".to_string()
        } else {
            "MyClass".to_string()
        };
        assert_eq!(lsp_info.name, expected_name);
        assert_eq!(lsp_info.kind, SymbolKind::CLASS);
        let expected_uri: Uri = Url::from_file_path(registry.get_path(1))
            .unwrap()
            .as_str()
            .parse()?;
        assert_eq!(lsp_info.location.uri, expected_uri);
        assert_eq!(lsp_info.location.range.start.line, 24);
        assert_eq!(lsp_info.container_name, Some("file2".to_string()));
        Ok(())
    }

    #[test]
    fn test_to_lsp_symbol_information_method_conversion() -> Result<(), Box<dyn std::error::Error>>
    {
        let registry = create_test_path_registry();
        let symbol = create_test_symbol(
            "my_method",
            SymbolType::Method,
            30,
            1,
            Some("MyClass"),
            "file2",
        );
        let score = 75;

        let lsp_info_opt = to_lsp_symbol_information(&symbol, &registry, score);

        assert!(lsp_info_opt.is_some());
        let lsp_info = lsp_info_opt.unwrap();

        let expected_name = if cfg!(debug_assertions) {
            "my_method (75)".to_string()
        } else {
            "my_method".to_string()
        };
        assert_eq!(lsp_info.name, expected_name);
        assert_eq!(lsp_info.kind, SymbolKind::FUNCTION);
        let expected_uri: Uri = Url::from_file_path(registry.get_path(1))
            .unwrap()
            .as_str()
            .parse()?;
        assert_eq!(lsp_info.location.uri, expected_uri);
        assert_eq!(lsp_info.location.range.start.line, 29);
        assert_eq!(lsp_info.container_name, Some("MyClass".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_workspace_symbol_request_empty_query() {
        let server_state = Arc::new(RwLock::new(ServerState::new()));
        let params = WorkspaceSymbolParams {
            query: "".to_string(),
            ..Default::default()
        };

        let results = handle_workspace_symbol_request_async(params, server_state).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_handle_workspace_symbol_request_no_matches() {
        let registry = create_test_path_registry();
        let functions: HashSet<Symbol> = [create_test_symbol(
            "func_a",
            SymbolType::Function,
            5,
            0,
            None,
            "file1",
        )]
        .into_iter()
        .collect();
        let classes: HashSet<Symbol> = [create_test_symbol(
            "ClassB",
            SymbolType::Class,
            15,
            1,
            None,
            "file2",
        )]
        .into_iter()
        .collect();

        let server_state = Arc::new(RwLock::new(ServerState::new()));
        {
            let mut state = server_state.write().unwrap();
            state.set_symbols(SymbolData {
                functions,
                classes,
                path_registry: registry,
            });
        }

        let params = WorkspaceSymbolParams {
            query: "nonexistent".to_string(),
            ..Default::default()
        };

        let results = handle_workspace_symbol_request_async(params, server_state).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_handle_workspace_symbol_request_finds_symbol() {
        let registry = create_test_path_registry();
        let functions: HashSet<Symbol> = [
            create_test_symbol("find_this_func", SymbolType::Function, 5, 0, None, "file1"),
            create_test_symbol("another_func", SymbolType::Function, 20, 0, None, "file1"),
        ]
        .into_iter()
        .collect();
        let classes: HashSet<Symbol> = [create_test_symbol(
            "FindThisClass",
            SymbolType::Class,
            15,
            1,
            None,
            "file2",
        )]
        .into_iter()
        .collect();

        let server_state = Arc::new(RwLock::new(ServerState::new()));
        {
            let mut state = server_state.write().unwrap();
            state.set_symbols(SymbolData {
                functions: functions.clone(),
                classes: classes.clone(),
                path_registry: registry.clone(),
            });
        }

        let params_func = WorkspaceSymbolParams {
            query: "find_this_f".to_string(),
            ..Default::default()
        };
        let results_func =
            handle_workspace_symbol_request_async(params_func, server_state.clone()).await;
        assert_eq!(results_func.len(), 1);
        assert!(results_func[0].name.starts_with("find_this_func"));
        assert_eq!(results_func[0].kind, SymbolKind::FUNCTION);

        let params_class = WorkspaceSymbolParams {
            query: "FindThisC".to_string(),
            ..Default::default()
        };
        let results_class =
            handle_workspace_symbol_request_async(params_class, server_state.clone()).await;
        assert_eq!(results_class.len(), 1);
        assert!(results_class[0].name.starts_with("FindThisClass"));
        assert_eq!(results_class[0].kind, SymbolKind::CLASS);

        let params_multi = WorkspaceSymbolParams {
            query: "find".to_string(),
            ..Default::default()
        };
        let results_multi = handle_workspace_symbol_request_async(params_multi, server_state).await;
        let get_base_name =
            |s: &SymbolInformation| s.name.split(' ').next().unwrap_or("").to_string();
        assert_eq!(results_multi.len(), 2);
        let names: HashSet<String> = results_multi.iter().map(get_base_name).collect();
        assert!(names.contains("find_this_func"));
        assert!(names.contains("FindThisClass"));
    }
}
