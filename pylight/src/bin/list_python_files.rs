use anyhow::Result;
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use symbol_experiments::files::list_python_files_recursive;
use tracing::{debug, info, Level};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use walkdir::{DirEntry, WalkDir};

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Maximum depth to scan (0 for unlimited)
    #[arg(short, long, default_value_t = 0)]
    max_depth: usize,

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

    info!("Scanning directory: {}", args.directory.display());

    let python_files: Vec<String> = list_python_files_recursive(&args.directory, args.follow_links)
        .unwrap()
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();

    // Sleep for 2 minutes
    info!("Sleeping for 2 minutes...");
    thread::sleep(Duration::from_secs(120));
    info!("Sleep finished, continuing execution");

    let total_files = python_files.len();

    for file_path in &python_files {
        debug!("Found Python file: {}", file_path);
    }

    let duration = start_time.elapsed();
    info!(
        "Scan complete! Found {} Python files in {:.2?}",
        total_files, duration
    );
    Ok(())
}
