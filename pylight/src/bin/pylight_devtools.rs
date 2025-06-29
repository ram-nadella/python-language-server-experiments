use pylight::parser::{create_parser, ParserBackend};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_http::{Response, Server};
use tracing::{debug, error, info, warn};

#[derive(Debug)]
struct PylightInstance {
    process: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    request_id: i64,
    pending_search_request: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct IndexRequest {
    path: String,
    #[serde(default = "default_parser")]
    parser: String,
}

fn default_parser() -> String {
    "tree-sitter".to_string()
}

#[derive(Serialize, Deserialize)]
struct SearchRequest {
    query: String,
}

#[derive(Serialize)]
struct SearchResponse {
    results: Vec<Value>, // Pass through raw LSP results
    duration_ms: u128,
}

type SharedPylight = Arc<Mutex<Option<PylightInstance>>>;

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("pylight_devtools=info".parse().unwrap()),
        )
        .init();

    info!("Starting Pylight DevTools");

    let pylight: SharedPylight = Arc::new(Mutex::new(None));
    let server = Server::http("0.0.0.0:8095").unwrap();
    info!("Pylight DevTools running at http://localhost:8095");

    for mut request in server.incoming_requests() {
        let url = request.url();
        let method = request.method();

        info!("Received {} request to {}", method, url);

        // Extract path without query parameters
        let path = if let Some(query_pos) = url.find('?') {
            &url[..query_pos]
        } else {
            url
        };

        match (method.as_str(), path) {
            ("GET", "/") => {
                let html = include_str!("../../static/devtools.html");
                let response = Response::from_string(html).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..]).unwrap(),
                );
                request.respond(response).unwrap();
            }
            ("POST", "/index") => {
                let mut content = String::new();
                request.as_reader().read_to_string(&mut content).unwrap();
                info!("Index request body: {}", content);

                let index_req: IndexRequest = match serde_json::from_str(&content) {
                    Ok(req) => req,
                    Err(e) => {
                        error!("Failed to parse index request: {}", e);
                        let response = Response::from_string(
                            json!({"status": "error", "message": format!("Invalid request: {e}")})
                                .to_string(),
                        )
                        .with_status_code(400);
                        request
                            .respond(
                                response.with_header(
                                    tiny_http::Header::from_bytes(
                                        b"Content-Type",
                                        b"application/json",
                                    )
                                    .unwrap(),
                                ),
                            )
                            .unwrap();
                        continue;
                    }
                };

                info!(
                    "Indexing codebase at: {} with parser: {}",
                    index_req.path, index_req.parser
                );

                let result = spawn_pylight(&index_req.path, &index_req.parser, pylight.clone());
                let response = if result.is_ok() {
                    info!("Successfully spawned pylight for {}", index_req.path);
                    Response::from_string(json!({"status": "success"}).to_string())
                } else {
                    let err = result.unwrap_err();
                    error!("Failed to spawn pylight: {}", err);
                    Response::from_string(json!({"status": "error", "message": err}).to_string())
                        .with_status_code(500)
                };
                request
                    .respond(
                        response.with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .unwrap(),
                        ),
                    )
                    .unwrap();
            }
            ("GET", "/search") => {
                // Parse query parameter from URL
                let url = request.url();
                let query = if let Some(query_pos) = url.find('?') {
                    let query_string = &url[query_pos + 1..];
                    let params: std::collections::HashMap<String, String> = query_string
                        .split('&')
                        .filter_map(|pair| {
                            let mut parts = pair.split('=');
                            match (parts.next(), parts.next()) {
                                (Some(key), Some(value)) => Some((
                                    key.to_string(),
                                    urlencoding::decode(value).ok()?.into_owned(),
                                )),
                                _ => None,
                            }
                        })
                        .collect();
                    params.get("q").cloned().unwrap_or_default()
                } else {
                    String::new()
                };

                debug!("Searching for: {}", query);

                let start = Instant::now();
                let results = search_symbols(&query, pylight.clone());
                let duration = start.elapsed().as_millis();

                debug!("Search completed in {}ms", duration);

                let response = match results {
                    Ok(symbols) => {
                        debug!("Found {} symbols", symbols.len());
                        let resp = SearchResponse {
                            results: symbols,
                            duration_ms: duration,
                        };
                        Response::from_string(serde_json::to_string(&resp).unwrap())
                    }
                    Err(e) => {
                        error!("Search failed: {}", e);
                        Response::from_string(json!({"error": e}).to_string()).with_status_code(500)
                    }
                };
                request
                    .respond(
                        response.with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .unwrap(),
                        ),
                    )
                    .unwrap();
            }
            ("POST", "/compare-parsers") => {
                let mut content = String::new();
                request.as_reader().read_to_string(&mut content).unwrap();

                let req: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(req) => req,
                    Err(e) => {
                        error!("Failed to parse compare request: {}", e);
                        let response = Response::from_string(
                            json!({"status": "error", "message": format!("Invalid request: {e}")})
                                .to_string(),
                        )
                        .with_status_code(400);
                        request
                            .respond(
                                response.with_header(
                                    tiny_http::Header::from_bytes(
                                        b"Content-Type",
                                        b"application/json",
                                    )
                                    .unwrap(),
                                ),
                            )
                            .unwrap();
                        continue;
                    }
                };

                let test_code = req.get("code").and_then(|v| v.as_str()).unwrap_or(
                    r#"
def test_function():
    pass

class TestClass:
    def method(self):
        pass
"#,
                );

                let response = compare_parsers(test_code);
                request
                    .respond(
                        Response::from_string(serde_json::to_string(&response).unwrap())
                            .with_header(
                                tiny_http::Header::from_bytes(b"Content-Type", b"application/json")
                                    .unwrap(),
                            ),
                    )
                    .unwrap();
            }
            _ => {
                warn!("404 Not Found: {} {}", method, url);
                request
                    .respond(Response::from_string("Not Found").with_status_code(404))
                    .unwrap();
            }
        }
    }
}

