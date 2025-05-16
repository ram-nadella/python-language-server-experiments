use anyhow::Result;
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use tracing::info;
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use flate2::read::GzDecoder;
use std::io::BufReader;
use std::fs::File;
use bincode;
use symbol_experiments::symbols::{
    Symbol, SymbolStats, SymbolData, PathRegistry
};
use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to scan (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Whether to follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Load symbols from this file instead of scanning directory
    #[arg(short, long)]
    symbols_file: Option<PathBuf>,
}

fn load_symbols_from_file(path: &PathBuf) -> Result<(Vec<Symbol>, Vec<Symbol>, PathRegistry)> {
    info!("Loading symbols from {}...", path.display());
    
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let decoder = GzDecoder::new(reader);
    
    info!("Starting deserialization...");
    let data: SymbolData = bincode::deserialize_from(decoder)?;
    
    info!("Converting to symbol collections...");
    let (functions, classes, paths) = data.into_symbols();
    
    // Create a new PathRegistry and populate it with the exact same paths in the same order
    let mut path_registry = PathRegistry::new();
    
    // First register all paths to ensure they have the same indices as when saved
    for (i, path) in paths.iter().enumerate() {
        let index = path_registry.register_path(path.clone());
        // Sanity check - make sure the index matches the position
        assert_eq!(index, i, "Path registry indexing error: expected index {} for path {}, got {}", 
                  i, path.display(), index);
    }
    
    info!("Load complete! Loaded {} functions, {} classes, and {} paths", 
          functions.len(), classes.len(), paths.len());

    Ok((functions, classes, path_registry))
}

fn print_symbols(symbols: &[Symbol], path_registry: &PathRegistry) {
    for symbol in symbols {
        let path = path_registry.get_path(symbol.context.file_path_index);
        println!("{:?}: {} ({}:{})", 
                 symbol.context.symbol_type, 
                 symbol.name, 
                 path.display(), 
                 symbol.context.line_number);
    }
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    
    let args = Args::parse();
    let start = Instant::now();
    
    // Decide whether to load from file or scan directory
    if let Some(symbols_file) = &args.symbols_file {
        // Load from file
        info!("Loading symbols from file: {}", symbols_file.display());
        let (functions, classes, path_registry) = load_symbols_from_file(symbols_file)?;
        
        info!("Loaded {} functions and {} classes", functions.len(), classes.len());
        
        // Print all symbols, one per line
        print_symbols(&functions, &path_registry);
        print_symbols(&classes, &path_registry);
        
        info!("Listed {} total symbols", functions.len() + classes.len());
    } else {
        // Scan directory
        info!("Scanning directory: {}", args.directory.display());
        
        // Find all Python files
        let python_files: Vec<_> = list_python_files(&args.directory, args.follow_links).collect();
        info!("Found {} Python files", python_files.len());
        
        // Parse Python files and collect symbols
        let stats = SymbolStats::new();
        
        // Process files and print symbols as they're found
        parse_python_files_parallel(&python_files, &args.directory, &stats)?;
        
        // Get final counts
        let functions = stats.functions.lock().unwrap();
        let classes = stats.classes.lock().unwrap();
        let path_registry = stats.path_registry.lock().unwrap();
        
        // Print all symbols to ensure complete output
        print_symbols(&functions.iter().cloned().collect::<Vec<_>>(), &path_registry);
        print_symbols(&classes.iter().cloned().collect::<Vec<_>>(), &path_registry);
        
        info!("Found and listed {} functions and {} classes", 
             functions.len(), classes.len());
    }
    
    info!("Processing complete in {}ms", start.elapsed().as_millis());
    
    Ok(())
} 