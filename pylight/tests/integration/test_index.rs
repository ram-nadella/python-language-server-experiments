use pylight::{Symbol, SymbolIndex, SymbolKind};
use std::path::PathBuf;

#[test]
fn test_index_add_file() {
    let index = SymbolIndex::default();
    let path = PathBuf::from("test.py");

    let symbols = vec![
        Symbol::new(
            "func1".to_string(),
            SymbolKind::Function,
            path.clone(),
            1,
            0,
        ),
        Symbol::new("Class1".to_string(), SymbolKind::Class, path.clone(), 10, 0),
    ];

    index.add_file(path.clone(), symbols.clone()).unwrap();

    let all_symbols = index.get_all_symbols();
    assert_eq!(all_symbols.len(), 2);

    let file_symbols = index.get_file_symbols(&path).unwrap();
    assert_eq!(file_symbols.len(), 2);
}

#[test]
fn test_index_update_file() {
    let index = SymbolIndex::default();
    let path = PathBuf::from("test.py");

    // Add initial symbols
    let symbols_v1 = vec![Symbol::new(
        "old_func".to_string(),
        SymbolKind::Function,
        path.clone(),
        1,
        0,
    )];
    index.add_file(path.clone(), symbols_v1).unwrap();

    // Update with new symbols
    let symbols_v2 = vec![
        Symbol::new(
            "new_func".to_string(),
            SymbolKind::Function,
            path.clone(),
            1,
            0,
        ),
        Symbol::new(
            "NewClass".to_string(),
            SymbolKind::Class,
            path.clone(),
            10,
            0,
        ),
    ];
    index.add_file(path.clone(), symbols_v2).unwrap();

    let all_symbols = index.get_all_symbols();
    assert_eq!(all_symbols.len(), 2);

    // Old symbol should be gone
    assert!(!all_symbols.iter().any(|s| s.name == "old_func"));

    // New symbols should be present
    assert!(all_symbols.iter().any(|s| s.name == "new_func"));
    assert!(all_symbols.iter().any(|s| s.name == "NewClass"));
}

#[test]
fn test_index_remove_file() {
    let index = SymbolIndex::default();
    let path1 = PathBuf::from("file1.py");
    let path2 = PathBuf::from("file2.py");

    let symbols1 = vec![Symbol::new(
        "func1".to_string(),
        SymbolKind::Function,
        path1.clone(),
        1,
        0,
    )];
    let symbols2 = vec![Symbol::new(
        "func2".to_string(),
        SymbolKind::Function,
        path2.clone(),
        1,
        0,
    )];

    index.add_file(path1.clone(), symbols1).unwrap();
    index.add_file(path2.clone(), symbols2).unwrap();

    assert_eq!(index.get_all_symbols().len(), 2);

    index.remove_file(&path1).unwrap();

    let all_symbols = index.get_all_symbols();
    assert_eq!(all_symbols.len(), 1);
    assert_eq!(all_symbols[0].name, "func2");

    assert!(index.get_file_symbols(&path1).is_none());
    assert!(index.get_file_symbols(&path2).is_some());
}

#[test]
fn test_index_clear() {
    let index = SymbolIndex::default();
    let path = PathBuf::from("test.py");

    let symbols = vec![Symbol::new(
        "func1".to_string(),
        SymbolKind::Function,
        path.clone(),
        1,
        0,
    )];

    index.add_file(path, symbols).unwrap();
    assert_eq!(index.get_all_symbols().len(), 1);

    index.clear();
    assert_eq!(index.get_all_symbols().len(), 0);
}
