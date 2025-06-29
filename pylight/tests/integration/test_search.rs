use pylight::{SearchEngine, Symbol, SymbolKind};
use std::path::PathBuf;
use std::sync::Arc;

fn create_test_symbols() -> Vec<Arc<Symbol>> {
    vec![
        Arc::new(Symbol::new(
            "test_function".to_string(),
            SymbolKind::Function,
            PathBuf::from("test.py"),
            1,
            0,
        )),
        Arc::new(Symbol::new(
            "TestClass".to_string(),
            SymbolKind::Class,
            PathBuf::from("test.py"),
            10,
            0,
        )),
        Arc::new(Symbol::new(
            "another_test_func".to_string(),
            SymbolKind::Function,
            PathBuf::from("test.py"),
            20,
            0,
        )),
        Arc::new(Symbol::new(
            "helper_function".to_string(),
            SymbolKind::Function,
            PathBuf::from("helper.py"),
            5,
            0,
        )),
        Arc::new(Symbol::new(
            "HelperClass".to_string(),
            SymbolKind::Class,
            PathBuf::from("helper.py"),
            15,
            0,
        )),
        Arc::new(
            Symbol::new(
                "test_method".to_string(),
                SymbolKind::Method,
                PathBuf::from("test.py"),
                12,
                4,
            )
            .with_container("TestClass".to_string()),
        ),
    ]
}

#[test]
fn test_exact_match() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("test_function", &symbols);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbol.name, "test_function");
}

#[test]
fn test_fuzzy_match() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("test", &symbols);
    assert!(results.len() >= 3);

    // Should match: test_function, TestClass, another_test_func, test_method
    let matched_names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    assert!(matched_names.contains(&"test_function"));
    assert!(matched_names.contains(&"TestClass"));
    assert!(matched_names.contains(&"test_method"));
}

#[test]
fn test_exact_match_ranks_first() {
    let engine = SearchEngine::new();

    // Create symbols where "test" appears in different contexts
    let symbols = vec![
        Arc::new(Symbol::new(
            "test".to_string(), // Exact match
            SymbolKind::Function,
            PathBuf::from("exact.py"),
            1,
            0,
        )),
        Arc::new(Symbol::new(
            "test_something".to_string(), // Prefix match
            SymbolKind::Function,
            PathBuf::from("prefix.py"),
            1,
            0,
        )),
        Arc::new(Symbol::new(
            "another_test".to_string(), // Suffix match
            SymbolKind::Function,
            PathBuf::from("suffix.py"),
            1,
            0,
        )),
        Arc::new(Symbol::new(
            "TestClass".to_string(), // Different case
            SymbolKind::Class,
            PathBuf::from("class.py"),
            1,
            0,
        )),
    ];

    // Search for "test"
    let results = engine.search("test", &symbols);

    // The exact match "test" should be first
    assert!(!results.is_empty());
    assert_eq!(results[0].symbol.name, "test");

    // Exact match should have a higher score than non-exact matches
    // We multiply by 10, so it should be significantly higher
    for result in results.iter().skip(1) {
        assert!(
            results[0].score > result.score,
            "Exact match score {} should be higher than {} for symbol '{}'",
            results[0].score,
            result.score,
            result.symbol.name
        );
    }
}

#[test]
fn test_case_insensitive_search() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("testclass", &symbols);
    assert!(results.iter().any(|r| r.symbol.name == "TestClass"));
}

#[test]
fn test_partial_match() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("help", &symbols);
    assert!(results.iter().any(|r| r.symbol.name == "helper_function"));
    assert!(results.iter().any(|r| r.symbol.name == "HelperClass"));
}

#[test]
fn test_empty_query() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("", &symbols);
    // Empty query now returns first 100 symbols (or all if less than 100)
    assert_eq!(results.len(), symbols.len());
    // All results should have score of 0
    assert!(results.iter().all(|r| r.score == 0));
}

#[test]
fn test_no_matches() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("xyz123", &symbols);
    assert_eq!(results.len(), 0);
}

#[test]
fn test_search_result_ordering() {
    let engine = SearchEngine::new();
    let symbols = create_test_symbols();

    let results = engine.search("test", &symbols);

    // Exact matches should score higher
    // Verify results are sorted by score (descending)
    for i in 1..results.len() {
        assert!(
            results[i - 1].score >= results[i].score,
            "Results not properly sorted by score"
        );
    }
}

#[test]
fn test_case_insensitive_exact_match() {
    let engine = SearchEngine::new();

    let symbols = vec![
        Arc::new(Symbol::new(
            "TestFunction".to_string(),
            SymbolKind::Function,
            PathBuf::from("test.py"),
            1,
            0,
        )),
        Arc::new(Symbol::new(
            "test_helper".to_string(),
            SymbolKind::Function,
            PathBuf::from("test.py"),
            10,
            0,
        )),
    ];

    // Search with different case variations
    let results = engine.search("testfunction", &symbols);
    assert!(
        !results.is_empty(),
        "Should find TestFunction with lowercase query"
    );
    assert_eq!(results[0].symbol.name, "TestFunction");
    // Verify it's boosted (exact match gets 10x multiplier)
    assert!(results[0].score > 100); // Base fuzzy score would be around 100-300

    let results = engine.search("TestFunction", &symbols);
    assert!(
        !results.is_empty(),
        "Should find TestFunction with exact case"
    );
    assert_eq!(results[0].symbol.name, "TestFunction");
    // Verify it's boosted (exact match gets 10x multiplier)
    assert!(results[0].score > 100);
}