fn spawn_pylight(workspace_path: &str, parser: &str, pylight: SharedPylight) -> Result<(), String> {
    info!(
        "spawn_pylight called with path: {} and parser: {}",
        workspace_path, parser
    );

    // Kill existing instance if any
    {
        let mut guard = pylight.lock().unwrap();
        if let Some(mut instance) = guard.take() {
            info!("Killing existing pylight instance");
            let _ = instance.process.kill();
        }
    }

    info!("Spawning new pylight instance with {} parser", parser);

    // Spawn new pylight instance with parser argument
    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--release",
        "--bin",
        "pylight",
        "--",
        "--parser",
        parser,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn pylight: {e}"))?;

    info!("Pylight process spawned with PID: {:?}", child.id());

    let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
    let stderr = child.stderr.take().ok_or("Failed to get stderr")?;
    let stdout_reader = BufReader::new(stdout);
    let stderr_reader = BufReader::new(stderr);

    // Spawn a thread to read stderr
    std::thread::spawn({
        let mut stderr_reader = stderr_reader;
        move || {
            let mut line = String::new();
            loop {
                match stderr_reader.read_line(&mut line) {
                    Ok(0) => {
                        // EOF reached, pylight process has closed stderr
                        debug!("pylight stderr closed");
                        break;
                    }
                    Ok(_) => {
                        if !line.is_empty() {
                            warn!("pylight stderr: {}", line.trim());
                            line.clear();
                        }
                    }
                    Err(e) => {
                        warn!("Error reading pylight stderr: {}", e);
                        break;
                    }
                }
            }
        }
    });

    // Send initialize request
    let init_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": format!("file://{workspace_path}"),
            "capabilities": {}
        }
    });

    info!("Sending initialize request: {}", init_request);

    let mut instance = PylightInstance {
        process: child,
        stdin,
        stdout: stdout_reader,
        request_id: 2,
        pending_search_request: None,
    };

    send_request(&mut instance, &init_request)?;

    info!("Waiting for initialize response");

    // Read initialize response
    let response = read_lsp_message(&mut instance)?;

    info!("Initialize response: {}", response);

    // Check if initialization was successful
    if response.get("error").is_some() {
        error!("Initialize failed with error: {:?}", response.get("error"));
        return Err("Failed to initialize LSP server".to_string());
    }

    // Send initialized notification
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });

    info!("Sending initialized notification");
    send_request(&mut instance, &initialized)?;

    // Store the instance
    let mut guard = pylight.lock().unwrap();
    *guard = Some(instance);

    info!("Pylight instance stored and ready");

    Ok(())
}

fn search_symbols(query: &str, pylight: SharedPylight) -> Result<Vec<Value>, String> {
    debug!("search_symbols called with query: {}", query);

    let mut guard = pylight.lock().unwrap();
    let instance = guard
        .as_mut()
        .ok_or("Pylight not initialized. Please index a codebase first.")?;

    // Cancel previous search request if any
    if let Some(prev_id) = instance.pending_search_request {
        info!("Cancelling previous search request {}", prev_id);
        let cancel_request = json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {
                "id": prev_id
            }
        });
        let _ = send_request(instance, &cancel_request); // Ignore errors for cancel
    }

    let current_request_id = instance.request_id;
    let request = json!({
        "jsonrpc": "2.0",
        "id": current_request_id,
        "method": "workspace/symbol",
        "params": {
            "query": query
        }
    });
    instance.request_id += 1;
    instance.pending_search_request = Some(current_request_id);

    debug!("Sending workspace/symbol request: {}", request);
    send_request(instance, &request)?;

    // Read response
    debug!("Reading workspace/symbol response");
    let response = read_lsp_message(instance)?;

    // Clear pending request
    instance.pending_search_request = None;

    debug!("Workspace/symbol response: {}", response);

    if let Some(error) = response.get("error") {
        return Err(format!("LSP error: {error:?}"));
    }

    let results = response
        .get("result")
        .and_then(|r| r.as_array())
        .ok_or("Invalid response format")?;

    debug!(
        "LSP returned {} symbols, passing through as-is",
        results.len()
    );

    // Pass through the raw LSP results without transformation
    Ok(results.clone())
}

