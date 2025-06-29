//! Symbol index implementation

use crate::{PythonParser, Result, Symbol};
use parking_lot::RwLock;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::files;

pub struct SymbolIndex {
    symbols: Arc<RwLock<HashMap<PathBuf, Vec<Arc<Symbol>>>>>,
    all_symbols: Arc<RwLock<Vec<Arc<Symbol>>>>,
    file_metadata: Arc<RwLock<HashMap<PathBuf, FileMetadata>>>,
}

#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub last_modified: std::time::SystemTime,
    pub symbol_count: usize,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self {
            symbols: Arc::new(RwLock::new(HashMap::new())),
            all_symbols: Arc::new(RwLock::new(Vec::new())),
            file_metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn add_file(&self, path: PathBuf, symbols: Vec<Symbol>) -> Result<()> {
        // Canonicalize the path for consistent comparison
        let canonical_path = path.canonicalize().unwrap_or(path.clone());

        let mut file_symbols = self.symbols.write();
        let mut all = self.all_symbols.write();
        let mut metadata = self.file_metadata.write();

        // Remove old symbols for this file if any
        if let Some(_old_symbols) = file_symbols.get(&canonical_path) {
            all.retain(|s| s.file_path != canonical_path);
        }

        // Update metadata
        if let Ok(file_metadata) = std::fs::metadata(&canonical_path) {
            if let Ok(modified) = file_metadata.modified() {
                metadata.insert(
                    canonical_path.clone(),
                    FileMetadata {
                        last_modified: modified,
                        symbol_count: symbols.len(),
                    },
                );
            }
        }

        // Convert symbols to Arc
        let arc_symbols: Vec<Arc<Symbol>> = symbols.into_iter().map(Arc::new).collect();

        // Add new symbols
        all.extend(arc_symbols.clone());
        file_symbols.insert(canonical_path, arc_symbols);

        Ok(())
    }

    pub fn remove_file(&self, path: &Path) -> Result<()> {
        // Canonicalize the path for consistent comparison
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        let mut file_symbols = self.symbols.write();
        let mut all = self.all_symbols.write();
        let mut metadata = self.file_metadata.write();

        file_symbols.remove(&canonical_path);
        all.retain(|s| s.file_path != canonical_path);
        metadata.remove(&canonical_path);

        Ok(())
    }

    pub fn get_all_symbols(&self) -> Vec<Arc<Symbol>> {
        self.all_symbols.read().clone()
    }

    /// Get a reference to all symbols without cloning for read-only operations
    pub fn with_all_symbols<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&[Arc<Symbol>]) -> R,
    {
        let symbols = self.all_symbols.read();
        f(&symbols)
    }

    pub fn get_file_symbols(&self, path: &Path) -> Option<Vec<Arc<Symbol>>> {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.symbols.read().get(&canonical_path).cloned()
    }

    pub fn clear(&self) {
        self.symbols.write().clear();
        self.all_symbols.write().clear();
        self.file_metadata.write().clear();
    }

    /// Get the total number of indexed files
    pub fn get_file_count(&self) -> usize {
        self.symbols.read().len()
    }

    /// Check if a file is already indexed
    pub fn has_file(&self, path: &Path) -> bool {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.symbols.read().contains_key(&canonical_path)
    }

    /// Get metadata for a file
    pub fn get_file_metadata(&self, path: &Path) -> Option<FileMetadata> {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.file_metadata.read().get(&canonical_path).cloned()
    }

    /// Update specific files without full re-index
    pub fn update_files_batch(
        &self,
        updates: Vec<(PathBuf, Vec<Symbol>)>,
    ) -> Result<(usize, usize)> {
        let mut file_symbols = self.symbols.write();
        let mut all = self.all_symbols.write();
        let mut metadata = self.file_metadata.write();

        let mut updated_files = 0;
        let mut total_symbols = 0;

        for (path, symbols) in updates {
            // Remove old symbols for this file if any
            if file_symbols.contains_key(&path) {
                all.retain(|s| s.file_path != path);
            }

            // Update metadata
            if let Ok(file_metadata) = std::fs::metadata(&path) {
                if let Ok(modified) = file_metadata.modified() {
                    metadata.insert(
                        path.clone(),
                        FileMetadata {
                            last_modified: modified,
                            symbol_count: symbols.len(),
                        },
                    );
                }
            }

            // Convert symbols to Arc
            let arc_symbols: Vec<Arc<Symbol>> = symbols.into_iter().map(Arc::new).collect();
            total_symbols += arc_symbols.len();

            // Add new symbols
            all.extend(arc_symbols.clone());
            file_symbols.insert(path, arc_symbols);
            updated_files += 1;
        }

        Ok((updated_files, total_symbols))
    }

    /// Create a new index from scratch and swap it atomically
    pub fn swap_index(&self, new_index: &SymbolIndex) {
        // Acquire all locks in a consistent order to avoid deadlocks
        let mut symbols = self.symbols.write();
        let mut all_symbols = self.all_symbols.write();
        let mut metadata = self.file_metadata.write();

        let new_symbols = new_index.symbols.read();
        let new_all = new_index.all_symbols.read();
        let new_metadata = new_index.file_metadata.read();

        // Swap the contents
        *symbols = new_symbols.clone();
        *all_symbols = new_all.clone();
        *metadata = new_metadata.clone();
    }

    /// Add multiple files in a single batch operation to minimize lock contention
    pub fn add_files_batch(&self, files: Vec<(PathBuf, Vec<Symbol>)>) -> Result<()> {
        let mut file_symbols = self.symbols.write();
        let mut all = self.all_symbols.write();
        let mut metadata = self.file_metadata.write();

        for (path, symbols) in files {
            // Remove old symbols for this file if any
            if file_symbols.contains_key(&path) {
                all.retain(|s| s.file_path != path);
            }

            // Update metadata
            if let Ok(file_metadata) = std::fs::metadata(&path) {
                if let Ok(modified) = file_metadata.modified() {
                    metadata.insert(
                        path.clone(),
                        FileMetadata {
                            last_modified: modified,
                            symbol_count: symbols.len(),
                        },
                    );
                }
            }

            // Convert symbols to Arc
            let arc_symbols: Vec<Arc<Symbol>> = symbols.into_iter().map(Arc::new).collect();

            // Add new symbols
            all.extend(arc_symbols.clone());
            file_symbols.insert(path, arc_symbols);
        }

        Ok(())
    }

    /// Parse and index a list of Python files in parallel
    /// Returns (number of files parsed, total symbols, elapsed time)
    pub fn parse_and_index_files(
        self: Arc<Self>,
        python_files: Vec<PathBuf>,
    ) -> Result<(usize, usize, std::time::Duration)> {
        let start_time = std::time::Instant::now();

        // Process files in parallel and collect all results
        let all_file_symbols: Vec<(PathBuf, Vec<Symbol>)> = python_files
            .par_iter()
            .filter_map(|path| {
                let thread_id = std::thread::current().id();
                tracing::debug!(
                    "Processing file: {} on thread {:?}",
                    path.display(),
                    thread_id
                );

                // Each thread gets its own parser
                let mut parser = match PythonParser::new() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("Failed to create parser: {}", e);
                        return None;
                    }
                };

                // Read and parse the file
                match std::fs::read_to_string(path) {
                    Ok(content) => match parser.parse_file(path, &content) {
                        Ok(symbols) => {
                            tracing::debug!(
                                "Parsed {} symbols from {} on thread {:?}",
                                symbols.len(),
                                path.display(),
                                thread_id
                            );
                            Some((path.clone(), symbols))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {}: {}", path.display(), e);
                            None
                        }
                    },
                    Err(e) => {
                        // Only warn for errors other than "file not found" since that's expected
                        // during rapid file system changes (e.g., git operations)
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!("Failed to read {}: {}", path.display(), e);
                        } else {
                            tracing::debug!("File no longer exists: {}", path.display());
                        }
                        None
                    }
                }
            })
            .collect();

        let elapsed = start_time.elapsed();

        // Calculate totals
        let total_symbols: usize = all_file_symbols
            .iter()
            .map(|(_, symbols)| symbols.len())
            .sum();
        let file_count = all_file_symbols.len();

        // Batch update the index
        self.add_files_batch(all_file_symbols)?;

        Ok((file_count, total_symbols, elapsed))
    }

    /// Index all Python files in a workspace directory
    pub fn index_workspace(self: Arc<Self>, root: &PathBuf) -> Result<()> {
        tracing::info!("Starting workspace indexing for: {}", root.display());

        // Collect all Python files first
        let file_collection_start = std::time::Instant::now();
        let python_files = files::collect_python_files(root);
        let file_collection_elapsed = file_collection_start.elapsed();
        tracing::info!(
            "Found {} Python files to index in {:.2}s (using {} threads)",
            python_files.len(),
            file_collection_elapsed.as_secs_f64(),
            num_cpus::get().saturating_sub(1).max(1)
        );

        // Log thread pool info
        tracing::info!(
            "Starting parallel processing with {} concurrent tasks",
            rayon::current_num_threads()
        );

        // Parse and index files
        let (file_count, total_symbols, elapsed) = self.parse_and_index_files(python_files)?;

        tracing::info!(
            "Parallel parsing completed in {:.2}s ({:.0} files/sec)",
            elapsed.as_secs_f64(),
            file_count as f64 / elapsed.as_secs_f64()
        );

        tracing::info!(
            "Indexed {} files with {} symbols",
            file_count,
            total_symbols
        );

        Ok(())
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_creation() {
        let index = SymbolIndex::new();
        assert_eq!(index.get_all_symbols().len(), 0);
    }
}
