use anyhow::Result;
use clap::Parser as ClapParser;
use std::collections::HashMap;
use std::io::stderr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

use lsp_server::{Connection, ErrorCode, Message, Response, ResponseError};
use lsp_types::{OneOf, ServerCapabilities, WorkspaceSymbolParams};
use serde_json::{self, Value};

// File watching imports
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;
use symbol_experiments::symbol_store::{SymbolData, SymbolStore};
use symbol_experiments::symbols::{PathRegistry, SymbolStats};

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

/// Process file changes in parallel
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

    // Parse all modified/created files in parallel
    let new_symbols = if !files_to_parse.is_empty() {
        let stats = SymbolStats::new();
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
            functions: std::collections::HashSet::new(),
            classes: std::collections::HashSet::new(),
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
            // Ignore other event types
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
        RecursiveMode::Recursive
    };

    watcher
        .watch(&directory, mode)
        .map_err(|e| anyhow::anyhow!("Failed to start watching directory: {}", e))?;

    info!(
        "File watcher started for directory: {}",
        directory.display()
    );

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

/// Debounced change processor that uses the symbol store with adaptive debouncing
async fn debounced_change_processor(
    mut change_receiver: mpsc::UnboundedReceiver<FileChange>,
    symbol_store: SymbolStore,
    base_dir: PathBuf,
) -> Result<()> {
    let mut pending_changes: HashMap<PathBuf, FileChange> = HashMap::new();
    let mut first_change_time: Option<Instant> = None;

    const SHORT_DEBOUNCE_MS: u64 = 10; // Wait 10ms after each change
    const MAX_WAIT_SECONDS: u64 = 5; // Maximum 5 seconds before forcing processing

    loop {
        // Wait for changes or timeout
        let change_result = if pending_changes.is_empty() {
            // No pending changes, wait indefinitely for the first change
            change_receiver.recv().await
        } else {
            // We have pending changes, use adaptive debouncing strategy
            let time_since_first_change = first_change_time
                .map(|t| t.elapsed())
                .unwrap_or(Duration::from_secs(0));

            // Check if we've hit the maximum wait time
            if time_since_first_change >= Duration::from_secs(MAX_WAIT_SECONDS) {
                // Force processing after 5 seconds
                None
            } else {
                // Wait for the short debounce period (10ms)
                match timeout(
                    Duration::from_millis(SHORT_DEBOUNCE_MS),
                    change_receiver.recv(),
                )
                .await
                {
                    Ok(change) => change,
                    Err(_) => None, // Timeout occurred - no new changes in 10ms, process now
                }
            }
        };

        match change_result {
            Some(change) => {
                // New change received
                debug!(
                    "File change detected: {:?} - {:?} (pending: {})",
                    change.change_type,
                    change.path,
                    pending_changes.len()
                );

                // Track when we first started accumulating changes
                if pending_changes.is_empty() {
                    first_change_time = Some(Instant::now());
                }

                // Update pending changes (latest change for each path wins)
                pending_changes.insert(change.path.clone(), change);
            }
            None => {
                // Timeout or channel closed - process pending changes
                if !pending_changes.is_empty() {
                    let changes: Vec<FileChange> = pending_changes.values().cloned().collect();
                    let change_count = changes.len();

                    // Check if we hit the maximum wait time and log it
                    let time_since_first = first_change_time
                        .map(|t| t.elapsed())
                        .unwrap_or(Duration::from_secs(0));

                    if time_since_first >= Duration::from_secs(MAX_WAIT_SECONDS) {
                        info!(
                            "⏰ Processing {} file changes after hitting maximum wait time of {}s (changes kept coming)",
                            change_count,
                            MAX_WAIT_SECONDS
                        );
                    } else {
                        info!(
                            "Processing {} debounced file changes after {}ms quiet period",
                            change_count,
                            time_since_first.as_millis()
                        );
                    }

                    pending_changes.clear();
                    first_change_time = None;

                    let process_start = Instant::now();

                    // Parse the files in parallel
                    let parse_start = Instant::now();
                    match process_file_changes_parallel(&changes, &base_dir).await {
                        Ok(parsed_changes) => {
                            let parse_time = parse_start.elapsed();
                            info!(
                                "Parsed {} files in {}ms",
                                change_count,
                                parse_time.as_millis()
                            );

                            // Apply the changes to the symbol store
                            let apply_start = Instant::now();
                            match symbol_store
                                .incremental_update(
                                    changes
                                        .iter()
                                        .filter(|c| {
                                            matches!(
                                                c.change_type,
                                                FileChangeType::Created | FileChangeType::Modified
                                            )
                                        })
                                        .map(|c| c.path.clone())
                                        .collect(),
                                    parsed_changes.deleted_files,
                                    parsed_changes.new_symbols,
                                )
                                .await
                            {
                                Ok(new_version) => {
                                    let apply_time = apply_start.elapsed();
                                    let total_time = process_start.elapsed();
                                    info!("✅ Applied {} file changes in {}ms (parse: {}ms, apply: {}ms, version: {})", 
                                        change_count,
                                        total_time.as_millis(),
                                        parse_time.as_millis(),
                                        apply_time.as_millis(),
                                        new_version
                                    );
                                }
                                Err(e) => {
                                    error!("❌ Failed to apply incremental update: {}", e);
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

/// Handle a workspace symbol request using the symbol store
async fn handle_workspace_symbol_request_async(
    params: WorkspaceSymbolParams,
    symbol_store: SymbolStore,
) -> Vec<lsp_types::SymbolInformation> {
    info!(
        "🔍 Handling workspace symbol request: query='{}'",
        params.query
    );

    // Check if the store is ready first
    if !symbol_store.is_ready() {
        info!("🔍 Symbol store not ready yet, returning empty result");
        return Vec::new();
    }

    // Add debugging info about the store state
    let db = symbol_store.read();
    info!(
        "🔍 Store state: version={}, functions={}, classes={}",
        db.version,
        db.functions.len(),
        db.classes.len()
    );

    // Log some symbol names for debugging
    if !db.functions.is_empty() {
        let first_few: Vec<&str> = db
            .functions
            .iter()
            .take(3)
            .map(|s| s.name.as_str())
            .collect();
        info!("🔍 First few function names: {:?}", first_few);
    }
    if !db.classes.is_empty() {
        let first_few: Vec<&str> = db.classes.iter().take(3).map(|s| s.name.as_str()).collect();
        info!("🔍 First few class names: {:?}", first_few);
    }
    drop(db); // Release the read lock

    let search_start = Instant::now();

    // Handle empty queries by returning some symbols (common LSP behavior)
    let results = if params.query.is_empty() {
        info!("🔍 Empty query - returning first 20 symbols");

        // For empty queries, directly convert symbols to LSP format (no searching needed!)
        let db = symbol_store.read();
        let mut all_symbols = Vec::new();

        // Add some functions (up to 10) - convert directly to LSP format
        for symbol in db.functions.iter().take(10) {
            if let Some(lsp_symbol) = symbol_experiments::symbol_store::to_lsp_symbol_information(
                symbol,
                &db.path_registry,
                100, // arbitrary score for empty queries
                symbol_store.base_directory(),
            ) {
                all_symbols.push(lsp_symbol);
            }
            if all_symbols.len() >= 10 {
                break;
            }
        }

        // Add some classes (up to 10) - convert directly to LSP format
        for symbol in db.classes.iter().take(10) {
            if let Some(lsp_symbol) = symbol_experiments::symbol_store::to_lsp_symbol_information(
                symbol,
                &db.path_registry,
                100, // arbitrary score for empty queries
                symbol_store.base_directory(),
            ) {
                all_symbols.push(lsp_symbol);
            }
            if all_symbols.len() >= 20 {
                break;
            }
        }

        info!("🔍 Empty query returning {} symbols", all_symbols.len());
        all_symbols
    } else {
        // Perform lock-free search with LSP conversion
        info!("🔍 Performing search for query: '{}'", params.query);

        // Test the raw search first
        let raw_results = symbol_store.search(&params.query);
        info!("🔍 Raw search() returned {} results", raw_results.len());
        if !raw_results.is_empty() {
            let first_result = &raw_results[0];
            info!(
                "🔍 First raw result: {} (score: {})",
                first_result.0.name, first_result.1
            );
        }

        let results = symbol_store.search_to_lsp(&params.query, 100);
        info!("🔍 search_to_lsp() returned {} results", results.len());
        results
    };

    let search_time = search_start.elapsed();
    info!(
        "🔍 Search completed: found {} results in {}ms",
        results.len(),
        search_time.as_millis()
    );

    results
}

/// Main LSP server loop - now fully async but takes a pre-initialized connection
async fn run_server_async_with_connection(
    symbol_store: SymbolStore,
    connection: Connection,
    io_threads: lsp_server::IoThreads,
) -> Result<()> {
    info!("Starting async LSP server with lock-free symbol store");

    // Main message loop - now async
    info!("Entering async main message loop");
    let sender = connection.sender.clone();

    // Spawn the message processing loop as an async task
    let message_loop_handle = tokio::spawn(async move {
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

                    if connection.handle_shutdown(&req).unwrap_or(false) {
                        info!("📨 LSP: Shutdown request received, exiting...");
                        break;
                    }

                    match req.method.as_str() {
                        "workspace/symbol" => {
                            info!(
                                "🔍 LSP: Received workspace/symbol request with id: {:?}",
                                req.id
                            );
                            let request_start = Instant::now();

                            match serde_json::from_value::<WorkspaceSymbolParams>(req.params) {
                                Ok(params) => {
                                    info!("🔍 LSP: Processing workspace/symbol request id={:?} query='{}'", 
                                        req.id, params.query);

                                    // Handle the request directly (no need for async task since search is fast)
                                    let search_start = Instant::now();
                                    let symbols = handle_workspace_symbol_request_async(
                                        params,
                                        symbol_store.clone(),
                                    )
                                    .await;
                                    let search_time = search_start.elapsed();

                                    let symbol_count = symbols.len();
                                    info!("🔍 LSP: Search completed for id={:?} with {} results in {}ms", 
                                        req.id, symbol_count, search_time.as_millis());

                                    // Send response
                                    let serialize_start = Instant::now();
                                    match serde_json::to_value(symbols) {
                                        Ok(symbols_value) => {
                                            let serialize_time = serialize_start.elapsed();
                                            info!(
                                                "🔍 LSP: Serialized {} symbols in {}ms for id={:?}",
                                                symbol_count,
                                                serialize_time.as_millis(),
                                                req.id
                                            );

                                            let resp = Response {
                                                id: req.id.clone(),
                                                result: Some(symbols_value),
                                                error: None,
                                            };

                                            let send_start = Instant::now();
                                            match sender.send(Message::Response(resp)) {
                                                Ok(_) => {
                                                    let send_time = send_start.elapsed();
                                                    let total_time = request_start.elapsed();
                                                    info!("✅ LSP: Successfully sent response for id={:?} with {} symbols (send: {}ms, total: {}ms)", 
                                                        req.id, symbol_count, send_time.as_millis(), total_time.as_millis());
                                                }
                                                Err(e) => {
                                                    error!("❌ LSP: Failed to send response for id={:?}: {}", req.id, e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("❌ LSP: Failed to serialize symbols for id={:?}: {}", req.id, e);
                                            let resp = Response {
                                                id: req.id.clone(),
                                                result: None,
                                                error: Some(ResponseError {
                                                    code: ErrorCode::InternalError as i32,
                                                    message: format!("Serialization error: {}", e),
                                                    data: None,
                                                }),
                                            };
                                            if let Err(e2) = sender.send(Message::Response(resp)) {
                                                error!("❌ LSP: Failed to send error response for id={:?}: {}", req.id, e2);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to parse workspace/symbol params: {}", e);
                                    let resp = Response {
                                        id: req.id,
                                        result: None,
                                        error: Some(ResponseError {
                                            code: ErrorCode::InvalidParams as i32,
                                            message: format!("Invalid params: {}", e),
                                            data: None,
                                        }),
                                    };
                                    if let Err(e) = sender.send(Message::Response(resp)) {
                                        error!("Failed to send error response: {}", e);
                                    }
                                }
                            }
                        }

                        // For any other requests, respond with null
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
                            if let Err(e) = sender.send(Message::Response(resp)) {
                                error!("Failed to send null response: {}", e);
                            }
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
        info!("Message loop exiting");
    });

    // Wait for the message loop to complete
    if let Err(e) = message_loop_handle.await {
        error!("Message loop task failed: {}", e);
    }

    // Wait for the io threads to finish
    io_threads
        .join()
        .map_err(|_| anyhow::anyhow!("IO threads failed"))?;
    info!("LSP server shutting down");

    Ok(())
}

/// Background task to bootstrap symbol loading
async fn bootstrap_symbols(args: Args, symbol_store: SymbolStore) -> Result<()> {
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

    // Update the symbol store with loaded symbols
    let symbol_data = SymbolData {
        functions,
        classes,
        path_registry,
    };

    info!("About to send batch update to symbol store");
    match symbol_store.batch_update(symbol_data).await {
        Ok(version) => {
            info!(
                "Symbol bootstrap process completed successfully, version: {}",
                version
            );
        }
        Err(e) => {
            error!("Failed to update symbol store: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_env_filter(env_filter)
        .with_ansi(false)
        .init();

    let args = Args::parse();

    info!("🚀 Starting lock-free LSP server with args: {:?}", args);

    // Create a tokio runtime first
    info!("🔧 Creating tokio runtime");
    let rt = Runtime::new()?;
    info!("✅ Tokio runtime created successfully");

    // Run everything in the async context
    rt.block_on(async {
        // Set up LSP connection within async context
        info!("Setting up LSP connection");
        let (connection, io_threads) = if let Some(port) = args.port {
            info!("Starting LSP server on port {}", port);
            let addr = format!("127.0.0.1:{}", port);
            Connection::listen(addr)?
        } else {
            info!("Starting LSP server on stdio");
            Connection::stdio()
        };

        info!("LSP connection established");

        // Handle the initialize request
        let server_capabilities = serde_json::to_value(ServerCapabilities {
            workspace_symbol_provider: Some(OneOf::Left(true)),
            ..ServerCapabilities::default()
        })?;

        // Process initialize request
        let _initialize_result = connection.initialize(server_capabilities)?;
        info!("LSP server initialized successfully");

        // Create the symbol store and start writer thread (now within async context)
        // Convert base directory to absolute path to avoid path joining issues
        let base_directory = args
            .directory
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_dir().unwrap().join(&args.directory));
        let (symbol_store, writer_handle) = SymbolStore::new_with_writer(base_directory);

        // Give the writer thread a moment to fully start
        tokio::time::sleep(Duration::from_millis(100)).await;
        info!("Writer thread should be ready now");

        // Start the bootstrap process
        let bootstrap_store = symbol_store.clone();
        let bootstrap_args = args.clone();
        tokio::spawn(async move {
            if let Err(e) = bootstrap_symbols(bootstrap_args, bootstrap_store).await {
                error!("Bootstrap process failed: {}", e);
            }
        });

        // Start file watching
        let (change_sender, change_receiver) = mpsc::unbounded_channel();

        // Start the file watcher task
        let watcher_directory = args.directory.clone();
        let watcher_follow_links = args.follow_links;
        let watcher_change_sender = change_sender.clone();
        tokio::spawn(async move {
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

        // Start the debounced change processor
        let processor_store = symbol_store.clone();
        let processor_base_dir = args.directory.clone();
        tokio::spawn(async move {
            if let Err(e) =
                debounced_change_processor(change_receiver, processor_store, processor_base_dir)
                    .await
            {
                error!("Debounced change processor failed: {}", e);
            }
        });

        info!("File watching enabled with adaptive debouncing");

        // Spawn the writer thread cleanup task
        tokio::spawn(async move {
            if let Err(e) = writer_handle.await {
                error!("Database writer thread failed: {}", e);
            }
        });

        // Start the LSP server with the pre-initialized connection
        run_server_async_with_connection(symbol_store, connection, io_threads).await
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration, Instant};

    // Mock symbol store that tracks incremental_update calls
    #[derive(Clone)]
    struct MockSymbolStore {
        update_count: Arc<AtomicUsize>,
        update_calls: Arc<tokio::sync::Mutex<Vec<(Vec<PathBuf>, Vec<PathBuf>)>>>,
    }

    impl MockSymbolStore {
        fn new() -> Self {
            Self {
                update_count: Arc::new(AtomicUsize::new(0)),
                update_calls: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            }
        }

        async fn incremental_update(
            &self,
            added_files: Vec<PathBuf>,
            removed_files: Vec<PathBuf>,
            _parsed_symbols: SymbolData,
        ) -> Result<u64> {
            let count = self.update_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.update_calls
                .lock()
                .await
                .push((added_files, removed_files));
            Ok(count as u64)
        }

        async fn get_update_count(&self) -> usize {
            self.update_count.load(Ordering::SeqCst)
        }

        async fn get_update_calls(&self) -> Vec<(Vec<PathBuf>, Vec<PathBuf>)> {
            self.update_calls.lock().await.clone()
        }
    }

    // Mock process_file_changes_parallel function
    async fn mock_process_file_changes_parallel(
        changes: &[FileChange],
        _base_dir: &PathBuf,
    ) -> Result<ParsedFileChanges> {
        // Create mock symbol data based on the changes
        let mut deleted_files = Vec::new();

        for change in changes {
            if matches!(change.change_type, FileChangeType::Deleted) {
                deleted_files.push(change.path.clone());
            }
        }

        Ok(ParsedFileChanges {
            deleted_files,
            new_symbols: SymbolData {
                functions: std::collections::HashSet::new(),
                classes: std::collections::HashSet::new(),
                path_registry: symbol_experiments::symbols::PathRegistry::new(),
            },
        })
    }

    // Modified debounced_change_processor for testing
    async fn test_debounced_change_processor(
        mut change_receiver: mpsc::UnboundedReceiver<FileChange>,
        mock_store: MockSymbolStore,
        base_dir: PathBuf,
    ) -> Result<()> {
        let mut pending_changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let mut first_change_time: Option<Instant> = None;

        // Adaptive debouncing constants
        const SHORT_DEBOUNCE_MS: u64 = 10; // Wait 10ms after each change
        const MAX_WAIT_SECONDS: u64 = 5; // Maximum 5 seconds before forcing processing

        loop {
            // Wait for changes or timeout
            let change_result = if pending_changes.is_empty() {
                // No pending changes, wait indefinitely for the first change
                change_receiver.recv().await
            } else {
                // We have pending changes, use adaptive debouncing strategy
                let time_since_first_change = first_change_time
                    .map(|t| t.elapsed())
                    .unwrap_or(Duration::from_secs(0));

                // Check if we've hit the maximum wait time
                if time_since_first_change >= Duration::from_secs(MAX_WAIT_SECONDS) {
                    // Force processing after 5 seconds
                    None
                } else {
                    // Wait for the short debounce period (10ms)
                    match timeout(
                        Duration::from_millis(SHORT_DEBOUNCE_MS),
                        change_receiver.recv(),
                    )
                    .await
                    {
                        Ok(change) => change,
                        Err(_) => None, // Timeout occurred - no new changes in 10ms, process now
                    }
                }
            };

            match change_result {
                Some(change) => {
                    // Track when we first started accumulating changes
                    if pending_changes.is_empty() {
                        first_change_time = Some(Instant::now());
                    }

                    // Update pending changes (latest change for each path wins)
                    pending_changes.insert(change.path.clone(), change);
                }
                None => {
                    // Timeout or channel closed - process pending changes
                    if !pending_changes.is_empty() {
                        let changes: Vec<FileChange> = pending_changes.values().cloned().collect();

                        pending_changes.clear();
                        first_change_time = None;

                        // Parse the files in parallel (mocked)
                        match mock_process_file_changes_parallel(&changes, &base_dir).await {
                            Ok(parsed_changes) => {
                                // Apply the changes to the mock symbol store
                                let _ = mock_store
                                    .incremental_update(
                                        changes
                                            .iter()
                                            .filter(|c| {
                                                matches!(
                                                    c.change_type,
                                                    FileChangeType::Created
                                                        | FileChangeType::Modified
                                                )
                                            })
                                            .map(|c| c.path.clone())
                                            .collect(),
                                        parsed_changes.deleted_files,
                                        parsed_changes.new_symbols,
                                    )
                                    .await;
                            }
                            Err(_) => {
                                // Handle error in real implementation
                            }
                        }
                    } else {
                        // Channel closed and no pending changes
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_debounced_processor_single_change() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        // Send a single file change
        let change = FileChange {
            path: PathBuf::from("test.py"),
            change_type: FileChangeType::Modified,
        };
        tx.send(change).unwrap();

        // Wait a bit longer than the debounce period
        sleep(Duration::from_millis(50)).await;

        // Close the channel to stop the processor
        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Verify that exactly one update was called
        assert_eq!(mock_store.get_update_count().await, 1);
    }

    #[tokio::test]
    async fn test_debounced_processor_rapid_changes() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        // Send multiple rapid changes to the same file
        for _i in 0..5 {
            let change = FileChange {
                path: PathBuf::from("test.py"),
                change_type: FileChangeType::Modified,
            };
            tx.send(change).unwrap();

            // Small delay between changes (less than debounce period)
            sleep(Duration::from_millis(5)).await;
        }

        // Wait for debounce period to complete
        sleep(Duration::from_millis(50)).await;

        // Close the channel
        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Should only have one update call despite multiple changes
        assert_eq!(mock_store.get_update_count().await, 1);

        // Verify the update was called with the correct file
        let calls = mock_store.get_update_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, vec![PathBuf::from("test.py")]);
    }

    #[tokio::test]
    async fn test_debounced_processor_multiple_files() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        // Send changes to multiple files rapidly
        let files = ["file1.py", "file2.py", "file3.py"];
        for file in &files {
            let change = FileChange {
                path: PathBuf::from(file),
                change_type: FileChangeType::Modified,
            };
            tx.send(change).unwrap();
            sleep(Duration::from_millis(2)).await; // Very short delay
        }

        // Wait for debounce
        sleep(Duration::from_millis(50)).await;

        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Should batch all files into one update
        assert_eq!(mock_store.get_update_count().await, 1);

        let calls = mock_store.get_update_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.len(), 3); // All three files
    }

    #[tokio::test]
    async fn test_debounced_processor_separated_batches() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        // First batch of changes
        tx.send(FileChange {
            path: PathBuf::from("file1.py"),
            change_type: FileChangeType::Modified,
        })
        .unwrap();

        // Wait for first batch to process
        sleep(Duration::from_millis(50)).await;

        // Second batch of changes (after debounce period)
        tx.send(FileChange {
            path: PathBuf::from("file2.py"),
            change_type: FileChangeType::Modified,
        })
        .unwrap();

        // Wait for second batch
        sleep(Duration::from_millis(50)).await;

        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Should have two separate update calls
        assert_eq!(mock_store.get_update_count().await, 2);
    }

    #[tokio::test]
    async fn test_debounced_processor_max_wait_time() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        let start_time = Instant::now();

        // Send initial change
        tx.send(FileChange {
            path: PathBuf::from("file1.py"),
            change_type: FileChangeType::Modified,
        })
        .unwrap();

        // Keep sending changes every 100ms for 6 seconds (longer than max wait time)
        let mut _change_count = 1;
        for i in 0..60 {
            // 60 * 100ms = 6 seconds
            sleep(Duration::from_millis(100)).await;

            // Send a change every 100ms to keep the processor busy
            tx.send(FileChange {
                path: PathBuf::from(format!("file{}.py", i + 2)),
                change_type: FileChangeType::Modified,
            })
            .unwrap();
            _change_count += 1;

            // Check if processing happened around the 5-second mark
            if start_time.elapsed() >= Duration::from_secs(5)
                && start_time.elapsed() < Duration::from_secs(6)
            {
                if mock_store.get_update_count().await > 0 {
                    break; // Processing happened due to max wait time
                }
            }
        }

        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Should have processed at least once due to max wait time
        let update_count = mock_store.get_update_count().await;
        assert!(
            update_count > 0,
            "Expected at least one update due to max wait time"
        );

        // The processing should have happened around the 5-second mark
        let elapsed = start_time.elapsed();
        assert!(
            elapsed >= Duration::from_secs(5),
            "Processing should have been forced after 5 seconds"
        );
    }

    #[tokio::test]
    async fn test_debounced_processor_mixed_change_types() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mock_store = MockSymbolStore::new();
        let base_dir = PathBuf::from("/test");

        // Start the processor
        let processor_store = mock_store.clone();
        let processor_handle = tokio::spawn(async move {
            test_debounced_change_processor(rx, processor_store, base_dir).await
        });

        // Send different types of changes
        tx.send(FileChange {
            path: PathBuf::from("created.py"),
            change_type: FileChangeType::Created,
        })
        .unwrap();

        tx.send(FileChange {
            path: PathBuf::from("modified.py"),
            change_type: FileChangeType::Modified,
        })
        .unwrap();

        tx.send(FileChange {
            path: PathBuf::from("deleted.py"),
            change_type: FileChangeType::Deleted,
        })
        .unwrap();

        // Wait for processing
        sleep(Duration::from_millis(50)).await;

        drop(tx);
        processor_handle.await.unwrap().unwrap();

        // Verify processing happened
        assert_eq!(mock_store.get_update_count().await, 1);

        let calls = mock_store.get_update_calls().await;
        assert_eq!(calls.len(), 1);

        // Should have 2 added files (created + modified) and 1 deleted file
        assert_eq!(calls[0].0.len(), 2); // added files
        assert_eq!(calls[0].1.len(), 1); // deleted files
        assert_eq!(calls[0].1[0], PathBuf::from("deleted.py"));
    }
}
