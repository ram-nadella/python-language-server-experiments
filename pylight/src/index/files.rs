//! File operations for the Python Language Server

use crate::file_filter::IgnoreFilter;
use ignore::WalkBuilder;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

/// Collect all Python files in a directory
pub fn collect_python_files(root: &PathBuf) -> Vec<PathBuf> {
    let ignore_filter = Arc::new(IgnoreFilter::new(root.clone()));
    let files = Arc::new(Mutex::new(Vec::new()));

    // Use the ignore crate's WalkBuilder which respects .gitignore
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(false) // We'll use our own filter
        .follow_links(false)
        .threads(num_cpus::get().saturating_sub(1).max(1)); // Use all CPUs minus 1

    builder.build_parallel().run(|| {
        let ignore_filter = Arc::clone(&ignore_filter);
        let files = Arc::clone(&files);

        Box::new(move |entry| {
            if let Ok(entry) = entry {
                let path = entry.path();
                // Must be a Python file and not ignored
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                    && path.extension().and_then(|s| s.to_str()) == Some("py")
                    && !ignore_filter.should_ignore(path)
                {
                    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                    files.lock().push(canonical_path);
                }
            }
            ignore::WalkState::Continue
        })
    });

    Arc::try_unwrap(files)
        .map(|mutex| mutex.into_inner())
        .unwrap_or_else(|arc| arc.lock().clone())
}
