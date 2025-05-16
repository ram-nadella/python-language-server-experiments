use std::collections::HashSet;
use crate::symbols::{Symbol, PathRegistry, SymbolType};
use crate::search_skim::search_symbols_skim;
use std::time::Instant;
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern}, 
    Config as NucleoConfig, 
    Matcher as NucleoMatcher
};

/// Defines the available search algorithms
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchAlgorithm {
    /// Uses SkimMatcherV2 for fuzzy matching
    Skim,
    /// Uses fuzzy-matcher with ignore_case option
    Nucleo,
}

impl std::str::FromStr for SearchAlgorithm {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "skim" => Ok(SearchAlgorithm::Skim),
            "nucleo" => Ok(SearchAlgorithm::Nucleo),
            _ => Err(format!("Unknown search algorithm: {}. Valid options are 'skim' or 'nucleo'", s)),
        }
    }
}

impl std::fmt::Display for SearchAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchAlgorithm::Skim => write!(f, "skim"),
            SearchAlgorithm::Nucleo => write!(f, "nucleo"),
        }
    }
}

/// Performance metrics for search operations
#[derive(Debug, Default, Clone)]
pub struct SearchMetrics {
    /// Time spent initializing the matcher
    pub matcher_init_time_ms: u128,
    /// Time spent searching through all symbols
    pub search_time_ms: u128,
    /// Time spent sorting results
    pub sort_time_ms: u128,
    /// Total time spent
    pub total_time_ms: u128,
    /// Number of results found
    pub results_count: usize,
}

