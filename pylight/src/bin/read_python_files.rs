use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, info};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use walkdir::WalkDir;
use std::fs;

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

    info!("Starting scan of directory: {}", args.directory.display());
    
    // First, collect all Python files using synchronous walkdir
    let discovery_start = Instant::now();
    let mut python_files = Vec::new();
    let mut entries_processed = 0;

    for entry in WalkDir::new(&args.directory)
        .follow_links(args.follow_links)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path().to_string_lossy().contains(".git"))
    {
        entries_processed += 1;
        if entries_processed % 10000 == 0 {
            debug!("Processed {} entries so far...", entries_processed);
        }

        if entry.path().extension().map_or(false, |ext| ext == "py") {
            python_files.push(entry.path().to_path_buf());
        }
    }

    let discovery_duration = discovery_start.elapsed();
    info!("File discovery phase: Found {} Python files out of {} total entries in {:.2?}", 
          python_files.len(), entries_processed, discovery_duration);

    // Now process the files
    let reading_start = Instant::now();
    let mut total_bytes = 0;
    let total_files = python_files.len();

    for path in python_files {
        match fs::read_to_string(&path) {
            Ok(contents) => {
                total_bytes += contents.len();
            }
            Err(e) => {
                debug!("Failed to read file {}: {}", path.display(), e);
            }
        }
    }

    let reading_duration = reading_start.elapsed();
    let total_duration = start_time.elapsed();

    info!("Scan complete! Found {} Python files ({} bytes) in {:.2?}", 
          total_files, total_bytes, total_duration);
    info!("Breakdown:");
    info!("  - File discovery: {:.2?}", discovery_duration);
    info!("  - File reading: {:.2?}", reading_duration);
    Ok(())
} 