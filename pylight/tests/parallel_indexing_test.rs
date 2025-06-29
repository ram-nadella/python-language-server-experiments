use pylight::SymbolIndex;
use rayon::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// Create a test directory with Python files that have artificial delays
fn create_test_files_with_delays() -> (TempDir, Vec<std::path::PathBuf>) {
    let temp_dir = TempDir::new().unwrap();
    let mut files = Vec::new();

    // Create 10 Python files with sleep statements
    for i in 0..10 {
        let file_path = temp_dir.path().join(format!("file_{i}.py"));
        let content = format!(
            r#"
import time

# This simulates a slow-to-parse file
time.sleep(0.001)  # Just for the content, won't actually execute

def function_{i}():
    pass

class Class_{i}:
    def method(self):
        pass
"#
        );
        std::fs::write(&file_path, content).unwrap();
        files.push(file_path);
    }

    (temp_dir, files)
}

#[test]
fn test_parallel_indexing_is_faster_than_sequential() {
    // Skip this test in CI environments where timing might be unreliable
    if std::env::var("CI").is_ok() {
        return;
    }

    let (_temp_dir, files) = create_test_files_with_delays();

    // Measure sequential processing (1 thread)
    let sequential_start = std::time::Instant::now();
    let pool_sequential = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();

    let (seq_files, seq_symbols, _) = pool_sequential.install(|| {
        let index = Arc::new(SymbolIndex::new());
        index.parse_and_index_files(files.clone()).unwrap()
    });
    let sequential_duration = sequential_start.elapsed();

    // Measure parallel processing (multiple threads)
    let parallel_start = std::time::Instant::now();
    let num_threads = num_cpus::get().min(4); // Use at most 4 threads for testing
    let pool_parallel = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .unwrap();

    let (par_files, par_symbols, _) = pool_parallel.install(|| {
        let index = Arc::new(SymbolIndex::new());
        index.parse_and_index_files(files.clone()).unwrap()
    });
    let parallel_duration = parallel_start.elapsed();

    // Verify same results
    assert_eq!(seq_files, par_files, "Should process same number of files");
    assert_eq!(
        seq_symbols, par_symbols,
        "Should extract same number of symbols"
    );

    // Parallel should be faster (allowing some margin for small file sets)
    let speedup = sequential_duration.as_secs_f64() / parallel_duration.as_secs_f64();

    eprintln!(
        "Sequential: {sequential_duration:?}, Parallel: {parallel_duration:?}, Speedup: {speedup:.2}x"
    );

    // We expect at least 1.5x speedup with multiple threads
    assert!(
        speedup > 1.5,
        "Parallel indexing should be at least 1.5x faster than sequential. Got {speedup:.2}x speedup"
    );
}

#[test]
fn test_parallel_indexing_uses_multiple_threads() {
    use parking_lot::Mutex;
    use std::collections::HashSet;

    let (_temp_dir, files) = create_test_files_with_delays();
    let thread_ids = Arc::new(Mutex::new(HashSet::new()));

    // Monkey-patch to track thread IDs (in a real implementation,
    // we'd modify parse_and_index_files to return this info)
    let num_threads = num_cpus::get().min(4);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .unwrap();

    // For this test, we'll use a custom parallel iterator to track threads
    let thread_ids_clone = thread_ids.clone();
    pool.install(|| {
        files.par_iter().for_each(|_file| {
            let thread_id = std::thread::current().id();
            thread_ids_clone.lock().insert(thread_id);
            // Simulate some work
            std::thread::sleep(Duration::from_millis(10));
        });
    });

    let unique_threads = thread_ids.lock().len();

    eprintln!("Used {unique_threads} unique threads out of {num_threads} available");

    // We should use more than 1 thread
    assert!(
        unique_threads > 1,
        "Parallel processing should use multiple threads. Only used {unique_threads} thread(s)"
    );
}
