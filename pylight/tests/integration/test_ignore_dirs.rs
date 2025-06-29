//! Integration tests for directory ignoring functionality

use pylight::index::updater::IndexUpdater;
use pylight::watcher::{FileWatcher, WatcherConfig};
use pylight::SymbolIndex;
use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_ignored_directories_not_indexed() {
    // Create a temporary directory structure
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().to_path_buf();

    // Create various directories and files
    let test_files = vec![
        ("src/main.py", "def main():\n    pass\n"),
        ("src/utils.py", "def helper():\n    pass\n"),
        ("tests/test_main.py", "def test_main():\n    pass\n"),
        (".git/config", "[core]\n    repositoryformatversion = 0\n"),
        (
            ".git/hooks/pre-commit.py",
            "#!/usr/bin/env python\nprint('pre-commit')\n",
        ),
        ("__pycache__/main.cpython-39.pyc", "binary content"),
        (
            ".venv/lib/python3.9/site-packages/requests.py",
            "def get():\n    pass\n",
        ),
        (
            "node_modules/package/index.py",
            "def node_func():\n    pass\n",
        ),
        ("build/lib/module.py", "def build_func():\n    pass\n"),
        ("myproject.egg-info/PKG-INFO", "Metadata-Version: 2.1\n"),
    ];

    // Create all files
    for (path, content) in &test_files {
        let file_path = workspace_path.join(path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, content).unwrap();
    }

    // Create and index the workspace
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();

    // Check that only non-ignored Python files were indexed
    let all_symbols = index.get_all_symbols();
    let symbol_names: Vec<&str> = all_symbols.iter().map(|s| s.name.as_str()).collect();

    // Should include symbols from normal directories
    assert!(symbol_names.contains(&"main"));
    assert!(symbol_names.contains(&"helper"));
    assert!(symbol_names.contains(&"test_main"));

    // Should NOT include symbols from ignored directories
    assert!(!symbol_names.contains(&"get")); // from .venv
    assert!(!symbol_names.contains(&"node_func")); // from node_modules
    assert!(!symbol_names.contains(&"build_func")); // from build

    // Verify file count - should only be 3 (src/main.py, src/utils.py, tests/test_main.py)
    assert_eq!(index.get_file_count(), 3);
}

#[test]
fn test_file_watcher_ignores_directories() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().canonicalize().unwrap();

    // Create initial files
    let src_path = workspace_path.join("src");
    fs::create_dir_all(&src_path).unwrap();
    fs::write(src_path.join("main.py"), "def main():\n    pass\n").unwrap();

    // Create ignored directories
    let git_path = workspace_path.join(".git");
    fs::create_dir_all(&git_path).unwrap();

    let venv_path = workspace_path.join(".venv");
    fs::create_dir_all(&venv_path).unwrap();

    // Create index and initial indexing
    let index = Arc::new(SymbolIndex::default());
    index.clone().index_workspace(&workspace_path).unwrap();
    assert_eq!(index.get_file_count(), 1);

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

    // Add a file to an ignored directory
    fs::write(git_path.join("test.py"), "def git_func():\n    pass\n").unwrap();
    fs::write(venv_path.join("test.py"), "def venv_func():\n    pass\n").unwrap();

    // Add a file to a normal directory
    fs::write(src_path.join("utils.py"), "def utils_func():\n    pass\n").unwrap();

    // Wait for debounce and processing (longer wait for thread pool)
    thread::sleep(Duration::from_millis(1000));

    // Check that only the non-ignored file was indexed
    let all_symbols = index.get_all_symbols();
    let symbol_names: Vec<&str> = all_symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(symbol_names.contains(&"main"));
    assert!(symbol_names.contains(&"utils_func"));
    assert!(!symbol_names.contains(&"git_func"));
    assert!(!symbol_names.contains(&"venv_func"));

    // File count should be 2 (main.py and utils.py)
    assert_eq!(index.get_file_count(), 2);
}
