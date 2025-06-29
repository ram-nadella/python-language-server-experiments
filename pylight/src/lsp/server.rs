//! Core LSP server implementation

use crate::index::updater::IndexUpdater;
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
    pub fn new() -> Result<Self> {
        let (connection, _io_threads) = Connection::stdio();

        Ok(Self {
            connection,
            index: Arc::new(SymbolIndex::new()),
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
                                let is_cancelled = self
                                    .cancelled_requests
                                    .lock()
                                    .contains(&request_id);
                                if is_cancelled {
                                    tracing::info!(
                                        "Request {} was cancelled before processing",
                                        request_id
                                    );
                                    self.cancelled_requests.lock().remove(&request_id);
                                    continue;
                                }

                                let query = params.query.clone();
                                let cancelled_requests = self.cancelled_requests.clone();
                                let req_id_for_check = request_id.clone();

                                match super::handlers::handle_workspace_symbol(
                                    params,
                                    self.index.clone(),
                                    self.search_engine.clone(),
                                    cancelled_requests,
                                    req_id_for_check,
                                ) {
                                    Ok(symbols) => {
                                        let duration = start.elapsed();
                                        tracing::info!(
                                            "Request {} (workspace/symbol '{}') completed in {:?} - {} results",
                                            request_id,
                                            query,
                                            duration,
                                            symbols.len()
                                        );

                                        let resp = Response {
                                            id: req.id,
                                            result: Some(serde_json::to_value(symbols).unwrap()),
                                            error: None,
                                        };
                                        self.connection
                                            .sender
                                            .send(Message::Response(resp))
                                            .unwrap();
                                    }
                                    Err(e) => {
                                        let duration = start.elapsed();
                                        tracing::error!(
                                            "Request {} (workspace/symbol '{}') failed in {:?} - {}",
                                            request_id,
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
                                        self.connection
                                            .sender
                                            .send(Message::Response(resp))
                                            .unwrap();
                                    }
                                }
                            }
                            Err(e) => {
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
