include!("lib.rs");

#[rustfmt::skip]
#[path = "result_cache.rs"]
pub mod result_cache;

pub use result_cache::{CacheDisposition, CachedResult, McpResultCache, ResultCacheKey};