/// Implementation of the nucleo search algorithm
pub fn search_symbols_nucleo(
    query: &str,
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    debug: bool,
) -> (Vec<(Symbol, i64)>, SearchMetrics) {
    let mut metrics = SearchMetrics::default();
    let start_total = Instant::now();
    
    // Create the nucleo matcher
    let matcher_start = Instant::now();
    let mut matcher = NucleoMatcher::new(NucleoConfig::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    metrics.matcher_init_time_ms = matcher_start.elapsed().as_millis();
    
    let mut results = Vec::new();
    
    // Track symbols we've already added to avoid duplicates
    let mut seen_symbols = HashSet::new();

    // Start measuring search time
    let search_start = Instant::now();
    
    // Process function symbols
    process_collection(functions, &mut seen_symbols, &pattern, &mut matcher, &mut results, path_registry, debug, query);
    
    // Process class symbols
    process_collection(classes, &mut seen_symbols, &pattern, &mut matcher, &mut results, path_registry, debug, query);

    // Record search time
    metrics.search_time_ms = search_start.elapsed().as_millis();
    
    // Sort by score (highest first)
    let sort_start = Instant::now();
    results.sort_by(|a, b| b.1.cmp(&a.1));
    metrics.sort_time_ms = sort_start.elapsed().as_millis();
    
    metrics.results_count = results.len();
    metrics.total_time_ms = start_total.elapsed().as_millis();
    
    (results, metrics)
}

// Helper function to process a collection of symbols
fn process_collection(
    symbols: &HashSet<Symbol>,
    seen_symbols: &mut HashSet<String>,
    pattern: &Pattern,
    matcher: &mut NucleoMatcher,
    results: &mut Vec<(Symbol, i64)>,
    path_registry: &PathRegistry,
    debug: bool,
    original_query: &str,
) {
    for symbol in symbols {
        let symbol_key = format!("{}:{}:{}", 
            symbol.name, 
            symbol.context.line_number, 
            symbol.context.file_path_index);
        
        // Only check if we've seen this exact symbol (name+line+file) before
        if !seen_symbols.contains(&symbol_key) {
            // Match each symbol one at a time
            let name_slice = [symbol.name.as_str()];
            let matches = pattern.match_list(&name_slice, matcher);
            
            // If we got a match with a positive score
            if !matches.is_empty() && matches[0].1 > 0 {
                // Mark as seen ONLY IF it matches the pattern
                seen_symbols.insert(symbol_key);
                
                // Boost the score for exact matches
                let mut score_i64 = matches[0].1 as i64;
                
                // Check for exact match - case insensitive
                let symbol_name_lower = symbol.name.to_lowercase();
                let query_lower = original_query.to_lowercase();
                
                if symbol_name_lower == query_lower {
                    // Use a very high score to ensure exact matches appear first
                    score_i64 = 10000;
                    
                    if debug {
                        println!("EXACT MATCH BOOSTED: {} (Score: {})", symbol.name, score_i64);
                    }
                }
                
                if debug {
                    let symbol_type = if matches!(symbol.context.symbol_type, SymbolType::Class | SymbolType::NestedClass) {
                        "CLASS"
                    } else {
                        "FUNCTION"
                    };
                    
                    println!("{}: {} | Score: {} | File: {}:{} | Module: {} | Type: {:?} | Parents: {}",
                        symbol_type,
                        symbol.name,
                        score_i64,
                        path_registry.get_path(symbol.context.file_path_index).display(),
                        symbol.context.line_number,
                        symbol.context.fully_qualified_module,
                        symbol.context.symbol_type,
                        symbol.context.parent_context.iter()
                            .map(|p| format!("{}:{}", p.name, p.line_number))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                
                results.push((symbol.clone(), score_i64));
            }
        }
    }
}

/// Unified search function that delegates to the appropriate search implementation
pub fn search_symbols(
    query: &str,
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    debug: bool,
    algorithm: SearchAlgorithm,
) -> (Vec<(Symbol, i64)>, SearchMetrics) {
    // Just delegate to the appropriate implementation
    match algorithm {
        SearchAlgorithm::Skim => {
            // For Skim, we delegate to the skim implementation
            let (results, metrics) = search_symbols_skim(query, functions, classes, path_registry, debug);
            (results, metrics)
        },
        SearchAlgorithm::Nucleo => {
            // For Nucleo, we delegate to the nucleo implementation
            search_symbols_nucleo(query, functions, classes, path_registry, debug)
        },
    }
}

/// Print a symbol with its details
pub fn print_symbol(symbol: &Symbol, path_registry: &PathRegistry) {
    // First check if the file_path_index seems valid
    let file_path_display = if symbol.context.file_path_index < path_registry.paths.len() {
        path_registry.get_path(symbol.context.file_path_index).display().to_string()
    } else {
        format!("INVALID_PATH_INDEX({})", symbol.context.file_path_index)
    };

    println!("{}: {} | File: {}:{} | Module: {} | Type: {:?} | Parents: {}",
        if matches!(symbol.context.symbol_type, SymbolType::Class | SymbolType::NestedClass) { "CLASS" } else { "FUNCTION" },
        symbol.name,
        file_path_display,
        symbol.context.line_number,
        symbol.context.fully_qualified_module,
        symbol.context.symbol_type,
        symbol.context.parent_context.iter()
            .map(|p| format!("{}:{}", p.name, p.line_number))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // If path index is very high, it might be an error, so add debug info
    if symbol.context.file_path_index > 1000 {  // Arbitrary threshold for suspicious indices
        println!("  [DEBUG] {}", path_registry.debug_path_info(symbol.context.file_path_index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::symbols::{Symbol, SymbolContext, SymbolType, PathRegistry};
    use std::collections::HashSet;

    fn create_test_data() -> (HashSet<Symbol>, HashSet<Symbol>, PathRegistry) {
        let mut path_registry = PathRegistry::new();
        let mut functions = HashSet::new();
        let mut classes = HashSet::new();
        
        // Add some paths to the registry
        let file_path1 = PathBuf::from("/test/module1/file1.py");
        let file_path2 = PathBuf::from("/test/module2/file2.py");
        let file_path_index1 = path_registry.register_path(file_path1);
        let file_path_index2 = path_registry.register_path(file_path2);
        
        // Add some functions
        functions.insert(Symbol {
            name: "test_function".to_string(),
            context: SymbolContext {
                file_path_index: file_path_index1,
                line_number: 10,
                module: "file1".to_string(),
                fully_qualified_module: "module1.file1".to_string(),
                symbol_type: SymbolType::Function,
                parent_context: vec![],
            },
        });
        
        functions.insert(Symbol {
            name: "another_function".to_string(),
            context: SymbolContext {
                file_path_index: file_path_index2,
                line_number: 20,
                module: "file2".to_string(),
                fully_qualified_module: "module2.file2".to_string(),
                symbol_type: SymbolType::Function,
                parent_context: vec![],
            },
        });
        
        // Add some classes
        classes.insert(Symbol {
            name: "TestClass".to_string(),
            context: SymbolContext {
                file_path_index: file_path_index1,
                line_number: 5,
                module: "file1".to_string(),
                fully_qualified_module: "module1.file1".to_string(),
                symbol_type: SymbolType::Class,
                parent_context: vec![],
            },
        });
        
        classes.insert(Symbol {
            name: "AnotherClass".to_string(),
            context: SymbolContext {
                file_path_index: file_path_index2,
                line_number: 15,
                module: "file2".to_string(),
                fully_qualified_module: "module2.file2".to_string(),
                symbol_type: SymbolType::Class,
                parent_context: vec![],
            },
        });
        
        (functions, classes, path_registry)
    }

    #[test]
    fn test_search_algorithms() {
        let (functions, classes, path_registry) = create_test_data();
        
        // Test all search algorithms
        for algorithm in [SearchAlgorithm::Skim, SearchAlgorithm::Nucleo] {
            // Test exact match
            let (results, metrics) = search_symbols("test_function", &functions, &classes, &path_registry, false, algorithm);
            assert!(!results.is_empty(), "Should find exact match with {:?}", algorithm);
            println!("{:?} metrics for exact match: {:?}", algorithm, metrics);
            
            // Test fuzzy match
            let (results, metrics) = search_symbols("testfunc", &functions, &classes, &path_registry, false, algorithm);
            assert!(!results.is_empty(), "Should find fuzzy matches with {:?}", algorithm);
            println!("{:?} metrics for fuzzy match: {:?}", algorithm, metrics);
            
            // Test no match
            let (results, metrics) = search_symbols("nonexistent", &functions, &classes, &path_registry, false, algorithm);
            assert!(results.is_empty(), "Should not find any matches with {:?}", algorithm);
            println!("{:?} metrics for no match: {:?}", algorithm, metrics);
            
            // Test case insensitive
            let (results, metrics) = search_symbols("TEST_FUNCTION", &functions, &classes, &path_registry, false, algorithm);
            assert!(!results.is_empty(), "Should find case-insensitive matches with {:?}", algorithm);
            println!("{:?} metrics for case-insensitive match: {:?}", algorithm, metrics);
        }
    }
}