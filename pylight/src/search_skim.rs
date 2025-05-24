use crate::search::SearchMetrics;
use crate::symbols::{PathRegistry, Symbol};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use std::collections::HashSet;
use std::time::Instant;

pub fn search_symbols_skim(
    query: &str,
    functions: &HashSet<Symbol>,
    classes: &HashSet<Symbol>,
    path_registry: &PathRegistry,
    debug: bool,
) -> (Vec<(Symbol, i64)>, SearchMetrics) {
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
            if seen_symbols.insert(symbol_key) {
                if debug {
                    println!("FUNCTION: {} | Score: {} | File: {}:{} | Module: {} | Type: {:?} | Parents: {}",
                        symbol.name,
                        score,
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
                results.push((symbol.clone(), score));
            }
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
            if seen_symbols.insert(symbol_key) {
                if debug {
                    println!("CLASS: {} | Score: {} | File: {}:{} | Module: {} | Type: {:?} | Parents: {}",
                        symbol.name,
                        score,
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
                results.push((symbol.clone(), score));
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{PathRegistry, Symbol, SymbolContext, SymbolType};
    use std::collections::HashSet;
    use std::path::PathBuf;

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
    fn test_search_exact_match() {
        let (functions, classes, path_registry) = create_test_data();

        // Test exact match
        let (results, metrics) =
            search_symbols_skim("test_function", &functions, &classes, &path_registry, false);
        assert_eq!(results.len(), 1, "Should find exactly one match");
        assert_eq!(
            results[0].0.name, "test_function",
            "Should match the correct symbol"
        );
        assert_eq!(results[0].1, 1000, "Exact match should have score 1000");
        println!("Metrics for exact match: {:?}", metrics);
    }

    #[test]
    fn test_search_fuzzy_match() {
        let (functions, classes, path_registry) = create_test_data();

        // Test fuzzy match
        let (results, metrics) =
            search_symbols_skim("testfunc", &functions, &classes, &path_registry, false);
        assert!(!results.is_empty(), "Should find fuzzy matches");
        let has_test_function = results.iter().any(|(s, _)| s.name == "test_function");
        assert!(
            has_test_function,
            "Should find 'test_function' with fuzzy search"
        );
        println!("Metrics for fuzzy match: {:?}", metrics);
    }

    #[test]
    fn test_search_no_match() {
        let (functions, classes, path_registry) = create_test_data();

        // Test no match
        let (results, metrics) =
            search_symbols_skim("nonexistent", &functions, &classes, &path_registry, false);
        assert!(results.is_empty(), "Should not find any matches");
        println!("Metrics for no match: {:?}", metrics);
    }

    #[test]
    fn test_case_insensitive_search() {
        let (functions, classes, path_registry) = create_test_data();

        // Test case insensitive search
        let (results, metrics) =
            search_symbols_skim("TEST_FUNCTION", &functions, &classes, &path_registry, false);
        assert!(!results.is_empty(), "Should find case-insensitive matches");
        let has_test_function = results.iter().any(|(s, _)| s.name == "test_function");
        assert!(
            has_test_function,
            "Should find 'test_function' with case-insensitive search"
        );
        println!("Metrics for case-insensitive match: {:?}", metrics);
    }
}
