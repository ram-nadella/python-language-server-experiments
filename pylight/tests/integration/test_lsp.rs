use lsp_server::RequestId;
use parking_lot::Mutex;
use pylight::SymbolIndex;
use std::collections::HashSet;
use std::path::PathBuf;

#[test]
fn test_lsp_handler() {
    use pylight::{SearchEngine, Symbol, SymbolKind};
    use std::sync::Arc;

    // Create test symbols
    let index = Arc::new(SymbolIndex::default());
    let search_engine = Arc::new(SearchEngine::new());

    let symbols = vec![
        Symbol::new(
            "test_function".to_string(),
            SymbolKind::Function,
            PathBuf::from("/test/file.py"),
            10,
            0,
        ),
        Symbol::new(
            "TestClass".to_string(),
            SymbolKind::Class,
            PathBuf::from("/test/file.py"),
            20,
            0,
        ),
    ];

    index
        .add_file(PathBuf::from("/test/file.py"), symbols)
        .unwrap();

    // Test workspace symbol search
    let params = lsp_types::WorkspaceSymbolParams {
        query: "test".to_string(),
        ..Default::default()
    };

    let cancelled_requests = Arc::new(Mutex::new(HashSet::new()));
    let request_id = RequestId::from(1);

    let results = pylight::lsp::handlers::handle_workspace_symbol(
        params,
        index,
        search_engine,
        cancelled_requests,
        request_id,
    )
    .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|s| s.name == "test_function"));
    assert!(results.iter().any(|s| s.name == "TestClass"));
}

#[test]
fn test_empty_query_returns_symbols() {
    use pylight::{SearchEngine, Symbol, SymbolKind};
    use std::sync::Arc;

    let index = Arc::new(SymbolIndex::default());
    let search_engine = Arc::new(SearchEngine::new());

    // Add test symbols
    let symbols = vec![
        Symbol::new(
            "function1".to_string(),
            SymbolKind::Function,
            PathBuf::from("/test/file.py"),
            10,
            0,
        ),
        Symbol::new(
            "function2".to_string(),
            SymbolKind::Function,
            PathBuf::from("/test/file.py"),
            20,
            0,
        ),
    ];

    index
        .add_file(PathBuf::from("/test/file.py"), symbols)
        .unwrap();

    let params = lsp_types::WorkspaceSymbolParams {
        query: "".to_string(),
        ..Default::default()
    };

    let cancelled_requests = Arc::new(Mutex::new(HashSet::new()));
    let request_id = RequestId::from(2);

    let results = pylight::lsp::handlers::handle_workspace_symbol(
        params,
        index,
        search_engine,
        cancelled_requests,
        request_id,
    )
    .unwrap();
    // Empty query should return symbols (up to 100)
    assert_eq!(results.len(), 2);
}
