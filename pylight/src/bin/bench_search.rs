use anyhow::Result;
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use std::collections::HashSet;
use std::io::Write;
use tracing::info;
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use flate2::read::GzDecoder;
use std::io::BufReader;
use std::fs::File;
use bincode;
use symbol_experiments::symbols::{
    Symbol, SymbolData, PathRegistry
};
use symbol_experiments::search::{search_symbols, SearchAlgorithm, SearchMetrics};

#[derive(ClapParser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Load symbols from this file
    #[arg(short, long)]
    load: PathBuf,

    /// Search queries to benchmark (comma separated)
    #[arg(short, long, default_value = "test,function,class,init")]
    queries: String,
    
    /// Number of iterations per search
    #[arg(short, long, default_value = "10")]
    iterations: usize,
}

fn load_symbols_from_file(path: &PathBuf) -> Result<(HashSet<Symbol>, HashSet<Symbol>, PathRegistry)> {
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

    Ok((
        functions.into_iter().collect(),
        classes.into_iter().collect(),
        path_registry
    ))
}

/// Run a benchmark for a single query using both algorithms
fn run_benchmark(
    query: &str, 
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    iterations: usize
) -> Result<()> {
    println!("\nBenchmarking search for query: '{}'", query);
    println!("Running {} iterations for each algorithm...", iterations);
    
    // Store metrics for each algorithm
    let mut skim_metrics = Vec::with_capacity(iterations);
    let mut nucleo_metrics = Vec::with_capacity(iterations);
    
    // Run skim algorithm
    for i in 0..iterations {
        print!("Skim iteration {}/{}...\r", i+1, iterations);
        std::io::stdout().flush()?;
        
        let start = Instant::now();
        let (results, metrics) = search_symbols(query, functions, classes, path_registry, false, SearchAlgorithm::Skim);
        let elapsed = start.elapsed();
        
        skim_metrics.push((metrics, elapsed, results.len()));
    }
    println!("\nSkim search completed                      ");
    
    // Run nucleo algorithm
    for i in 0..iterations {
        print!("Nucleo iteration {}/{}...\r", i+1, iterations);
        std::io::stdout().flush()?;
        
        let start = Instant::now();
        let (results, metrics) = search_symbols(query, functions, classes, path_registry, false, SearchAlgorithm::Nucleo);
        let elapsed = start.elapsed();
        
        nucleo_metrics.push((metrics, elapsed, results.len()));
    }
    println!("\nNucleo search completed                     ");
    
    // Calculate averages for skim
    let avg_skim_matcher_init = skim_metrics.iter().map(|(m, _, _)| m.matcher_init_time_ms).sum::<u128>() / iterations as u128;
    let avg_skim_search = skim_metrics.iter().map(|(m, _, _)| m.search_time_ms).sum::<u128>() / iterations as u128;
    let avg_skim_sort = skim_metrics.iter().map(|(m, _, _)| m.sort_time_ms).sum::<u128>() / iterations as u128;
    let avg_skim_total = skim_metrics.iter().map(|(m, _, _)| m.total_time_ms).sum::<u128>() / iterations as u128;
    let avg_skim_total_ext = skim_metrics.iter().map(|(_, e, _)| e.as_millis()).sum::<u128>() / iterations as u128;
    
    // Calculate averages for nucleo
    let avg_nucleo_matcher_init = nucleo_metrics.iter().map(|(m, _, _)| m.matcher_init_time_ms).sum::<u128>() / iterations as u128;
    let avg_nucleo_search = nucleo_metrics.iter().map(|(m, _, _)| m.search_time_ms).sum::<u128>() / iterations as u128;
    let avg_nucleo_sort = nucleo_metrics.iter().map(|(m, _, _)| m.sort_time_ms).sum::<u128>() / iterations as u128;
    let avg_nucleo_total = nucleo_metrics.iter().map(|(m, _, _)| m.total_time_ms).sum::<u128>() / iterations as u128;
    let avg_nucleo_total_ext = nucleo_metrics.iter().map(|(_, e, _)| e.as_millis()).sum::<u128>() / iterations as u128;
    
    // Results count should be the same for both
    let result_count = skim_metrics[0].2;
    
    // Print results table
    println!("\nResults for query '{}' (found {} matches):", query, result_count);
    println!("┌───────────────┬──────────┬───────────┬────────────┐");
    println!("│ Metric        │ Skim     │ Nucleo    │ Difference │");
    println!("├───────────────┼──────────┼───────────┼────────────┤");
    println!("│ Matcher init  │ {:6}ms │ {:7}ms │ {:+8}ms  │", 
             avg_skim_matcher_init, avg_nucleo_matcher_init, 
             avg_nucleo_matcher_init as i128 - avg_skim_matcher_init as i128);
    println!("│ Search time   │ {:6}ms │ {:7}ms │ {:+8}ms  │", 
             avg_skim_search, avg_nucleo_search, 
             avg_nucleo_search as i128 - avg_skim_search as i128);
    println!("│ Sort time     │ {:6}ms │ {:7}ms │ {:+8}ms  │", 
             avg_skim_sort, avg_nucleo_sort, 
             avg_nucleo_sort as i128 - avg_skim_sort as i128);
    println!("│ Total time    │ {:6}ms │ {:7}ms │ {:+8}ms  │", 
             avg_skim_total, avg_nucleo_total, 
             avg_nucleo_total as i128 - avg_skim_total as i128);
    println!("│ Total (ext)   │ {:6}ms │ {:7}ms │ {:+8}ms  │", 
             avg_skim_total_ext, avg_nucleo_total_ext, 
             avg_nucleo_total_ext as i128 - avg_skim_total_ext as i128);
    println!("└───────────────┴──────────┴───────────┴────────────┘");
    
    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    
    let args = Args::parse();
    
    // Load symbols
    let (functions, classes, path_registry) = load_symbols_from_file(&args.load)?;
    info!("Loaded {} functions and {} classes", functions.len(), classes.len());
    
    // Run benchmarks for each query
    let queries: Vec<_> = args.queries.split(',').collect();
    
    for query in queries {
        run_benchmark(query, &functions, &classes, &path_registry, args.iterations)?;
    }
    
    // Print summary
    println!("\nBenchmark complete!");
    println!("Note: The key differences to observe are in 'Search time' and 'Total time'.");
    println!("If nucleo is supposed to be faster, we would expect negative numbers in the Difference column.");
    
    Ok(())
} 