include!("lib.rs");

pub mod result_cache;

pub use result_cache::{CacheDisposition, CachedResult, McpResultCache, ResultCacheKey};
