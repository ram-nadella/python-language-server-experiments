use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, debug};
use tree_sitter::Parser;
use rayon::prelude::*;
use tracing_subscriber;
use tracing_subscriber::EnvFilter;

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

struct ParseStats {
    success: AtomicUsize,
    syntax_errors: AtomicUsize,
    io_errors: AtomicUsize,
    other_errors: AtomicUsize,
}

impl ParseStats {
    fn new() -> Self {
        Self {
            success: AtomicUsize::new(0),
            syntax_errors: AtomicUsize::new(0),
            io_errors: AtomicUsize::new(0),
            other_errors: AtomicUsize::new(0),
        }
    }

    fn get_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.success.load(Ordering::Relaxed),
            self.syntax_errors.load(Ordering::Relaxed),
            self.io_errors.load(Ordering::Relaxed),
            self.other_errors.load(Ordering::Relaxed),
        )
    }
}

fn parse_python_file(parser: &mut Parser, path: &Path) -> Result<()> {
    let source = std::fs::read_to_string(path)?;
    let tree = parser.parse(&source, None)
        .context("Failed to parse file")?;
    
    // Check if the tree has any errors
    let has_error = tree.root_node()
        .has_error();
    
    if has_error {
        anyhow::bail!("Syntax error in file");
    }
    
    Ok(())
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

        if entry.path().extension().map_or(false, |ext| ext == "py") {
            python_files.push(entry.path().to_path_buf());
        }
    }

    let discovery_duration = discovery_start.elapsed();
    info!(
        "File discovery phase: Found {} Python files out of {} total entries in {:.2?}", 
        python_files.len(), entries_processed, discovery_duration
    );

    // Now parse all files in parallel
    let parsing_start = Instant::now();
    let total_files = python_files.len();
    let stats = Arc::new(ParseStats::new());
    let progress = Arc::new(AtomicUsize::new(0));

    // Process files in parallel chunks
    python_files.par_chunks(1000).for_each(|chunk| {
        // Each thread gets its own parser
        let mut parser = Parser::new();
        parser.set_language(language).unwrap();
        
        for path in chunk {
            let result = parse_python_file(&mut parser, path);
            match result {
                Ok(_) => {
                    stats.success.fetch_add(1, Ordering::Relaxed);
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
        
        info!(
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
    
    let (success, syntax_errors, io_errors, other_errors) = stats.get_counts();
    
    info!("\nFinal Statistics:");
    info!("Discovery time: {:.2}s", discovery_duration.as_secs_f32());
    info!("Parse time: {:.2}s", parsing_duration.as_secs_f32());
    info!("Total time: {:.2}s", total_duration.as_secs_f32());
    info!("Successfully parsed: {} files", success);
    info!("Syntax errors: {} files", syntax_errors);
    info!("I/O errors: {} files", io_errors);
    info!("Other errors: {} files", other_errors);
    info!(
        "Average parse rate: {:.1} files/second",
        total_files as f32 / parsing_duration.as_secs_f32()
    );

    Ok(())
} 