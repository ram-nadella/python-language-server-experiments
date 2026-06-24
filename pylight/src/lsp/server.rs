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
    workspace_roots: Vec<PathBuf>,
    cancelled_requests: Arc<Mutex<HashSet<RequestId>>>,
    _file_watcher: Option<FileWatcher>,
}

fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

fn workspace_roots_from_initialize_params(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|folder| uri_to_path(&folder.uri))
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }

    #[allow(deprecated)]
    params
        .root_uri
        .as_ref()
        .and_then(uri_to_path)
        .into_iter()
        .collect()
}

impl LspServer {
    pub fn new(parser_backend: ParserBackend) -> Result<Self> {
        let (connection, _io_threads) = Connection::stdio();

        Ok(Self {
            connection,
            index: Arc::new(SymbolIndex::new(parser_backend)),
            search_engine: Arc::new(SearchEngine::new()),
            workspace_roots: Vec::new(),
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

        // Extract all workspace roots VS Code sends for multi-root workspaces.
        if let Ok(params) = serde_json::from_value::<InitializeParams>(initialization_params) {
            self.workspace_roots = workspace_roots_from_initialize_params(&params);

            if !self.workspace_roots.is_empty() {
                let roots = self.workspace_roots.clone();
                let index = self.index.clone();

                // Create one updater/watcher and subscribe it to every workspace folder.
                let updater = Arc::new(IndexUpdater::new_multi(self.index.clone(), roots.clone()));
                let watcher_config = WatcherConfig::default();

                match FileWatcher::new(watcher_config, updater) {
                    Ok(mut watcher) => {
                        let mut watching = false;
                        for root in &roots {
                            if let Err(e) = watcher.watch(root) {
                                tracing::error!(
                                    "Failed to start file watcher for {}: {}",
                                    root.display(),
                                    e
                                );
                            } else {
                                watching = true;
                                tracing::info!(
                                    "File watcher started for workspace: {}",
                                    root.display()
                                );
                            }
                        }
                        if watching {
                            self._file_watcher = Some(watcher);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to create file watcher: {}", e);
                    }
                }

                thread::spawn(move || {
                    for root in &roots {
                        if let Err(e) = index.clone().index_workspace(root) {
                            tracing::error!("Failed to index workspace {}: {}", root.display(), e);
                        }
                    }
                    tracing::info!("Initial workspace indexing completed");
                });
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_params_use_all_workspace_folders() {
        let params: InitializeParams = serde_json::from_value(json!({
            "processId": null,
            "rootUri": "file:///top",
            "capabilities": {},
            "workspaceFolders": [
                { "uri": "file:///repo-a", "name": "repo-a" },
                { "uri": "file:///repo-b", "name": "repo-b" }
            ]
        }))
        .unwrap();

        let roots = workspace_roots_from_initialize_params(&params);
        assert_eq!(
            roots,
            vec![PathBuf::from("/repo-a"), PathBuf::from("/repo-b")]
        );
    }

    #[test]
    fn initialize_params_fall_back_to_root_uri() {
        let params: InitializeParams = serde_json::from_value(json!({
            "processId": null,
            "rootUri": "file:///top",
            "capabilities": {}
        }))
        .unwrap();

        assert_eq!(
            workspace_roots_from_initialize_params(&params),
            vec![PathBuf::from("/top")]
        );
    }
}
