use anyhow::Result;
use clap::Parser as ClapParser;
use std::path::PathBuf;
use std::time::Instant;
use std::collections::HashSet;
use tracing::info;
use tracing_subscriber;
use tracing_subscriber::EnvFilter;
use flate2::read::GzDecoder;
use std::io::{self, BufReader, Write};
use std::fs::File;
use bincode;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{self, ClearType},
    cursor,
    ExecutableCommand,
};
use symbol_experiments::symbols::{
    Symbol, SymbolStats, SymbolData, PathRegistry, SymbolType
};
use symbol_experiments::files::list_python_files;
use symbol_experiments::python::parse_python_files_parallel;
use symbol_experiments::search::{search_symbols, print_symbol, SearchAlgorithm, SearchMetrics};

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
    load: Option<PathBuf>,

    /// Print all symbols without searching
    #[arg(short, long)]
    print: bool,

    /// Search query (if not provided with --print, will print all symbols)
    #[arg(short, long)]
    search: Option<String>,
    
    /// Run in interactive mode
    #[arg(short, long)]
    interactive: bool,
    
    /// Search algorithm to use (skim or nucleo)
    #[arg(long, default_value = "skim")]
    algorithm: SearchAlgorithm,
    
    /// Show performance metrics for search operations
    #[arg(short, long)]
    metrics: bool,
}

