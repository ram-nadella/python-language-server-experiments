use crate::symbols::{PathRegistry, Symbol, SymbolType};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use std::collections::HashSet;
use std::time::Instant;

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

pub fn search_symbols(
    query: &str,
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    debug: bool,
) -> (Vec<(Symbol, i64)>, SearchMetrics) {
    // Implementation using Skim fuzzy matcher
    let mut metrics = SearchMetrics::default();
    let start_total = Instant::now();

    let matcher_start = Instant::now();
    let matcher = SkimMatcherV2::default();
    metrics.matcher_init_time_ms = matcher_start.elapsed().as_millis();

    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    // Track symbols we've already added to avoid duplicates
    let mut seen_symbols = HashSet::new();

    // Start measuring search time
    let search_start = Instant::now();

    // TODO: parallelize the function vs class search and merge results
    // TODO: consider a different split – eg. depending on number of symbols and number of CPUs, parallelize further

    // Search in functions
    for symbol in functions {
        let name_lower = symbol.name.to_lowercase();
        let score = if name_lower == query_lower {
            // Exact match gets highest score
            1000
        } else {
            // Fuzzy match gets lower score
            matcher.fuzzy_match(&symbol.name, query).unwrap_or(0)
        };

        if score > 0 {
            // Create a unique key for this symbol (name + line number + file path)
            let symbol_key = format!(
                "{}:{}:{}",
                symbol.name, symbol.context.line_number, symbol.context.file_path_index
            );

            // Only add if we haven't seen this symbol before
            if !seen_symbols.contains(&symbol_key) {
                if debug {
                    let file_path = path_registry.get_path(symbol.context.file_path_index);
                    eprintln!(
                        "Match: func {} ({}:{}) - Score: {}",
                        symbol.name,
                        file_path.display(),
                        symbol.context.line_number,
                        score
                    );
                }

                results.push((symbol.clone(), score));
                seen_symbols.insert(symbol_key);
            } else if debug {
                eprintln!("Skipping duplicate function symbol: {}", symbol.name);
            }
        } else if debug {
            eprintln!("No match: func {}", symbol.name);
        }
    }

    // Search in classes
    for symbol in classes {
        let name_lower = symbol.name.to_lowercase();
        let score = if name_lower == query_lower {
            // Exact match gets highest score
            1000
        } else {
            // Fuzzy match gets lower score
            matcher.fuzzy_match(&symbol.name, query).unwrap_or(0)
        };

        if score > 0 {
            // Create a unique key for this symbol (name + line number + file path)
            let symbol_key = format!(
                "{}:{}:{}",
                symbol.name, symbol.context.line_number, symbol.context.file_path_index
            );

            // Only add if we haven't seen this symbol before
            if !seen_symbols.contains(&symbol_key) {
                if debug {
                    let file_path = path_registry.get_path(symbol.context.file_path_index);
                    eprintln!(
                        "Match: class {} ({}:{}) - Score: {}",
                        symbol.name,
                        file_path.display(),
                        symbol.context.line_number,
                        score
                    );
                }

                results.push((symbol.clone(), score));
                seen_symbols.insert(symbol_key);
            } else if debug {
                eprintln!("Skipping duplicate class symbol: {}", symbol.name);
            }
        } else if debug {
            eprintln!("No match: class {}", symbol.name);
        }
    }

    metrics.search_time_ms = search_start.elapsed().as_millis();

    // Sort results by score (descending)
    let sort_start = Instant::now();
    results.sort_by(|a, b| b.1.cmp(&a.1));
    metrics.sort_time_ms = sort_start.elapsed().as_millis();

    metrics.total_time_ms = start_total.elapsed().as_millis();
    metrics.results_count = results.len();

    if debug {
        eprintln!("Skim search completed:");
        eprintln!("  Matcher init: {}ms", metrics.matcher_init_time_ms);
        eprintln!("  Search: {}ms", metrics.search_time_ms);
        eprintln!("  Sort: {}ms", metrics.sort_time_ms);
        eprintln!("  Total: {}ms", metrics.total_time_ms);
        eprintln!("  Results: {}", metrics.results_count);
        eprintln!();
    }

    (results, metrics)
}

/// Print a symbol with its details
pub fn print_symbol(symbol: &Symbol, path_registry: &PathRegistry) {
    // First check if the file_path_index seems valid
    let file_path_display = if symbol.context.file_path_index < path_registry.paths.len() {
        path_registry
            .get_path(symbol.context.file_path_index)
            .display()
            .to_string()
    } else {
        format!("INVALID_PATH_INDEX({})", symbol.context.file_path_index)
    };

    println!(
        "{}: {} | File: {}:{} | Module: {} | Type: {:?} | Parents: {}",
        if matches!(
            symbol.context.symbol_type,
            SymbolType::Class | SymbolType::NestedClass
        ) {
            "CLASS"
        } else {
            "FUNCTION"
        },
        symbol.name,
        file_path_display,
        symbol.context.line_number,
        symbol.context.fully_qualified_module,
        symbol.context.symbol_type,
        symbol
            .context
            .parent_context
            .iter()
            .map(|p| format!("{}:{}", p.name, p.line_number))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // If path index is very high, it might be an error, so add debug info
    if symbol.context.file_path_index > 1000 {
        // Arbitrary threshold for suspicious indices
        println!(
            "  [DEBUG] {}",
            path_registry.debug_path_info(symbol.context.file_path_index)
        );
    }
}
