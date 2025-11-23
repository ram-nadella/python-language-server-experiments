//! Simple string cache for deduplicating file paths and module paths

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// A thread-safe string cache that deduplicates strings using Arc
#[derive(Clone, Default)]
pub struct StringCache {
    cache: Arc<RwLock<HashMap<String, Arc<str>>>>,
}

impl StringCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or insert a string into the cache, returning an Arc<str>
    pub fn intern(&self, s: impl Into<String>) -> Arc<str> {
        let s = s.into();

        // Fast path: check if already cached
        {
            let cache = self.cache.read();
            if let Some(cached) = cache.get(&s) {
                return Arc::clone(cached);
            }
        }

        // Slow path: insert into cache
        let mut cache = self.cache.write();
        cache
            .entry(s.clone())
            .or_insert_with(|| Arc::from(s.as_str()))
            .clone()
    }

    /// Get the number of unique strings in the cache
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Check if the cache is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cache.read().is_empty()
    }

    /// Clear the cache
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.cache.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_cache() {
        let cache = StringCache::new();

        let s1 = cache.intern("hello");
        let s2 = cache.intern("hello");
        let s3 = cache.intern("world");

        // Same string should return same Arc
        assert!(Arc::ptr_eq(&s1, &s2));

        // Different strings should not
        assert!(!Arc::ptr_eq(&s1, &s3));

        assert_eq!(cache.len(), 2);
    }
}