/// Print the search metrics
fn print_metrics(metrics: &SearchMetrics) {
    println!("Performance metrics:");
    println!("  Results found: {}", metrics.results_count);
    println!("  Matcher init time: {}ms", metrics.matcher_init_time_ms);
    println!("  Search time: {}ms", metrics.search_time_ms);
    println!("  Sort time: {}ms", metrics.sort_time_ms);
    println!("  Total time: {}ms", metrics.total_time_ms);
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

fn run_interactive_mode(
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    algorithm: SearchAlgorithm,
    show_metrics: bool,
) -> Result<()> {
    // Enter terminal raw mode for character-by-character input
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    
    let mut query = String::new();
    
    // Main loop
    loop {
        // Clear screen and reset cursor
        stdout.execute(terminal::Clear(ClearType::All))?;
        stdout.execute(cursor::MoveTo(0, 0))?;
        
        // Print header - using write! directly to have better control over positioning
        stdout.execute(cursor::MoveTo(0, 0))?;
        writeln!(stdout, "Interactive symbol search mode ({}). Results update as you type.", algorithm)?;
        stdout.execute(cursor::MoveTo(0, 1))?;
        writeln!(stdout, "Use Backspace to delete, Esc or Ctrl+C to exit.")?;
        stdout.execute(cursor::MoveTo(0, 2))?;
        writeln!(stdout, "----------------------------------------------------------------")?;
        
        // Print search prompt
        stdout.execute(cursor::MoveTo(0, 3))?;
        write!(stdout, "Search: {}", query)?;
        stdout.flush()?;
        
        // Show search results if we have a query
        if !query.is_empty() {
            // Position cursor to start showing results
            stdout.execute(cursor::MoveTo(0, 5))?;
            
            let (results, metrics) = search_symbols(&query, functions, classes, path_registry, false, algorithm);
            writeln!(stdout, "Found {} matches:", results.len())?;
            
            if show_metrics {
                stdout.execute(cursor::MoveTo(0, 6))?;
                writeln!(stdout, "Search time: {}ms, Sort time: {}ms, Total: {}ms", 
                    metrics.search_time_ms, metrics.sort_time_ms, metrics.total_time_ms)?;
            }
            
            stdout.execute(cursor::MoveTo(0, if show_metrics { 7 } else { 6 }))?;
            writeln!(stdout, "----------------------------------------------------------------")?;
            
            // Show top results (limit to 7 for readability)
            let result_limit = 7;
            let mut current_line = if show_metrics { 8 } else { 7 };
            
            for (i, (symbol, score)) in results.iter().take(result_limit).enumerate() {
                let symbol_type = if matches!(symbol.context.symbol_type, SymbolType::Class | SymbolType::NestedClass) {
                    "CLASS"
                } else {
                    "FUNCTION"
                };
                
                stdout.execute(cursor::MoveTo(0, current_line))?;
                writeln!(stdout, "{}. {} \"{}\"", i+1, symbol_type, symbol.name)?;
                current_line += 1;
                
                stdout.execute(cursor::MoveTo(0, current_line))?;
                writeln!(stdout, "   Score: {}", score)?;
                current_line += 1;
                
                // Check if path index is valid
                let file_path_display = if symbol.context.file_path_index < path_registry.paths.len() {
                    path_registry.get_path(symbol.context.file_path_index).display().to_string()
                } else {
                    format!("INVALID_PATH_INDEX({})", symbol.context.file_path_index)
                };
                
                stdout.execute(cursor::MoveTo(0, current_line))?;
                writeln!(stdout, "   File: {}:{}", file_path_display, symbol.context.line_number)?;
                current_line += 1;
                
                // Add debug info if path index looks suspicious
                if symbol.context.file_path_index > 1000 {  // Arbitrary threshold for suspicious indices
                    stdout.execute(cursor::MoveTo(0, current_line))?;
                    writeln!(stdout, "   [DEBUG] {}", 
                            path_registry.debug_path_info(symbol.context.file_path_index))?;
                    current_line += 1;
                }
                
                stdout.execute(cursor::MoveTo(0, current_line))?;
                writeln!(stdout, "   Module: {}", symbol.context.fully_qualified_module)?;
                current_line += 1;
                
                if !symbol.context.parent_context.is_empty() {
                    stdout.execute(cursor::MoveTo(0, current_line))?;
                    let parents = symbol.context.parent_context.iter()
                        .map(|p| format!("{}:{}", p.name, p.line_number))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(stdout, "   Parent: {}", parents)?;
                    current_line += 1;
                }
                
                // Add a blank line between results
                current_line += 1;
            }
            
            if results.len() > result_limit {
                stdout.execute(cursor::MoveTo(0, current_line))?;
                writeln!(stdout, "... and {} more results", results.len() - result_limit)?;
            }
        } else {
            // Type to start searching...
            stdout.execute(cursor::MoveTo(0, 5))?;
            writeln!(stdout, "Type to start searching...")?;
        }
        
        stdout.flush()?;
        
        // Read a key event
        if let Event::Key(key_event) = event::read()? {
            match key_event.code {
                // Exit on Escape
                KeyCode::Esc => break,
                
                // Exit on Ctrl+C
                KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => break,
                
                // Handle character input
                KeyCode::Char(c) => {
                    query.push(c);
                },
                
                // Handle backspace
                KeyCode::Backspace => {
                    query.pop();
                },
                
                // Ignore other keys
                _ => {},
            }
        }
    }
    
    // Restore terminal
    terminal::disable_raw_mode()?;
    stdout.execute(terminal::Clear(ClearType::All))?;
    stdout.execute(cursor::MoveTo(0, 0))?;
    
    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    
    let args = Args::parse();
    let start = Instant::now();
    
    info!("Using search algorithm: {}", args.algorithm);
    
    // Decide whether to load from file or scan directory
    let (functions, classes, path_registry) = if let Some(load_path) = &args.load {
        info!("Loading symbols from file: {}", load_path.display());
        load_symbols_from_file(load_path)?
    } else {
        info!("Scanning directory: {}", args.directory.display());
        
        // Find all Python files
        let python_files: Vec<_> = list_python_files(&args.directory, args.follow_links).collect();
        info!("Found {} Python files", python_files.len());
        
        // Parse Python files and collect symbols
        let stats = SymbolStats::new();
        parse_python_files_parallel(&python_files, &args.directory, &stats)?;
        
        let functions = stats.functions.lock().unwrap().clone();
        let classes = stats.classes.lock().unwrap().clone();
        let path_registry = stats.path_registry.lock().unwrap().clone();
        
        (functions, classes, path_registry)
    };
    
    info!("Processing complete in {}ms", start.elapsed().as_millis());
    info!("Found {} functions and {} classes", functions.len(), classes.len());
    
    // Enter interactive mode if requested
    if args.interactive {
        info!("Entering interactive mode...");
        run_interactive_mode(&functions, &classes, &path_registry, args.algorithm, args.metrics)?;
        return Ok(());
    }
    
    // Handle printing all symbols or searching
    if args.print {
        // Print all symbols
        for symbol in functions.iter().chain(classes.iter()) {
            print_symbol(symbol, &path_registry);
        }
    } else if let Some(query) = args.search {
        // Search for a specific query
        info!("Searching for: {}", query);
        
        // Run search with benchmarking
        let search_start = Instant::now();
        let (results, metrics) = search_symbols(&query, &functions, &classes, &path_registry, true, args.algorithm);
        let search_time = search_start.elapsed();
        
        println!("Found {} matches (search took {}ms):", results.len(), search_time.as_millis());
        
        if args.metrics {
            print_metrics(&metrics);
            println!();
        }
        
        for (symbol, score) in results {
            print_symbol(&symbol, &path_registry);
            println!("  Score: {}", score);
            println!();
        }
    } else {
        // If neither --print nor --search is specified, print summary
        println!("Use --print to list all symbols or --search to search for symbols.");
        println!("Use --interactive for an interactive search experience.");
        println!("Use --algorithm=[skim|nucleo] to select search algorithm (default: skim).");
        println!("Use --metrics to display performance metrics.");
    }
    
    Ok(())
} 