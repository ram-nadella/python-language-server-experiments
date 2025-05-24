use anyhow::Result;
use clap::Parser as ClapParser;
use flate2::read::GzDecoder;
use std::collections::HashSet;
use std::fs::File;
use std::io::{stderr, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

use lsp_server::{Connection, ErrorCode, Message, Response, ResponseError};
use lsp_types::{
    Location, OneOf, Position, Range, ServerCapabilities, SymbolInformation, SymbolKind,
    WorkspaceSymbolParams,
};
use serde_json::{self, Value};

use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;
use symbol_experiments::search::search_symbols;
use symbol_experiments::symbols::{PathRegistry, Symbol, SymbolData, SymbolStats, SymbolType};

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Load symbols from this file instead of scanning directory
    #[arg(short, long)]
    load: Option<PathBuf>,

    /// Listen on this TCP port instead of using stdio
    #[arg(long)]
    port: Option<u16>,
}

/// Load symbols from a previously saved file
fn load_symbols_from_file(
    path: &PathBuf,
) -> Result<(HashSet<Symbol>, HashSet<Symbol>, PathRegistry)> {
    info!("Loading symbols from {}...", path.display());

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let decoder = GzDecoder::new(reader);

    info!("Starting deserialization...");
    let data: SymbolData = bincode::deserialize_from(decoder)?;

    info!("Converting to symbol collections...");
    let (functions, classes, paths) = data.into_symbols();

    // Create a new PathRegistry and populate it with the exact same paths in the same order
    let mut path_registry = PathRegistry::new();

    // First register all paths to ensure they have the same indices as when saved
    for (i, path) in paths.iter().enumerate() {
        let index = path_registry.register_path(path.clone());
        // Sanity check - make sure the index matches the position
        assert_eq!(
            index,
            i,
            "Path registry indexing error: expected index {} for path {}, got {}",
            i,
            path.display(),
            index
        );
    }

    info!(
        "Load complete! Loaded {} functions, {} classes, and {} paths",
        functions.len(),
        classes.len(),
        paths.len()
    );

    Ok((
        functions.into_iter().collect(),
        classes.into_iter().collect(),
        path_registry,
    ))
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

/// Handle a workspace symbol request synchronously (for tests)
#[cfg(test)]
fn handle_workspace_symbol_request(
    params: WorkspaceSymbolParams,
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
) -> Vec<SymbolInformation> {
    // Convert to Arc types for the async function
    let functions_arc = Arc::new(functions.clone());
    let classes_arc = Arc::new(classes.clone());
    let path_registry_arc = Arc::new(path_registry.clone());

    // Use a basic async runtime to call the async function
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle_workspace_symbol_request_async(
            params,
            functions_arc,
            classes_arc,
            path_registry_arc,
        ))
}

/// Handle a workspace symbol request from the LSP client asynchronously
async fn handle_workspace_symbol_request_async(
    params: WorkspaceSymbolParams,
    functions: Arc<HashSet<Symbol>>,
    classes: Arc<HashSet<Symbol>>,
    path_registry: Arc<PathRegistry>,
) -> Vec<SymbolInformation> {
    info!(
        "Handling workspace symbol request asynchronously: query='{}'",
        params.query
    );

    // If the query is empty, return an empty result
    if params.query.is_empty() {
        return Vec::new();
    }

    // Perform the search
    let search_start = Instant::now();
    let (results, metrics) = search_symbols(
        &params.query,
        &functions,
        &classes,
        &path_registry,
        false,
    );
    let search_time = search_start.elapsed();

    let result_count = results.len();
    info!(
        "Search completed: found {} results in {}ms",
        result_count,
        search_time.as_millis()
    );
    info!(
        "Search metrics: matcher_init={}ms, search={}ms, sort={}ms, total={}ms",
        metrics.matcher_init_time_ms,
        metrics.search_time_ms,
        metrics.sort_time_ms,
        metrics.total_time_ms
    );

    // truncate results to 100 symbols
    let max_results = 100;
    if result_count > max_results {
        info!("Truncating results to {} symbols", max_results);
    }

    // Convert the results to LSP format, filtering out None values from conversion errors
    let lsp_symbols: Vec<SymbolInformation> = results
        .iter()
        .filter_map(|(symbol, score)| to_lsp_symbol_information(symbol, &path_registry, *score)) // Use filter_map
        .take(max_results)
        .collect();

    info!("Converted {} symbols to LSP format", lsp_symbols.len());
    lsp_symbols
}

