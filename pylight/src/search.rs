//! Symbol search functionality

use crate::symbols::Symbol;
use nucleo_matcher::pattern::{Atom, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::Arc;

pub struct SearchEngine {
    matcher: Matcher,
}

#[derive(Debug)]
pub struct SearchResult {
    pub symbol: Arc<Symbol>,
    pub score: i64,
}

impl SearchEngine {
    pub fn new() -> Self {
        let mut config = Config::DEFAULT;
        config.normalize = true;
        Self {
            matcher: Matcher::new(config),
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

        // Create the search pattern with smart case matching
        let pattern = Atom::parse(query, CaseMatching::Smart, Normalization::Smart);

        // Create a matcher instance for scoring
        let mut matcher = self.matcher.clone();

        // First pass: collect fuzzy match results
        let mut results: Vec<SearchResult> = symbols
            .iter()
            .filter_map(|symbol| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(&symbol.name, &mut buf);
                pattern
                    .score(haystack, &mut matcher)
                    .map(|score| SearchResult {
                        symbol: Arc::clone(symbol),
                        score: score as i64,
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
                "test.py".to_string(),
                1,
                0,
            )),
            Arc::new(Symbol::new(
                "another_function".to_string(),
                SymbolKind::Function,
                "test.py".to_string(),
                2,
                0,
            )),
        ];

        let results = engine.search("test", &symbols);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol.name, "test_function");
    }
}
