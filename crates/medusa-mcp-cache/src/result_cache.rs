use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::ServerId;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ResultCacheKey {
    pub server_id: ServerId,
    pub tool_name: String,
    pub input_fingerprint: String,
    pub schema_fingerprint: String,
    pub protocol_version: String,
    pub server_version: Option<String>,
}

impl ResultCacheKey {
    pub fn build(
        server_id: ServerId,
        tool_name: impl Into<String>,
        input: &Value,
        schema_fingerprint: impl Into<String>,
        protocol_version: impl Into<String>,
        server_version: Option<String>,
    ) -> Result<Self, &'static str> {
        let tool_name = tool_name.into();
        let schema_fingerprint = schema_fingerprint.into();
        let protocol_version = protocol_version.into();
        if tool_name.trim().is_empty() {
            return Err("tool name cannot be empty");
        }
        if schema_fingerprint.trim().is_empty() {
            return Err("schema fingerprint cannot be empty");
        }
        if protocol_version.trim().is_empty() {
            return Err("protocol version cannot be empty");
        }
        let canonical_input = canonical_json(input);
        Ok(Self {
            server_id,
            tool_name,
            input_fingerprint: fingerprint(canonical_input.as_bytes()),
            schema_fingerprint,
            protocol_version,
            server_version,
        })
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(
            serde_json::to_vec(self)
                .unwrap_or_default()
                .as_slice(),
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDisposition {
    Cacheable,
    Sensitive,
    NonCacheable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CachedResult {
    pub key: ResultCacheKey,
    pub value: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub stored_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub value_fingerprint: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct McpResultCache {
    entries: BTreeMap<ResultCacheKey, CachedResult>,
}

impl McpResultCache {
    pub fn get(&mut self, key: &ResultCacheKey, now: OffsetDateTime) -> Option<&CachedResult> {
        self.purge_expired(now);
        self.entries.get(key)
    }

    pub fn insert(
        &mut self,
        key: ResultCacheKey,
        value: Value,
        stored_at: OffsetDateTime,
        ttl: Duration,
        disposition: CacheDisposition,
    ) -> Result<bool, &'static str> {
        if disposition != CacheDisposition::Cacheable {
            return Ok(false);
        }
        if ttl <= Duration::ZERO {
            return Err("result cache ttl must be positive");
        }
        let value_fingerprint = fingerprint(canonical_json(&value).as_bytes());
        self.entries.insert(
            key.clone(),
            CachedResult {
                key,
                value,
                stored_at,
                expires_at: stored_at + ttl,
                value_fingerprint,
            },
        );
        Ok(true)
    }

    pub fn invalidate_server(&mut self, server_id: &ServerId) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|key, _| &key.server_id != server_id);
        before.saturating_sub(self.entries.len())
    }

    pub fn invalidate_schema(&mut self, schema_fingerprint: &str) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|key, _| key.schema_fingerprint != schema_fingerprint);
        before.saturating_sub(self.entries.len())
    }

    pub fn purge_expired(&mut self, now: OffsetDateTime) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| now < entry.expires_at);
        before.saturating_sub(self.entries.len())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut fields = values.iter().collect::<Vec<_>>();
            fields.sort_by(|left, right| left.0.cmp(right.0));
            format!(
                "{{{}}}",
                fields
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn key(input: Value, schema: &str, version: &str) -> ResultCacheKey {
        ResultCacheKey::build(
            ServerId::parse("github").expect("server"),
            "search",
            &input,
            schema,
            "2025-11-25",
            Some(version.to_owned()),
        )
        .expect("key")
    }

    #[test]
    fn canonical_input_order_hits_the_same_entry() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut cache = McpResultCache::default();
        let first = key(json!({"query":"medusa","limit":5}), "schema-a", "1");
        let reordered = key(json!({"limit":5,"query":"medusa"}), "schema-a", "1");
        assert_eq!(first, reordered);
        cache
            .insert(
                first,
                json!({"items":[1]}),
                now,
                Duration::minutes(5),
                CacheDisposition::Cacheable,
            )
            .expect("insert");
        assert!(cache.get(&reordered, now).is_some());
    }

    #[test]
    fn input_schema_and_version_changes_miss() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut cache = McpResultCache::default();
        let original = key(json!({"query":"medusa"}), "schema-a", "1");
        cache
            .insert(
                original,
                json!({"items":[]}),
                now,
                Duration::minutes(5),
                CacheDisposition::Cacheable,
            )
            .expect("insert");
        assert!(cache
            .get(&key(json!({"query":"other"}), "schema-a", "1"), now)
            .is_none());
        assert!(cache
            .get(&key(json!({"query":"medusa"}), "schema-b", "1"), now)
            .is_none());
        assert!(cache
            .get(&key(json!({"query":"medusa"}), "schema-a", "2"), now)
            .is_none());
    }

    #[test]
    fn sensitive_and_non_cacheable_results_are_never_stored() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut cache = McpResultCache::default();
        for disposition in [CacheDisposition::Sensitive, CacheDisposition::NonCacheable] {
            assert!(!cache
                .insert(
                    key(json!({"query":"secret"}), "schema-a", "1"),
                    json!({"token":"redacted"}),
                    now,
                    Duration::minutes(5),
                    disposition,
                )
                .expect("skip"));
        }
        assert!(cache.is_empty());
    }

    #[test]
    fn expired_entries_are_removed() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut cache = McpResultCache::default();
        let cache_key = key(json!({"query":"medusa"}), "schema-a", "1");
        cache
            .insert(
                cache_key.clone(),
                json!({"items":[]}),
                now,
                Duration::seconds(1),
                CacheDisposition::Cacheable,
            )
            .expect("insert");
        assert!(cache.get(&cache_key, now + Duration::seconds(1)).is_none());
        assert!(cache.is_empty());
    }
}
