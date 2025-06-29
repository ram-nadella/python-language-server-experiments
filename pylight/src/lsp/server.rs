//! Core LSP server implementation

use crate::index::updater::IndexUpdater;
use crate::parser::ParserBackend;
use crate::watcher::{FileWatcher, WatcherConfig};
use crate::{Error, Result, SearchEngine, SymbolIndex};
use lsp_server::{Connection, Message, RequestId, Response};
use lsp_types::{InitializeParams, ServerCapabilities, WorkspaceSymbolParams};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

pub struct LspServer {
    connection: Connection,
    index: Arc<SymbolIndex>,
    search_engine: Arc<SearchEngine>,
    workspace_root: Option<PathBuf>,
    cancelled_requests: Arc<Mutex<HashSet<RequestId>>>,
    _file_watcher: Option<FileWatcher>,
}

impl LspServer {
    pub fn new(parser_backend: ParserBackend) -> Result<Self> {
        let (connection, _io_threads) = Connection::stdio();

        Ok(Self {
            connection,
            index: Arc::new(SymbolIndex::new(parser_backend)),
            search_engine: Arc::new(SearchEngine::new()),
            workspace_root: None,
            cancelled_requests: Arc::new(Mutex::new(HashSet::new())),
            _file_watcher: None,
        })
    }

    pub fn run(mut self) -> Result<()> {
        // Initialize server
        let server_capabilities = ServerCapabilities {
            workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
            ..ServerCapabilities::default()
        };

        let initialization_params = self
            .connection
            .initialize(serde_json::to_value(server_capabilities).unwrap())
            .map_err(|e| Error::Lsp(format!("Failed to initialize: {e}")))?;

        // Extract workspace root
        if let Ok(params) = serde_json::from_value::<InitializeParams>(initialization_params) {
            #[allow(deprecated)]
            if let Some(root_uri) = params.root_uri {
                if let Ok(url) = url::Url::parse(root_uri.as_str()) {
                    if let Ok(path) = url.to_file_path() {
                        self.workspace_root = Some(path.clone());

                        // Start background indexing
                        let index = self.index.clone();
                        let root = path.clone();
                        let root_for_watcher = path.clone();

                        // Create the index updater and file watcher
                        let updater = Arc::new(IndexUpdater::new(
                            self.index.clone(),
                            root_for_watcher.clone(),
                        ));
                        let watcher_config = WatcherConfig::default();

                        match FileWatcher::new(watcher_config, updater) {
                            Ok(mut watcher) => {
                                // Start watching the workspace
                                if let Err(e) = watcher.watch(&root_for_watcher) {
                                    tracing::error!("Failed to start file watcher: {}", e);
                                } else {
                                    tracing::info!(
                                        "File watcher started for workspace: {}",
                                        root_for_watcher.display()
                                    );
                                    self._file_watcher = Some(watcher);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to create file watcher: {}", e);
                            }
                        }

                        thread::spawn(move || {
                            if let Err(e) = index.index_workspace(&root) {
                                tracing::error!("Failed to index workspace: {}", e);
                            } else {
                                tracing::info!("Initial workspace indexing completed");
                            }
                        });
                    }
                }
            }
        }

        // Main message loop
        for msg in &self.connection.receiver {
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req).unwrap() {
                        return Ok(());
                    }

                    // Handle workspace/symbol requests
                    if req.method == "workspace/symbol" {
                        let start = std::time::Instant::now();
                        let request_id = req.id.clone();

                        match serde_json::from_value::<WorkspaceSymbolParams>(req.params) {
                            Ok(params) => {
                                // Check if request was already cancelled
                                let is_cancelled =
                                    self.cancelled_requests.lock().contains(&request_id);
                                if is_cancelled {
                                    tracing::info!(
                                        "Request {} was cancelled before processing",
                                        request_id
                                    );
                                    self.cancelled_requests.lock().remove(&request_id);
                                    continue;
                                }

                                // Clone everything needed for the spawned thread
                                let query = params.query.clone();
                                let cancelled_requests = self.cancelled_requests.clone();
                                let req_id_for_check = request_id.clone();
                                let req_id_for_log = request_id.clone();
                                let req_id_for_cleanup = request_id.clone();
                                let index = self.index.clone();
                                let search_engine = self.search_engine.clone();
                                let connection_sender = self.connection.sender.clone();
                                let cancelled_requests_cleanup = self.cancelled_requests.clone();

                                // Spawn thread to handle the request asynchronously
                                // This allows the main loop to continue processing messages (like cancellations)
                                // while the search is running
                                thread::Builder::new()
                                    .name(format!("lsp-request-{req_id_for_log}"))
                                    .spawn(move || {
                                    let result = super::handlers::handle_workspace_symbol(
                                        params,
                                        index,
                                        search_engine,
                                        cancelled_requests,
                                        req_id_for_check,
                                    );

                                    match result {
                                        Ok(symbols) => {
                                            let duration = start.elapsed();
                                            tracing::info!(
                                                "Request {} (workspace/symbol '{}') completed in {:?} - {} results",
                                                req_id_for_log,
                                                query,
                                                duration,
                                                symbols.len()
                                            );

                                            let resp = Response {
                                                id: req.id,
                                                result: Some(serde_json::to_value(symbols).unwrap()),
                                                error: None,
                                            };
                                            if let Err(e) = connection_sender.send(Message::Response(resp)) {
                                                tracing::error!("Failed to send response: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            let duration = start.elapsed();
                                            tracing::error!(
                                                "Request {} (workspace/symbol '{}') failed in {:?} - {}",
                                                req_id_for_log,
                                                query,
                                                duration,
                                                e
                                            );

                                            let resp = Response {
                                                id: req.id,
                                                result: None,
                                                error: Some(lsp_server::ResponseError {
                                                    code: lsp_server::ErrorCode::InternalError as i32,
                                                    message: format!("Error: {e}"),
                                                    data: None,
                                                }),
                                            };
                                            if let Err(e) = connection_sender.send(Message::Response(resp)) {
                                                tracing::error!("Failed to send error response: {}", e);
                                            }
                                        }
                                    }

                                    // Clean up cancelled request tracking
                                    cancelled_requests_cleanup.lock().remove(&req_id_for_cleanup);
                                })
                                .expect("Failed to spawn request handler thread");
                            }
                            Err(e) => {
                                // Parameter parsing error - respond immediately
                                let resp = Response {
                                    id: req.id,
                                    result: None,
                                    error: Some(lsp_server::ResponseError {
                                        code: lsp_server::ErrorCode::InvalidParams as i32,
                                        message: format!("Invalid params: {e}"),
                                        data: None,
                                    }),
                                };
                                self.connection
                                    .sender
                                    .send(Message::Response(resp))
                                    .unwrap();
                            }
                        }
                    } else {
                        tracing::info!(
                            "Received LSP request we are not handling yet: {}",
                            req.method
                        )
                    }
                }
                Message::Response(_) => {}
                Message::Notification(notif) => {
                    // Handle $/cancelRequest notifications
                    if notif.method == "$/cancelRequest" {
                        #[derive(serde::Deserialize)]
                        struct CancelParams {
                            id: RequestId,
                        }

                        if let Ok(params) = serde_json::from_value::<CancelParams>(notif.params) {
                            tracing::info!("Received cancel request for id: {:?}", params.id);
                            self.cancelled_requests.lock().insert(params.id);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
