use anyhow::Result;
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use tracing::info;
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use symbol_experiments::symbols::{SymbolStats, Symbol, save_symbols};
use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;
use std::mem;

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan
    #[arg(short, long)]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Save symbols to this file
    #[arg(short, long)]
    save: Option<PathBuf>,
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    
    // Parse command line arguments
    let args = Args::parse();
    let start = Instant::now();
    
    info!("Collecting Python files from {}", args.directory.display());
    let files: Vec<_> = list_python_files(&args.directory, args.follow_links).collect();
    info!("Found {} Python files in {}ms", files.len(), start.elapsed().as_millis());
    
    info!("Parsing Python files in parallel...");
    let parse_start = Instant::now();
    let stats = SymbolStats::new();
    
    parse_python_files_parallel(&files, &args.directory, &stats)?;
    
    let (num_functions, num_classes, syntax_errors, io_errors, other_errors) = stats.get_counts();
    
    info!("Parsing complete in {}ms", parse_start.elapsed().as_millis());
    info!("Found {} functions and {} classes", num_functions, num_classes);
    info!("Errors: {} syntax, {} I/O, {} other", syntax_errors, io_errors, other_errors);
    
    println!("Memory usage for functions: {} bytes", mem::size_of::<Symbol>() * num_functions);
    println!("Memory usage for classes: {} bytes", mem::size_of::<Symbol>() * num_classes);
    
    // Print path registry stats
    let path_registry = stats.path_registry.lock().unwrap();
    path_registry.print_stats();
    info!("Total path storage: {} bytes", path_registry.total_path_bytes());
    drop(path_registry);
    
    // Save symbols if requested
    if let Some(path) = &args.save {
        let save_start = Instant::now();
        info!("Saving symbols to {}...", path.display());
        save_symbols(path, &stats)?;
        info!("Save complete in {}ms", save_start.elapsed().as_millis());
    }
    
    info!("Total time: {}ms", start.elapsed().as_millis());
    
    Ok(())
}