/// Main LSP server loop
fn run_server(
    functions: HashSet<Symbol>,
    classes: HashSet<Symbol>,
    path_registry: PathRegistry,
    port: Option<u16>, // Added port argument
) -> Result<()> {
    info!(
        "Starting LSP server with {} functions and {} classes",
        functions.len(),
        classes.len()
    );

    // Create a tokio runtime for handling async tasks
    let rt = Runtime::new()?;

    // Wrap our data structures in Arc for sharing between threads
    let functions = Arc::new(functions);
    let classes = Arc::new(classes);
    let path_registry = Arc::new(path_registry);

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

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    info!("Shutdown request received, exiting...");
                    return Ok(());
                }

                // Handle different LSP requests
                match req.method.as_str() {
                    // Workspace symbol request - this is the main functionality we're providing
                    "workspace/symbol" => {
                        info!("Received workspace/symbol request with id: {:?}", req.id);

                        // Clone the necessary data for the async task
                        let functions_clone = functions.clone();
                        let classes_clone = classes.clone();
                        let path_registry_clone = path_registry.clone();
                        let sender_clone = sender.clone();
                        let req_id = req.id.clone();

                        match serde_json::from_value::<WorkspaceSymbolParams>(req.params) {
                            Ok(params) => {
                                info!(
                                    "Processing workspace/symbol request with query: '{}'",
                                    params.query
                                );

                                // Spawn an async task to handle the request
                                rt.spawn(async move {
                                    let symbols = handle_workspace_symbol_request_async(
                                        params,
                                        functions_clone,
                                        classes_clone,
                                        path_registry_clone,
                                    )
                                    .await;

                                    let symbol_count = symbols.len();
                                    info!("Async search completed with {} results", symbol_count);

                                    // Create and send the response
                                    match serde_json::to_value(symbols) {
                                        Ok(symbols_value) => {
                                            let resp = Response {
                                                id: req_id,
                                                result: Some(symbols_value),
                                                error: None,
                                            };
                                            if let Err(e) =
                                                sender_clone.send(Message::Response(resp))
                                            {
                                                tracing::error!("Failed to send response: {}", e);
                                            }
                                            info!("Sent response with {} symbols", symbol_count);
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to serialize symbols: {}", e);
                                            let resp = Response {
                                                id: req_id,
                                                result: None,
                                                error: Some(ResponseError {
                                                    code: ErrorCode::InternalError as i32,
                                                    message: format!("Error: {}", e),
                                                    data: None,
                                                }),
                                            };
                                            let _ = sender_clone.send(Message::Response(resp));
                                        }
                                    }
                                });

                                info!("Spawned async task for workspace/symbol request");
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
                        info!("Received unsupported request: {}", req.method);
                        let resp = Response {
                            id: req.id,
                            result: Some(Value::Null),
                            error: None,
                        };
                        connection.sender.send(Message::Response(resp))?;
                    }
                }
            }
            Message::Response(resp) => {
                info!("Received response: {:?}", resp);
            }
            Message::Notification(not) => {
                info!("Received notification: {}", not.method);
            }
        }
    }

    // Wait for the io threads to finish
    io_threads.join()?;
    info!("LSP server shutting down");

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
    let start = Instant::now();

    info!("Starting LSP server with args: {:?}", args);

    // Decide whether to load from file or scan directory
    let (functions, classes, path_registry) = if let Some(load_path) = &args.load {
        info!("Loading symbols from file: {}", load_path.display());
        load_symbols_from_file(load_path)?
    } else {
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

        (functions, classes, path_registry)
    };

    info!(
        "Symbol loading complete in {}ms",
        start.elapsed().as_millis()
    );
    info!(
        "Found {} functions and {} classes",
        functions.len(),
        classes.len()
    );

    // Run the LSP server with the loaded symbols
    run_server(functions, classes, path_registry, args.port)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use lsp_types::{Position, Range, SymbolKind, Uri};
    use serde_json::{json, Value};
    use std::collections::HashSet;
    use std::io::{self, BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Once;
    use symbol_experiments::symbols::{ParentContext, SymbolContext, SymbolData, SymbolType};
    use tempfile::NamedTempFile;

    // Ensure the binary is built before running integration tests
    static BUILD: Once = Once::new();
    fn ensure_binary_built() {
        BUILD.call_once(|| {
            let status = Command::new("cargo")
                .args(["build", "--bin", "symbol_search_lsp"])
                .status()
                .expect("Failed to execute cargo build");
            assert!(status.success(), "Cargo build failed");
        });
    }

    // Helper to get the path to the built binary
    fn get_binary_path() -> PathBuf {
        // Assumes standard cargo build directory structure
        let mut path = std::env::current_exe().expect("Failed to get current exe path");
        path.pop(); // Pop the executable name
        if path.ends_with("deps") {
            path.pop(); // Pop "deps"
        }
        path.join("symbol_search_lsp") // Adjust if your binary name is different
    }

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

    // Helper to write LSP messages
    fn write_lsp_message<W: Write>(writer: &mut W, message: &Value) -> io::Result<()> {
        let msg_str = message.to_string();
        write!(
            writer,
            "Content-Length: {}\r\n\r\n{}",
            msg_str.len(),
            msg_str
        )
    }

    // Helper to read LSP messages
    fn read_lsp_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        let mut headers = String::new();

        // Read headers
        loop {
            headers.clear();
            if reader.read_line(&mut headers)? == 0 {
                return Ok(None); // EOF
            }
            if headers.trim().is_empty() {
                break; // End of headers
            }
            if let Some(len_str) = headers.strip_prefix("Content-Length: ") {
                if let Ok(len) = len_str.trim().parse::<usize>() {
                    content_length = Some(len);
                }
            }
        }

        // Read body
        if let Some(len) = content_length {
            let mut body_buf = vec![0; len];
            reader.read_exact(&mut body_buf)?;
            let body_str = String::from_utf8(body_buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            serde_json::from_str(&body_str)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                .map(Some)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing Content-Length header",
            ))
        }
    }

    // Helper to create a temporary symbol file for testing
    fn create_test_symbol_file(
        functions: &HashSet<Symbol>,
        classes: &HashSet<Symbol>,
        path_registry: &PathRegistry,
    ) -> Result<NamedTempFile> {
        let temp_file = NamedTempFile::new()?;
        let file = File::create(temp_file.path())?;
        let encoder = GzEncoder::new(file, Compression::default());

        let symbol_data = SymbolData::new(
            functions.iter().cloned().collect(),
            classes.iter().cloned().collect(),
            path_registry.paths.clone(),
        );

        bincode::serialize_into(encoder, &symbol_data)?;
        Ok(temp_file)
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

    #[test]
    fn test_handle_workspace_symbol_request_empty_query() {
        let functions = HashSet::new();
        let classes = HashSet::new();
        let registry = create_test_path_registry();
        let params = WorkspaceSymbolParams {
            query: "".to_string(),
            ..Default::default()
        };

        let results = handle_workspace_symbol_request(
            params,
            &functions,
            &classes,
            &registry,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_handle_workspace_symbol_request_no_matches() {
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
        let params = WorkspaceSymbolParams {
            query: "nonexistent".to_string(),
            ..Default::default()
        };

        let results = handle_workspace_symbol_request(
            params,
            &functions,
            &classes,
            &registry,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_handle_workspace_symbol_request_finds_symbol() {
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

        let params_func = WorkspaceSymbolParams {
            query: "find_this_f".to_string(),
            ..Default::default()
        };
        let results_func = handle_workspace_symbol_request(
            params_func,
            &functions,
            &classes,
            &registry,
        );
        assert_eq!(results_func.len(), 1);
        assert!(results_func[0].name.starts_with("find_this_func"));
        assert_eq!(results_func[0].kind, SymbolKind::FUNCTION);

        let params_class = WorkspaceSymbolParams {
            query: "FindThisC".to_string(),
            ..Default::default()
        };
        let results_class = handle_workspace_symbol_request(
            params_class,
            &functions,
            &classes,
            &registry,
        );
        assert_eq!(results_class.len(), 1);
        assert!(results_class[0].name.starts_with("FindThisClass"));
        assert_eq!(results_class[0].kind, SymbolKind::CLASS);

        let params_multi = WorkspaceSymbolParams {
            query: "find".to_string(),
            ..Default::default()
        };
        let results_multi = handle_workspace_symbol_request(
            params_multi,
            &functions,
            &classes,
            &registry,
        );
        let get_base_name =
            |s: &SymbolInformation| s.name.split(' ').next().unwrap_or("").to_string();
        assert_eq!(results_multi.len(), 2);
        let names: HashSet<String> = results_multi.iter().map(get_base_name).collect();
        assert!(names.contains("find_this_func"));
        assert!(names.contains("FindThisClass"));
    }

    #[test]
    fn test_lsp_server_integration_symbol_search() -> Result<()> {
        ensure_binary_built();

        let registry = create_test_path_registry();
        let functions: HashSet<Symbol> = [
            create_test_symbol(
                "searchable_func",
                SymbolType::Function,
                10,
                0,
                None,
                "file1",
            ),
            create_test_symbol("another_func", SymbolType::Function, 20, 0, None, "file1"),
        ]
        .into_iter()
        .collect();
        let classes: HashSet<Symbol> = [create_test_symbol(
            "SearchableClass",
            SymbolType::Class,
            5,
            1,
            None,
            "file2",
        )]
        .into_iter()
        .collect();

        let temp_symbol_file = create_test_symbol_file(&functions, &classes, &registry)?;
        let symbol_file_path = temp_symbol_file.path().to_str().unwrap();

        let binary_path = get_binary_path();
        let mut child = Command::new(&binary_path)
            .args(["--load", symbol_file_path])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn LSP server process");

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let init_params = json!({
            "processId": 1234,
            "rootUri": null,
            "capabilities": {},
            "trace": "off"
        });
        let init_request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": init_params
        });
        write_lsp_message(&mut stdin, &init_request)?;
        stdin.flush()?;

        let _init_response =
            read_lsp_message(&mut reader)?.expect("Server did not send initialize response");

        let initialized_notification = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        write_lsp_message(&mut stdin, &initialized_notification)?;
        stdin.flush()?;

        let query = "searchable";
        let symbol_request_params = json!({ "query": query });
        let symbol_request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/symbol",
            "params": symbol_request_params
        });
        write_lsp_message(&mut stdin, &symbol_request)?;
        stdin.flush()?;

        let symbol_response =
            read_lsp_message(&mut reader)?.expect("Server did not send symbol response");

        assert_eq!(symbol_response["id"], 2);
        assert!(symbol_response["error"].is_null());

        let result_value = symbol_response["result"].clone();
        let symbols: Vec<SymbolInformation> =
            serde_json::from_value(result_value).expect("Failed to deserialize symbol information");

        assert_eq!(symbols.len(), 2);

        let get_base_name =
            |s: &SymbolInformation| s.name.split(' ').next().unwrap_or("").to_string();
        let names: HashSet<String> = symbols.iter().map(get_base_name).collect();

        assert!(names.contains("searchable_func"));
        assert!(names.contains("SearchableClass"));

        let func_symbol = symbols
            .iter()
            .find(|s| get_base_name(s) == "searchable_func")
            .unwrap();
        assert_eq!(func_symbol.kind, SymbolKind::FUNCTION);
        let expected_func_uri: Uri = Url::from_file_path("/test/path/file1.py")
            .unwrap()
            .as_str()
            .parse()?;
        assert_eq!(func_symbol.location.uri, expected_func_uri);
        assert_eq!(func_symbol.location.range.start.line, 9);

        let class_symbol = symbols
            .iter()
            .find(|s| get_base_name(s) == "SearchableClass")
            .unwrap();
        assert_eq!(class_symbol.kind, SymbolKind::CLASS);
        let expected_class_uri: Uri = Url::from_file_path("/test/path/file2.py")
            .unwrap()
            .as_str()
            .parse()?;
        assert_eq!(class_symbol.location.uri, expected_class_uri);
        assert_eq!(class_symbol.location.range.start.line, 4);

        let shutdown_request = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        });
        write_lsp_message(&mut stdin, &shutdown_request)?;
        stdin.flush()?;

        let _shutdown_response =
            read_lsp_message(&mut reader)?.expect("Server did not send shutdown response");
        assert_eq!(_shutdown_response["id"], 3);
        assert!(_shutdown_response["result"].is_null());

        let exit_notification = json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        });
        write_lsp_message(&mut stdin, &exit_notification)?;
        stdin.flush()?;

        let status = child.wait()?;
        assert!(status.success());

        Ok(())
    }
}
