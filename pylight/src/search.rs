//! Symbol search functionality

use crate::symbols::Symbol;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::sync::Arc;

pub struct SearchEngine {
    matcher: SkimMatcherV2,
}

#[derive(Debug)]
pub struct SearchResult {
    pub symbol: Arc<Symbol>,
    pub score: i64,
}

impl SearchEngine {
    pub fn new() -> Self {
        Self {
            matcher: SkimMatcherV2::default().smart_case().use_cache(true),
        }
    }

    pub fn search(&self, query: &str, symbols: &[Arc<Symbol>]) -> Vec<SearchResult> {
        if query.is_empty() {
            // Return first 100 symbols when query is empty
            return symbols
                .iter()
                .take(100)
                .map(|symbol| SearchResult {
                    symbol: Arc::clone(symbol),
                    score: 0,
                })
                .collect();
        }

        let start_time = std::time::Instant::now();

        // First pass: collect fuzzy match results
        let mut results: Vec<SearchResult> = symbols
            .iter()
            .filter_map(|symbol| {
                self.matcher
                    .fuzzy_match(&symbol.name, query)
                    .map(|score| SearchResult {
                        symbol: Arc::clone(symbol),
                        score,
                    })
            })
            .collect();

        let fuzzy_match_duration = start_time.elapsed();

        // Second pass: boost exact matches, this is custom logic as the fuzzy matcher is not scoring
        // exact matches higher than prefix matches eg. when searching for test, test and test_something
        // are getting the same score
        let boost_start = std::time::Instant::now();
        let query_len = query.len();
        let mut boosted_count = 0;

        for result in &mut results {
            if result.symbol.name.len() == query_len
                && result.symbol.name.eq_ignore_ascii_case(query)
            {
                let original_score = result.score;
                result.score = result.score.saturating_mul(10);
                boosted_count += 1;

                tracing::debug!(
                    "Boosted exact match '{}': {} -> {}",
                    result.symbol.name,
                    original_score,
                    result.score
                );
            }
        }

        let boost_duration = boost_start.elapsed();

        // Sort by score descending
        let sort_start = std::time::Instant::now();
        results.sort_by(|a, b| b.score.cmp(&a.score));
        let sort_duration = sort_start.elapsed();

        let total_duration = start_time.elapsed();

        tracing::debug!(
            "Search for '{}': {} results, {} boosted. Timing - fuzzy: {:?}, boost: {:?}, sort: {:?}, total: {:?}",
            query,
            results.len(),
            boosted_count,
            fuzzy_match_duration,
            boost_duration,
            sort_duration,
            total_duration
        );

        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!(
                "Top 10 results: {:?}",
                &results.iter().take(10).collect::<Vec<_>>()
            );
        }

        results
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolKind;
    use std::path::PathBuf;

    #[test]
    fn test_search_engine_creation() {
        let engine = SearchEngine::new();
        let results = engine.search("test", &[]);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_search_basic() {
        let engine = SearchEngine::new();
        let symbols = vec![
            Arc::new(Symbol::new(
                "test_function".to_string(),
                SymbolKind::Function,
                PathBuf::from("test.py"),
                1,
                0,
            )),
            Arc::new(Symbol::new(
                "another_function".to_string(),
                SymbolKind::Function,
                PathBuf::from("test.py"),
                2,
                0,
            )),
        ];

        let results = engine.search("test", &symbols);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol.name, "test_function");
    }
}
