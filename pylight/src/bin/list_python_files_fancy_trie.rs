use anyhow::Result;
use clap::Parser as ClapParser;
use std::time::Instant;
use tracing::{debug, info};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use symbol_experiments::files::list_python_files_with_depth;
use std::thread;
use std::time::Duration;
use std::collections::HashMap;
use std::path::{Path, PathBuf, Component};

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Maximum depth to scan (0 for unlimited)
    #[arg(short, long, default_value_t = 100)]
    max_depth: usize,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,
}



/// A memory-efficient trie data structure optimized for file paths
/// Each node represents a path component (directory or file) rather than individual characters
#[derive(Debug, Default)]
pub struct PathTrie {
    /// Whether this node represents a file (endpoint)
    is_file: bool,
    /// Child nodes, keyed by path component
    children: HashMap<String, PathTrie>,
}

impl PathTrie {
    /// Create a new, empty PathTrie
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a path into the trie
    pub fn insert<P: AsRef<Path>>(&mut self, path: P) {
        // Get path components (directories and filename)
        let components: Vec<_> = path.as_ref()
            .components()
            .filter_map(|comp| match comp {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                Component::RootDir => Some("/".to_string()),
                _ => None, // Skip other component types like CurDir, ParentDir
            })
            .collect();
        
        // Insert components into trie
        let mut current = self;
        for component in components {
            current = current.children.entry(component).or_default();
        }
        current.is_file = true;
    }

    /// Get all file paths stored in the trie
    pub fn get_all_paths(&self) -> Vec<PathBuf> {
        let mut result = Vec::new();
        let mut current_path = PathBuf::new();
        self.collect_paths(&mut current_path, &mut result);
        result
    }

    /// Helper method to recursively collect all paths
    fn collect_paths(&self, current_path: &mut PathBuf, result: &mut Vec<PathBuf>) {
        if self.is_file {
            result.push(current_path.clone());
        }

        for (component, child) in &self.children {
            // Special handling for root directory
            if component == "/" {
                let root = Path::new("/");
                *current_path = root.to_path_buf();
            } else {
                current_path.push(component);
            }
            
            child.collect_paths(current_path, result);
            
            // Remove the last component before proceeding to siblings
            if component != "/" {
                current_path.pop();
            }
        }
    }

    /// Count the number of files stored in the trie
    pub fn len(&self) -> usize {
        let mut count = if self.is_file { 1 } else { 0 };
        for child in self.children.values() {
            count += child.len();
        }
        count
    }

    /// Check if the trie is empty (contains no files)
    pub fn is_empty(&self) -> bool {
        !self.is_file && self.children.is_empty()
    }

    /// Get memory usage statistics for the trie
    pub fn memory_stats(&self) -> TrieStats {
        let mut stats = TrieStats {
            node_count: 1,
            leaf_count: if self.is_file { 1 } else { 0 },
            max_depth: 0,
            component_count: self.children.len(),
        };

        for child in self.children.values() {
            let child_stats = child.memory_stats();
            stats.node_count += child_stats.node_count;
            stats.leaf_count += child_stats.leaf_count;
            stats.component_count += child_stats.component_count;
            stats.max_depth = stats.max_depth.max(child_stats.max_depth + 1);
        }

        stats
    }

    /// Iterate through all file paths in the trie
    pub fn iter(&self) -> PathTrieIter {
        let mut paths = Vec::new();
        self.get_all_paths().into_iter().for_each(|p| paths.push(p));
        PathTrieIter { paths, index: 0 }
    }
}

/// Statistics about the trie structure
#[derive(Debug, Default)]
pub struct TrieStats {
    /// Total number of nodes in the trie
    pub node_count: usize,
    /// Number of nodes that represent files (endpoints)
    pub leaf_count: usize,
    /// Maximum depth of the trie
    pub max_depth: usize,
    /// Total number of path components stored
    pub component_count: usize,
}

/// Iterator over all paths in the trie
pub struct PathTrieIter {
    paths: Vec<PathBuf>,
    index: usize,
}

impl Iterator for PathTrieIter {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.paths.len() {
            let path = self.paths[self.index].clone();
            self.index += 1;
            Some(path)
        } else {
            None
        }
    }
}


fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let start_time = Instant::now();

    // Validate directory exists
    if !args.directory.exists() {
        anyhow::bail!("Directory does not exist: {}", args.directory.display());
    }

    info!("Scanning directory: {}", args.directory.display());
    
    // Get Python files as PathBuf objects
    let python_files_iter = list_python_files_with_depth(
        &args.directory, 
        args.follow_links,
        args.max_depth
    );
    
    // Use our path component trie for memory-efficient storage
    let mut path_trie = PathTrie::new();
    let mut file_count = 0;
    
    // Insert each path into the trie
    for path in python_files_iter {
        path_trie.insert(&path);
        file_count += 1;
    }
    
    // Sleep for 2 minutes
    info!("Sleeping for 2 minutes...");
    thread::sleep(Duration::from_secs(120));
    info!("Sleep finished, continuing execution");
    
    // Get trie statistics
    let stats = path_trie.memory_stats();
    let trie_file_count = path_trie.len();
    
    info!("PathTrie memory statistics:");
    info!("- Total nodes: {}", stats.node_count);
    info!("- Leaf nodes (files): {}", stats.leaf_count);
    info!("- Path components stored: {}", stats.component_count);
    info!("- Maximum path depth: {}", stats.max_depth);
    
    assert_eq!(file_count, trie_file_count, 
        "File count mismatch between iteration ({}) and trie structure ({})", 
        file_count, trie_file_count);
    
    // Extract all paths from the trie for debugging
    for file_path in path_trie.iter() {
        debug!("Found Python file in trie: {}", file_path.display());
    }

    let duration = start_time.elapsed();
    info!("Scan complete! Found {} Python files in {:.2?}", file_count, duration);
    Ok(())
}