fn send_request(instance: &mut PylightInstance, request: &Value) -> Result<(), String> {
    let content = request.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", content.len());

    debug!("Sending LSP message header: {}", header.trim());
    debug!("Sending LSP message content: {}", content);

    instance
        .stdin
        .write_all(header.as_bytes())
        .map_err(|e| format!("Failed to write header: {e}"))?;
    instance
        .stdin
        .write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write content: {e}"))?;
    instance
        .stdin
        .flush()
        .map_err(|e| format!("Failed to flush: {e}"))?;

    Ok(())
}

fn read_lsp_message(instance: &mut PylightInstance) -> Result<Value, String> {
    debug!("Reading LSP message headers");

    // Read headers
    let mut content_length = None;

    loop {
        let mut line = String::new();
        instance
            .stdout
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read header: {e}"))?;

        debug!("Read header line: {}", line.trim());

        if line == "\r\n" || line == "\n" {
            break;
        }

        if line.starts_with("Content-Length: ") {
            let len_str = line.trim_start_matches("Content-Length: ").trim();
            content_length = Some(
                len_str
                    .parse::<usize>()
                    .map_err(|e| format!("Invalid content length: {e}"))?,
            );
            debug!("Found Content-Length: {}", content_length.unwrap());
        }
    }

    let content_length = content_length.ok_or("Missing Content-Length header")?;

    debug!("Reading {} bytes of content", content_length);

    // Read content
    let mut content = vec![0u8; content_length];
    instance
        .stdout
        .read_exact(&mut content)
        .map_err(|e| format!("Failed to read content: {e}"))?;

    let content_str = String::from_utf8(content).map_err(|e| format!("Invalid UTF-8: {e}"))?;
    debug!("Read LSP message content: {}", content_str);

    let response: Value =
        serde_json::from_str(&content_str).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    Ok(response)
}

fn compare_parsers(code: &str) -> Value {
    use std::path::Path;

    let test_path = Path::new("test.py");

    // Parse with tree-sitter
    let ts_start = Instant::now();
    let ts_parser = create_parser(ParserBackend::TreeSitter).unwrap();
    let ts_symbols = ts_parser.parse_file(test_path, code).unwrap_or_default();
    let ts_duration = ts_start.elapsed();

    // Parse with ruff
    let ruff_start = Instant::now();
    let ruff_parser = create_parser(ParserBackend::Ruff).unwrap();
    let ruff_symbols = ruff_parser.parse_file(test_path, code).unwrap_or_default();
    let ruff_duration = ruff_start.elapsed();

    // Compare results
    let ts_symbol_info: Vec<Value> = ts_symbols
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "kind": format!("{:?}", s.kind),
                "line": s.line,
                "column": s.column,
                "container": s.container_name.as_ref(),
            })
        })
        .collect();

    let ruff_symbol_info: Vec<Value> = ruff_symbols
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "kind": format!("{:?}", s.kind),
                "line": s.line,
                "column": s.column,
                "container": s.container_name.as_ref(),
            })
        })
        .collect();

    let same_count = ts_symbols.len() == ruff_symbols.len();
    let same_symbols = ts_symbols
        .iter()
        .zip(ruff_symbols.iter())
        .all(|(ts, ruff)| {
            ts.name == ruff.name && ts.kind == ruff.kind && ts.container_name == ruff.container_name
        });

    json!({
        "tree_sitter": {
            "symbols": ts_symbol_info,
            "count": ts_symbols.len(),
            "duration_ms": ts_duration.as_secs_f64() * 1000.0,
        },
        "ruff": {
            "symbols": ruff_symbol_info,
            "count": ruff_symbols.len(),
            "duration_ms": ruff_duration.as_secs_f64() * 1000.0,
        },
        "comparison": {
            "same_count": same_count,
            "same_symbols": same_symbols,
            "differences": if !same_symbols {
                Some("Symbols differ in name, kind, or container")
            } else {
                None
            }
        }
    })
}
