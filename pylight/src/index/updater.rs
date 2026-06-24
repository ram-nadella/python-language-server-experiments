//! Index update coordinator that handles file change events

use crate::parser::create_parser;
use crate::watcher::{FileEvent, FileEventHandler};
use crate::{Result, SymbolIndex};
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// State of the index updater
#[derive(Debug, Clone, Copy, PartialEq)]
enum UpdaterState {
    /// Idle, ready to process events
    Idle,
    /// Currently processing a full re-index
    ReIndexing,
}

/// Manages index updates from file system events
pub struct IndexUpdater {
    index: Arc<SymbolIndex>,
    state: Arc<RwLock<UpdaterState>>,
    workspace_roots: Vec<PathBuf>,
    ignore_filters: Vec<Arc<crate::file_filter::IgnoreFilter>>,
}

impl IndexUpdater {
    pub fn new(index: Arc<SymbolIndex>, workspace_root: PathBuf) -> Self {
        Self::new_multi(index, vec![workspace_root])
    }

    pub fn new_multi(index: Arc<SymbolIndex>, workspace_roots: Vec<PathBuf>) -> Self {
        let ignore_filters = workspace_roots
            .iter()
            .cloned()
            .map(crate::file_filter::IgnoreFilter::new)
            .map(Arc::new)
            .collect();
        Self {
            index,
            state: Arc::new(RwLock::new(UpdaterState::Idle)),
            workspace_roots,
            ignore_filters,
        }
    }

    fn should_ignore(&self, path: &Path) -> bool {
        self.workspace_roots
            .iter()
            .zip(&self.ignore_filters)
            .find(|(root, _)| path.starts_with(root))
            .map(|(_, filter)| filter.should_ignore(path))
            .unwrap_or(false)
    }

    /// Process a single file update
    fn process_file_update(&self, path: &Path) -> Result<()> {
        // Check if the file should be ignored
        if self.should_ignore(path) {
            debug!("Ignoring file update for: {}", path.display());
            return Ok(());
        }

        let start = Instant::now();

        // Create a parser for this file using the index's backend
        let parser = create_parser(self.index.parser_backend())?;

        // Read and parse the file
        match std::fs::read_to_string(path) {
            Ok(content) => match parser.parse_file(path, &content) {
                Ok(symbols) => {
                    self.index.add_file(path.to_path_buf(), symbols)?;
                    info!(
                        "Updated file {} in {:.2}ms",
                        path.display(),
                        start.elapsed().as_secs_f64() * 1000.0
                    );
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to parse {}: {}", path.display(), e);
                    Ok(())
                }
            },
            Err(e) => {
                // Only warn for errors other than "file not found" since that's expected
                // during rapid file system changes (e.g., git operations)
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!("Failed to read {}: {}", path.display(), e);
                } else {
                    debug!("File no longer exists: {}", path.display());
                }
                Ok(())
            }
        }
    }

    /// Perform a full re-index of the workspace
    fn perform_full_reindex(&self) -> Result<()> {
        info!("Starting full workspace re-index");
        let start = Instant::now();

        // Create a new temporary index with the same parser backend
        let new_index = Arc::new(SymbolIndex::new(self.index.parser_backend()));

        // Index each workspace folder into the new index
        for root in &self.workspace_roots {
            new_index.clone().index_workspace(root)?;
        }

        // Atomically swap the indices
        self.index.swap_index(&new_index);

        info!(
            "Full re-index completed in {:.2}s",
            start.elapsed().as_secs_f64()
        );

        Ok(())
    }

    /// Internal event handler
    fn handle_event_internal(&self, event: FileEvent) {
        match event {
            FileEvent::FileChanged(path) => {
                // Process the file change
                if let Err(e) = self.process_file_update(&path) {
                    error!("Failed to update file {}: {}", path.display(), e);
                }
            }
            FileEvent::BulkChange(_) => {
                // For bulk changes, always do a full re-index
                // This is simpler and avoids complex state management
                info!("Bulk change detected, performing full re-index");

                // Check if we're already re-indexing
                let state = self.state.read();
                if *state == UpdaterState::ReIndexing {
                    info!("Already re-indexing, skipping bulk change event");
                    return;
                }
                drop(state);

                // Set state to re-indexing
                *self.state.write() = UpdaterState::ReIndexing;

                if let Err(e) = self.perform_full_reindex() {
                    error!("Failed to perform full re-index: {}", e);
                }

                *self.state.write() = UpdaterState::Idle;
            }
            FileEvent::FileRemoved(path) => {
                if let Err(e) = self.index.remove_file(&path) {
                    error!("Failed to remove file {}: {}", path.display(), e);
                }
            }
        }
    }
}

impl FileEventHandler for IndexUpdater {
    fn handle_event(&self, event: FileEvent) {
        // For bulk changes, handle directly in current thread to avoid race conditions
        // For individual file changes, use rayon thread pool to avoid blocking the watcher
        match &event {
            FileEvent::BulkChange(_) => {
                // Handle bulk changes synchronously to ensure proper state management
                self.handle_event_internal(event);
            }
            _ => {
                // Clone what we need for the async task
                let index = self.index.clone();
                let state = self.state.clone();
                let workspace_roots = self.workspace_roots.clone();
                let ignore_filters = self.ignore_filters.clone();

                // Use rayon's thread pool instead of spawning new threads
                rayon::spawn(move || {
                    let updater = IndexUpdater {
                        index,
                        state,
                        workspace_roots,
                        ignore_filters,
                    };
                    updater.handle_event_internal(event);
                });
            }
        }
    }

    fn should_watch(&self, path: &Path) -> bool {
        // Only watch Python files that are not ignored
        let is_python = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s == "py")
            .unwrap_or(false);

        if !is_python {
            return false;
        }

        // Check if the file should be ignored
        let should_ignore = self.should_ignore(path);

        if should_ignore {
            debug!("Ignoring watch for: {}", path.display());
        }

        !should_ignore
    }

    fn workspace_root(&self) -> &Path {
        self.workspace_roots
            .first()
            .map(PathBuf::as_path)
            .unwrap_or_else(|| Path::new("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_updater_creation() {
        let temp_dir = TempDir::new().unwrap();
        let index = Arc::new(SymbolIndex::default());
        let updater = IndexUpdater::new(index, temp_dir.path().to_path_buf());

        assert_eq!(*updater.state.read(), UpdaterState::Idle);
    }

    #[test]
    fn test_should_watch_python_files() {
        let temp_dir = TempDir::new().unwrap();
        let index = Arc::new(SymbolIndex::default());
        let updater = IndexUpdater::new(index, temp_dir.path().to_path_buf());

        assert!(updater.should_watch(Path::new("test.py")));
        assert!(updater.should_watch(Path::new("path/to/file.py")));
        assert!(!updater.should_watch(Path::new("test.txt")));
        assert!(!updater.should_watch(Path::new("test.js")));
        assert!(!updater.should_watch(Path::new("test")));
    }
}
