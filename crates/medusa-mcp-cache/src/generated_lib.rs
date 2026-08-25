include!("lib.rs");

pub mod result_cache {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use time::{Duration, OffsetDateTime};

    #[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    pub struct ResultCacheKey {
        pub server_id: String,
        pub tool_name: String,
        pub input_fingerprint: String,
        pub schema_fingerprint: String,
        pub protocol_version: String,
        pub server_version: Option<String>,
    }

    impl ResultCacheKey {
        pub fn build(
            server_id: impl Into<String>,
            tool_name: impl Into<String>,
            input: &Value,
            schema_fingerprint: impl Into<String>,
            protocol_version: impl Into<String>,
            server_version: Option<String>,
        ) -> Result<Self, &'static str> {
            let server_id = server_id.into();
            let tool_name = tool_name.into();
            let schema_fingerprint = schema_fingerprint.into();
            let protocol_version = protocol_version.into();
            if server_id.trim().is_empty() || tool_name.trim().is_empty() {
                return Err("MCP cache server and tool names cannot be empty");
            }
            if schema_fingerprint.trim().is_empty() || protocol_version.trim().is_empty() {
                return Err("MCP cache schema and protocol versions cannot be empty");
            }
            let input_fingerprint = fingerprint(
                &serde_json::to_vec(input).map_err(|_| "MCP cache input serialization failed")?,
            );
            Ok(Self {
                server_id,
                tool_name,
                input_fingerprint,
                schema_fingerprint,
                protocol_version,
                server_version,
            })
        }

        #[must_use]
        pub fn fingerprint(&self) -> String {
            fingerprint(&serde_json::to_vec(self).unwrap_or_default())
        }
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    pub struct ResultCacheEntry {
        pub value: Value,
        #[serde(with = "time::serde::rfc3339")]
        pub stored_at: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339")]
        pub expires_at: OffsetDateTime,
        pub sensitive: bool,
        pub cacheable: bool,
    }

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
    pub struct ResultCache {
        entries: BTreeMap<ResultCacheKey, ResultCacheEntry>,
    }

    impl ResultCache {
        pub fn get(&mut self, key: &ResultCacheKey, now: OffsetDateTime) -> Option<Value> {
            let entry = self.entries.get(key)?;
            if now >= entry.expires_at {
                self.entries.remove(key);
                return None;
            }
            Some(entry.value.clone())
        }

        pub fn insert(
            &mut self,
            key: ResultCacheKey,
            value: Value,
            now: OffsetDateTime,
            ttl: Duration,
            sensitive: bool,
            cacheable: bool,
        ) -> Result<bool, &'static str> {
            if ttl <= Duration::ZERO {
                return Err("MCP result cache ttl must be positive");
            }
            if sensitive || !cacheable {
                self.entries.remove(&key);
                return Ok(false);
            }
            self.entries.insert(
                key,
                ResultCacheEntry {
                    value,
                    stored_at: now,
                    expires_at: now + ttl,
                    sensitive,
                    cacheable,
                },
            );
            Ok(true)
        }

        pub fn invalidate_server(&mut self, server_id: &str) -> usize {
            let before = self.entries.len();
            self.entries.retain(|key, _| key.server_id != server_id);
            before - self.entries.len()
        }

        pub fn invalidate_schema(&mut self, schema_fingerprint: &str) -> usize {
            let before = self.entries.len();
            self.entries
                .retain(|key, _| key.schema_fingerprint != schema_fingerprint);
            before - self.entries.len()
        }

        pub fn prune_expired(&mut self, now: OffsetDateTime) -> usize {
            let before = self.entries.len();
            self.entries.retain(|_, entry| now < entry.expires_at);
            before - self.entries.len()
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
                "github",
                "search",
                &input,
                schema,
                version,
                Some("1".into()),
            )
            .expect("key")
        }

        #[test]
        fn identical_requests_hit_and_changed_scope_misses() {
            let now = datetime!(2026-07-26 08:00 UTC);
            let mut cache = ResultCache::default();
            let original = key(json!({"q":"rust"}), "schema-a", "2026-01");
            cache
                .insert(
                    original.clone(),
                    json!({"items":[1]}),
                    now,
                    Duration::minutes(5),
                    false,
                    true,
                )
                .expect("insert");
            assert_eq!(cache.get(&original, now), Some(json!({"items":[1]})));
            assert_eq!(
                cache.get(&key(json!({"q":"go"}), "schema-a", "2026-01"), now),
                None
            );
            assert_eq!(
                cache.get(&key(json!({"q":"rust"}), "schema-b", "2026-01"), now),
                None
            );
            assert_eq!(
                cache.get(&key(json!({"q":"rust"}), "schema-a", "2026-02"), now),
                None
            );
        }

        #[test]
        fn sensitive_and_non_cacheable_results_are_not_stored() {
            let now = datetime!(2026-07-26 08:00 UTC);
            let mut cache = ResultCache::default();
            let sensitive = key(json!({"q":"secret"}), "schema", "1");
            assert!(
                !cache
                    .insert(
                        sensitive.clone(),
                        json!({"token":"secret"}),
                        now,
                        Duration::minutes(5),
                        true,
                        true,
                    )
                    .expect("sensitive")
            );
            assert_eq!(cache.get(&sensitive, now), None);
            let non_cacheable = key(json!({"q":"live"}), "schema", "1");
            assert!(
                !cache
                    .insert(
                        non_cacheable.clone(),
                        json!({"value":1}),
                        now,
                        Duration::minutes(5),
                        false,
                        false,
                    )
                    .expect("non-cacheable")
            );
            assert_eq!(cache.get(&non_cacheable, now), None);
        }

        #[test]
        fn invalidate_schema_removes_only_matching_schema_entries() {
            let now = datetime!(2026-07-26 08:00 UTC);
            let mut cache = ResultCache::default();
            let schema_a = key(json!({"q":"rust"}), "schema-a", "1");
            let schema_b = key(json!({"q":"rust"}), "schema-b", "1");
            for cache_key in [schema_a.clone(), schema_b.clone()] {
                cache
                    .insert(
                        cache_key,
                        json!({"items":[1]}),
                        now,
                        Duration::minutes(5),
                        false,
                        true,
                    )
                    .expect("insert");
            }

            assert_eq!(cache.invalidate_schema("schema-a"), 1);
            assert_eq!(cache.get(&schema_a, now), None);
            assert_eq!(cache.get(&schema_b, now), Some(json!({"items":[1]})));
        }
    }
}
