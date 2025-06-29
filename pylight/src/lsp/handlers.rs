//! LSP request handlers

use crate::{Result, SearchEngine, SymbolIndex};
use lsp_server::RequestId;
use lsp_types::{
    Location, Position, Range, SymbolInformation, SymbolKind as LspSymbolKind,
    WorkspaceSymbolParams,
};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;

pub fn handle_workspace_symbol(
    params: WorkspaceSymbolParams,
    index: Arc<SymbolIndex>,
    search_engine: Arc<SearchEngine>,
    cancelled_requests: Arc<Mutex<HashSet<RequestId>>>,
    request_id: RequestId,
) -> Result<Vec<SymbolInformation>> {
    let start = std::time::Instant::now();

    // Use with_all_symbols to avoid cloning the entire symbol list
    let result = index.with_all_symbols(|all_symbols| {
        let get_symbols_duration = start.elapsed();

        // Check if cancelled after getting symbols
        if cancelled_requests.lock().contains(&request_id) {
            tracing::info!("Request {:?} cancelled after getting symbols", request_id);
            cancelled_requests.lock().remove(&request_id);
            return Ok(vec![]);
        }

        let search_start = std::time::Instant::now();
        let search_results = search_engine.search(&params.query, all_symbols);
        let search_duration = search_start.elapsed();

        // Check if cancelled after search
        if cancelled_requests.lock().contains(&request_id) {
            tracing::info!("Request {:?} cancelled after search", request_id);
            cancelled_requests.lock().remove(&request_id);
            return Ok(vec![]);
        }

        tracing::info!(
            "Symbol search breakdown - get_all_symbols: {:?}, search: {:?}, for query {}",
            get_symbols_duration,
            search_duration,
            params.query,
        );

        let lsp_symbols: Vec<SymbolInformation> = search_results
            .into_iter()
            .take(200) // Limit results
            .filter_map(|result| {
                let symbol = &result.symbol;
                let uri = url::Url::from_file_path(&symbol.file_path).ok()?;

                #[allow(deprecated)]
                Some(SymbolInformation {
                    name: symbol.name.clone(),
                    kind: match symbol.kind {
                        crate::SymbolKind::Function | crate::SymbolKind::NestedFunction => {
                            LspSymbolKind::FUNCTION
                        }
                        crate::SymbolKind::Method => LspSymbolKind::METHOD,
                        crate::SymbolKind::Class | crate::SymbolKind::NestedClass => {
                            LspSymbolKind::CLASS
                        }
                    },
                    tags: None,
                    location: Location {
                        uri: uri.as_str().parse().unwrap(),
                        range: Range {
                            start: Position {
                                line: (symbol.line as u32).saturating_sub(1),
                                character: symbol.column as u32,
                            },
                            end: Position {
                                line: (symbol.line as u32).saturating_sub(1),
                                character: symbol.column as u32,
                            },
                        },
                    },
                    container_name: symbol.container_name.clone(),
                    deprecated: None,
                })
            })
            .collect();

        Ok(lsp_symbols)
    });

    result
}
