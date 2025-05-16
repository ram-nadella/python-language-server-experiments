use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, info};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use async_walkdir::{Filtering, WalkDir};
use futures_lite::future::block_on;
use futures_lite::stream::StreamExt;

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

    info!("Starting scan of directory: {}", args.directory.display());
    
    
    block_on(async {
        let mut entries = WalkDir::new(args.directory).filter(|entry| async move {
            if let Some(true) = entry
                .path()
                .file_name()
                .map(|f| f.to_string_lossy().starts_with('.'))
            {
                return Filtering::IgnoreDir;
            }
            Filtering::Continue
        });

        loop {
            match entries.next().await {
                Some(Ok(entry)) => debug!("file: {}", entry.path().display()),
                Some(Err(e)) => {
                    info!("error: {}", e);
                    break;
                }
                None => break,
            }
        }
    });
    
    let duration = start_time.elapsed();
    info!("Scan complete! Found {} Python files in {:.2?}", 0, duration);
    Ok(())
} 