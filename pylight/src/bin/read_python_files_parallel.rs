use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use tokio::fs;
use tokio::task;
use tracing::{debug, info};
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use walkdir::WalkDir;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::AsyncReadExt;

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Number of parallel tasks (defaults to number of CPU cores)
    #[arg(short, long)]
    parallel_tasks: Option<usize>,
}

async fn process_file(
    path: PathBuf,
    processed_files: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
    last_progress: Arc<AtomicU64>,
    total_files: u64,
) -> Result<()> {
    let file_size = fs::metadata(&path).await?.len();
    let mut file = fs::File::open(&path).await?;
    let mut contents = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut contents).await?;
    
    total_bytes.fetch_add(file_size, Ordering::SeqCst);
    let processed = processed_files.fetch_add(1, Ordering::SeqCst) + 1;
    
    // Log progress every 5 seconds
    let now = std::time::Instant::now();
    let last = last_progress.load(Ordering::SeqCst);
    if now.elapsed().as_secs() - last >= 5 {
        info!("Processed {}/{} files ({} bytes)", processed, total_files, total_bytes.load(Ordering::SeqCst));
        last_progress.store(now.elapsed().as_secs(), Ordering::SeqCst);
    }
    
    Ok(())
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

    info!("Starting parallel scan of directory: {}", args.directory.display());
    
    // Shared counters for atomic updates
    let processed_files = Arc::new(AtomicU64::new(0));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let last_progress = Arc::new(AtomicU64::new(0));

    // Determine number of parallel tasks
    let num_tasks = args.parallel_tasks.unwrap_or_else(|| num_cpus::get());
    info!("Using {} parallel tasks", num_tasks);

    // First, collect all Python files using synchronous walkdir
    let discovery_start = Instant::now();
    let mut python_files = Vec::new();
    let mut entries_processed = 0;

    for entry in WalkDir::new(&args.directory)
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

    // Process files in parallel using async tasks
    let total_files = python_files.len() as u64;
    let python_files = Arc::new(Mutex::new(python_files));
    let mut handles = Vec::new();

    for _ in 0..num_tasks {
        let python_files = Arc::clone(&python_files);
        let processed_files = Arc::clone(&processed_files);
        let total_bytes = Arc::clone(&total_bytes);
        let last_progress = Arc::clone(&last_progress);

        handles.push(task::spawn(async move {
            while let Some(path) = {
                let mut files = python_files.lock().unwrap();
                files.pop()
            } {
                if let Err(e) = process_file(
                    path,
                    Arc::clone(&processed_files),
                    Arc::clone(&total_bytes),
                    Arc::clone(&last_progress),
                    total_files,
                ).await {
                    debug!("Error processing file: {}", e);
                }
            }
        }));
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await?;
    }

    let total_duration = start_time.elapsed();
    let reading_duration = total_duration - discovery_duration;
    info!("Scan complete! Processed {} Python files ({} bytes) in {:.2?}", 
          total_files, total_bytes.load(Ordering::SeqCst), total_duration);
    info!("Breakdown:");
    info!("  - File discovery: {:.2?}", discovery_duration);
    info!("  - File reading: {:.2?}", reading_duration);
    Ok(())
} 