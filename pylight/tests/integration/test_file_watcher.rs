//! Integration tests for file watching functionality

use pylight::index::updater::IndexUpdater;
use pylight::watcher::{FileWatcher, WatcherConfig};
use pylight::SymbolIndex;
use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_file_watcher_single_file_change() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create an initial Python file
    let file_path = workspace_path.join("test.py");
    fs::write(&file_path, "def hello():\n    pass\n").unwrap();

    // Create index and updater
    let index = Arc::new(SymbolIndex::default());

    // Do initial indexing
    index.clone().index_workspace(&workspace_path).unwrap();
    assert_eq!(index.get_file_count(), 1);
    let initial_symbols = index.get_all_symbols();
    println!(
        "Initial symbols: {:#?}",
        initial_symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert_eq!(initial_symbols.len(), 1);

    // Create updater and file watcher
    let updater = Arc::new(IndexUpdater::new(index.clone(), workspace_path.clone()));
    let config = WatcherConfig {
        debounce_ms: 50, // Shorter for tests
        timeout_ms: 5000,
        batch_threshold: 10,
    };

    let mut watcher = FileWatcher::new(config, updater).unwrap();
    watcher.watch(&workspace_path).unwrap();

    // Give the watcher time to start
    thread::sleep(Duration::from_millis(100));

    // Modify the file
    fs::write(
        &file_path,
        "def hello():\n    pass\n\ndef world():\n    pass\n",
    )
    .unwrap();

    // Wait for the debounce period and processing
    thread::sleep(Duration::from_millis(500));

    // Check that the index was updated
    let file_count = index.get_file_count();
    println!("File count after update: {file_count}");
    assert_eq!(file_count, 1);

    let symbols = index.get_all_symbols();
    let symbol_names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    println!("Symbols after update: {symbol_names:?}");
    assert!(symbol_names.contains(&"hello"));
    assert!(symbol_names.contains(&"world"));
    assert_eq!(symbols.len(), 2);
}

#[test]
fn test_file_watcher_bulk_changes() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create multiple Python files
    for i in 0..15 {
        let file_path = workspace_path.join(format!("test{i}.py"));
        fs::write(&file_path, format!("def func{i}():\n    pass\n")).unwrap();
    }

    // Create index and do initial indexing
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();
    assert_eq!(index.get_file_count(), 15);
    assert_eq!(index.get_all_symbols().len(), 15);

    // Create updater and file watcher
    let updater = Arc::new(IndexUpdater::new(index.clone(), workspace_path.clone()));
    let config = WatcherConfig {
        debounce_ms: 50,
        timeout_ms: 5000,
        batch_threshold: 10,
    };

    let mut watcher = FileWatcher::new(config, updater).unwrap();
    watcher.watch(&workspace_path).unwrap();

    // Give the watcher time to start
    thread::sleep(Duration::from_millis(100));

    // Modify multiple files at once (simulating a git operation)
    for i in 0..12 {
        let file_path = workspace_path.join(format!("test{i}.py"));
        fs::write(
            &file_path,
            format!("def func{i}():\n    pass\n\ndef extra{i}():\n    pass\n"),
        )
        .unwrap();
    }

    // Wait for the debounce period and processing
    thread::sleep(Duration::from_millis(500));

    // Check that the index was updated with bulk change
    assert_eq!(index.get_file_count(), 15);
    assert_eq!(index.get_all_symbols().len(), 27); // 12 files * 2 functions + 3 unchanged files
}

#[test]
fn test_file_watcher_file_removal() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create Python files
    let file1 = workspace_path.join("file1.py");
    let file2 = workspace_path.join("file2.py");
    fs::write(&file1, "def func1():\n    pass\n").unwrap();
    fs::write(&file2, "def func2():\n    pass\n").unwrap();

    // Create index and do initial indexing
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();
    assert_eq!(index.get_file_count(), 2);
    assert_eq!(index.get_all_symbols().len(), 2);

    // Create updater and file watcher
    let updater = Arc::new(IndexUpdater::new(index.clone(), workspace_path.clone()));
    let config = WatcherConfig {
        debounce_ms: 50,
        timeout_ms: 5000,
        batch_threshold: 10,
    };

    let mut watcher = FileWatcher::new(config, updater).unwrap();
    watcher.watch(&workspace_path).unwrap();

    // Give the watcher time to start
    thread::sleep(Duration::from_millis(100));

    // Remove one file
    fs::remove_file(&file1).unwrap();

    // Wait for the debounce period and processing
    thread::sleep(Duration::from_millis(200));

    // Check that the file was removed from the index
    assert_eq!(index.get_file_count(), 1);
    assert_eq!(index.get_all_symbols().len(), 1);
    assert!(index.has_file(&file2));
    assert!(!index.has_file(&file1));
}

#[test]
fn test_file_watcher_rapid_changes() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create a Python file
    let file_path = workspace_path.join("test.py");
    fs::write(&file_path, "def initial():\n    pass\n").unwrap();

    // Create index and do initial indexing
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();

    // Create updater and file watcher with longer debounce
    let updater = Arc::new(IndexUpdater::new(index.clone(), workspace_path.clone()));
    let config = WatcherConfig {
        debounce_ms: 200, // Longer debounce for this test
        timeout_ms: 5000,
        batch_threshold: 10,
    };

    let mut watcher = FileWatcher::new(config, updater).unwrap();
    watcher.watch(&workspace_path).unwrap();

    // Give the watcher time to start
    thread::sleep(Duration::from_millis(100));

    // Make rapid changes
    for i in 0..5 {
        fs::write(&file_path, format!("def func{i}():\n    pass\n")).unwrap();
        thread::sleep(Duration::from_millis(50)); // Less than debounce period
    }

    // Wait for the debounce period to expire
    thread::sleep(Duration::from_millis(300));

    // Should only see the final state
    let symbols = index.get_all_symbols();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "func4");
}

#[test]
fn test_file_watcher_non_python_files_ignored() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create various files
    let py_file = workspace_path.join("test.py");
    let txt_file = workspace_path.join("readme.txt");
    let js_file = workspace_path.join("script.js");

    fs::write(&py_file, "def python_func():\n    pass\n").unwrap();
    fs::write(&txt_file, "This is a text file").unwrap();
    fs::write(&js_file, "function jsFunc() {}").unwrap();

    // Create index and do initial indexing
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();
    assert_eq!(index.get_file_count(), 1); // Only Python file

    // Create updater and file watcher
    let updater = Arc::new(IndexUpdater::new(index.clone(), workspace_path.clone()));
    let config = WatcherConfig {
        debounce_ms: 50,
        timeout_ms: 5000,
        batch_threshold: 10,
    };

    let mut watcher = FileWatcher::new(config, updater).unwrap();
    watcher.watch(&workspace_path).unwrap();

    // Give the watcher time to start
    thread::sleep(Duration::from_millis(100));

    // Modify non-Python files
    fs::write(&txt_file, "Updated text").unwrap();
    fs::write(&js_file, "function updated() {}").unwrap();

    // Wait for debounce
    thread::sleep(Duration::from_millis(200));

    // Index should remain unchanged
    assert_eq!(index.get_file_count(), 1);
    assert_eq!(index.get_all_symbols().len(), 1);

    // Now modify the Python file
    fs::write(
        &py_file,
        "def python_func():\n    pass\n\ndef new_func():\n    pass\n",
    )
    .unwrap();

    // Wait for debounce
    thread::sleep(Duration::from_millis(200));

    // Now the index should be updated
    assert_eq!(index.get_file_count(), 1);
    assert_eq!(index.get_all_symbols().len(), 2);
}
