use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;
use tree_sitter::{Node, Parser};

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,
}

#[derive(Debug, Default)]
struct SymbolStats {
    function_names: Arc<Mutex<HashSet<String>>>,
    class_names: Arc<Mutex<HashSet<String>>>,
    syntax_errors: AtomicUsize,
    io_errors: AtomicUsize,
    other_errors: AtomicUsize,
}

impl SymbolStats {
    fn new() -> Self {
        Self {
            function_names: Arc::new(Mutex::new(HashSet::new())),
            class_names: Arc::new(Mutex::new(HashSet::new())),
            syntax_errors: AtomicUsize::new(0),
            io_errors: AtomicUsize::new(0),
            other_errors: AtomicUsize::new(0),
        }
    }

    fn get_counts(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.function_names.lock().unwrap().len(),
            self.class_names.lock().unwrap().len(),
            self.syntax_errors.load(Ordering::Relaxed),
            self.io_errors.load(Ordering::Relaxed),
            self.other_errors.load(Ordering::Relaxed),
        )
    }
}

fn get_node_text(node: Node, source: &str) -> String {
    source[node.byte_range()].to_string()
}

fn collect_symbols(node: Node, source: &str) -> (Vec<String>, Vec<String>) {
    let mut function_names = Vec::new();
    let mut class_names = Vec::new();

    let mut cursor = node.walk();
    cursor.goto_first_child();

    loop {
        let node = cursor.node();
        match node.kind() {
            "function_definition" => {
                // Get the function name from the name field
                if let Some(name_node) = node.child_by_field_name("name") {
                    function_names.push(get_node_text(name_node, source));
                }
            }
            "class_definition" => {
                // Get the class name from the name field
                if let Some(name_node) = node.child_by_field_name("name") {
                    class_names.push(get_node_text(name_node, source));
                }
            }
            _ => {}
        }

        if !cursor.goto_next_sibling() && !cursor.goto_parent() {
            break;
        }
    }

    (function_names, class_names)
}

fn parse_python_file(parser: &mut Parser, path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let source = std::fs::read_to_string(path)?;
    let tree = parser
        .parse(&source, None)
        .context("Failed to parse file")?;

    // Check if the tree has any errors
    let has_error = tree.root_node().has_error();

    if has_error {
        anyhow::bail!("Syntax error in file");
    }

    Ok(collect_symbols(tree.root_node(), &source))
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let start_time = Instant::now();

    // Initialize tree-sitter
    let language = tree_sitter_python::language();

    info!("Starting scan of directory: {}", args.directory.display());

    // First collect all Python files
    let discovery_start = Instant::now();
    let mut python_files = Vec::new();
    let mut entries_processed = 0;

    for entry in walkdir::WalkDir::new(&args.directory)
        .follow_links(args.follow_links)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path().to_string_lossy().contains(".git"))
    {
        entries_processed += 1;
        if entries_processed % 10_000 == 0 {
            debug!("Scanned {} entries so far...", entries_processed);
        }

        if entry.path().extension().is_some_and(|ext| ext == "py") {
            python_files.push(entry.path().to_path_buf());
        }
    }

    let discovery_duration = discovery_start.elapsed();
    info!(
        "File discovery phase: Found {} Python files out of {} total entries in {:.2?}",
        python_files.len(),
        entries_processed,
        discovery_duration
    );

    // Now parse all files in parallel
    let parsing_start = Instant::now();
    let total_files = python_files.len();
    let stats = Arc::new(SymbolStats::new());
    let progress = Arc::new(AtomicUsize::new(0));

    // Process files in parallel chunks
    python_files.par_chunks(1000).for_each(|chunk| {
        // Each thread gets its own parser
        let mut parser = Parser::new();
        parser.set_language(language).unwrap();

        for path in chunk {
            let result = parse_python_file(&mut parser, path);
            match result {
                Ok((function_names, class_names)) => {
                    // Add function names to the set
                    let mut names = stats.function_names.lock().unwrap();
                    for name in function_names {
                        names.insert(name);
                    }
                    // Add class names to the set
                    let mut names = stats.class_names.lock().unwrap();
                    for name in class_names {
                        names.insert(name);
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Syntax error") {
                        stats.syntax_errors.fetch_add(1, Ordering::Relaxed);
                    } else if err_str.contains("No such file") {
                        stats.io_errors.fetch_add(1, Ordering::Relaxed);
                    } else {
                        stats.other_errors.fetch_add(1, Ordering::Relaxed);
                        info!("Failed to parse file {}: {}", path.display(), e);
                    }
                }
            }
        }

        // Update progress
        let current = progress.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
        let elapsed = parsing_start.elapsed();
        let rate = current as f32 / elapsed.as_secs_f32();
        let remaining = (total_files - current) as f32 / rate;

        debug!(
            "Progress: {}/{} files ({:.1}%) - {:.1} files/sec - Est. remaining: {:.1}s",
            current,
            total_files,
            (current as f32 / total_files as f32) * 100.0,
            rate,
            remaining
        );
    });

    let parsing_duration = parsing_start.elapsed();
    let total_duration = start_time.elapsed();

    let (function_count, class_count, syntax_errors, io_errors, other_errors) = stats.get_counts();

    info!("Final Statistics:");
    info!("Discovery time: {:.2}s", discovery_duration.as_secs_f32());
    info!("Parse time: {:.2}s", parsing_duration.as_secs_f32());
    info!("Total time: {:.2}s", total_duration.as_secs_f32());
    info!("Unique functions found: {}", function_count);
    info!("Unique classes found: {}", class_count);
    info!("Files with syntax errors: {}", syntax_errors);
    info!("Files with I/O errors: {}", io_errors);
    info!("Files with other errors: {}", other_errors);
    info!(
        "Average parse rate: {:.1} files/second",
        total_files as f32 / parsing_duration.as_secs_f32()
    );

    // print some sample names
    debug!("Sample function names (first 10):");
    for name in stats.function_names.lock().unwrap().iter().take(10) {
        debug!("  {}", name);
    }
    debug!("Sample class names (first 10):");
    for name in stats.class_names.lock().unwrap().iter().take(10) {
        debug!("  {}", name);
    }

    Ok(())
}
