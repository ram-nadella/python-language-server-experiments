use anyhow::{Context, Result};
use async_walkdir::{Filtering, WalkDir};
use clap::Parser as ClapParser;
use futures_lite::StreamExt;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, info};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;

#[derive(ClapParser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Maximum depth to scan (0 for unlimited)
    #[arg(short, long, default_value_t = 100)]
    max_depth: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
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

    debug!("Starting scan of directory: {}", args.directory.display());
    let num_cores = num_cpus::get();
    debug!("Using {} CPU cores", num_cores);

    let mut total_files = 0;
    let base_dir = args.directory.clone();
    let max_depth = args.max_depth;
    let mut entries = WalkDir::new(&base_dir)
        .filter(move |entry| {
            let base_dir = base_dir.clone();
            async move {
                let path = entry.path();
                let depth = path.strip_prefix(&base_dir)
                    .map(|p| p.components().count())
                    .unwrap_or(0);
                
                // Skip if beyond max depth
                if max_depth > 0 && depth > max_depth {
                    return Filtering::Ignore;
                }
                
                // Skip .git directories
                if path.to_string_lossy().contains(".git") {
                    return Filtering::IgnoreDir;
                }
                
                // For files, only process Python files
                if path.is_file() {
                    if path.extension().map_or(false, |ext| ext == "py") {
                        return Filtering::Continue;
                    }
                    return Filtering::Ignore;
                }
                
                // For directories, always continue
                Filtering::Continue
            }
        });

    while let Some(entry) = entries.next().await {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
                debug!("Found Python file: {}", path.display());
                total_files += 1;
            }
        }
    }

    let duration = start_time.elapsed();
    info!("Scan complete! Found {} Python files in {:.2?}", total_files, duration);
    Ok(())
} 