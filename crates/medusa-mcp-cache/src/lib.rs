//! Durable MCP schema cache and deterministic lazy connection planning.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ServerId(String);

impl ServerId {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("server id cannot be empty");
        }
        if trimmed.len() > 128 {
            return Err("server id is too long");
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Stdio,
    Http,
    Sse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerConfig {
    pub id: ServerId,
    pub transport: TransportKind,
    pub endpoint: String,
    #[serde(default)]
    pub startup_arguments: Vec<String>,
    #[serde(default)]
    pub environment_keys: BTreeSet<String>,
}

impl ServerConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.endpoint.trim().is_empty() {
            return Err("server endpoint cannot be empty");
        }
        if self
            .startup_arguments
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("startup arguments cannot contain empty values");
        }
        if self
            .environment_keys
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("environment keys cannot contain empty values");
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolSchema {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

impl ToolSchema {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("tool name cannot be empty");
        }
        if !self.input_schema.is_object() {
            return Err("tool input schema must be a JSON object");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SchemaSnapshot {
    pub server_id: ServerId,
    pub server_config_fingerprint: String,
    pub protocol_version: String,
    pub server_version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub tools: Vec<ToolSchema>,
    pub schema_fingerprint: String,
}

impl SchemaSnapshot {
    pub fn build(
        config: &ServerConfig,
        protocol_version: impl Into<String>,
        server_version: Option<String>,
        fetched_at: OffsetDateTime,
        ttl: Duration,
        mut tools: Vec<ToolSchema>,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        let protocol_version = protocol_version.into();
        if protocol_version.trim().is_empty() {
            return Err("protocol version cannot be empty");
        }
        if ttl <= Duration::ZERO {
            return Err("schema ttl must be positive");
        }
        for tool in &tools {
            tool.validate()?;
        }
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        if tools.windows(2).any(|items| items[0].name == items[1].name) {
            return Err("tool names must be unique per server");
        }
        let schema_fingerprint = fingerprint_tools(&tools);
        Ok(Self {
            server_id: config.id.clone(),
            server_config_fingerprint: config.fingerprint(),
            protocol_version,
            server_version,
            fetched_at,
            expires_at: fetched_at + ttl,
            tools,
            schema_fingerprint,
        })
    }

    #[must_use]
    pub fn is_fresh_for(&self, config: &ServerConfig, now: OffsetDateTime) -> bool {
        self.server_id == config.id
            && self.server_config_fingerprint == config.fingerprint()
            && now < self.expires_at
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol_version.trim().is_empty() {
            return Err("protocol version cannot be empty");
        }
        if self.expires_at <= self.fetched_at {
            return Err("schema snapshot expiry must follow fetch time");
        }
        for tool in &self.tools {
            tool.validate()?;
        }
        if self
            .tools
            .windows(2)
            .any(|items| items[0].name >= items[1].name)
        {
            return Err("tool schemas must be uniquely sorted by name");
        }
        if self.schema_fingerprint != fingerprint_tools(&self.tools) {
            return Err("schema fingerprint does not match tool schemas");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConnectionRecord {
    pub state: ConnectionState,
    pub generation: u32,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Default for ConnectionRecord {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            generation: 0,
            last_used_at: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPlan {
    UseCachedSchema,
    ConnectAndRefreshSchema,
    UseExistingConnection,
    ReconnectAndRefreshSchema,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ServerEntry {
    pub config: ServerConfig,
    pub connection: ConnectionRecord,
    pub schema: Option<SchemaSnapshot>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct McpManager {
    #[serde(default)]
    servers: BTreeMap<ServerId, ServerEntry>,
}

impl McpManager {
    pub fn register(&mut self, config: ServerConfig) -> Result<(), &'static str> {
        config.validate()?;
        if self.servers.contains_key(&config.id) {
            return Err("MCP server id is already registered");
        }
        self.servers.insert(
            config.id.clone(),
            ServerEntry {
                config,
                connection: ConnectionRecord::default(),
                schema: None,
            },
        );
        Ok(())
    }

    pub fn store_schema(&mut self, snapshot: SchemaSnapshot) -> Result<(), &'static str> {
        snapshot.validate()?;
        let entry = self
            .servers
            .get_mut(&snapshot.server_id)
            .ok_or("schema references an unregistered MCP server")?;
        if snapshot.server_config_fingerprint != entry.config.fingerprint() {
            return Err("schema was fetched for a different server configuration");
        }
        entry.schema = Some(snapshot);
        Ok(())
    }

    pub fn plan_access(
        &self,
        server_id: &ServerId,
        requires_live_connection: bool,
        now: OffsetDateTime,
    ) -> Result<AccessPlan, &'static str> {
        let entry = self
            .servers
            .get(server_id)
            .ok_or("MCP server is not registered")?;
        let fresh_schema = entry
            .schema
            .as_ref()
            .is_some_and(|schema| schema.is_fresh_for(&entry.config, now));
        let connected = entry.connection.state == ConnectionState::Ready;

        Ok(match (requires_live_connection, connected, fresh_schema) {
            (false, _, true) => AccessPlan::UseCachedSchema,
            (false, true, false) | (true, true, true) => AccessPlan::UseExistingConnection,
            (true, false, true) => AccessPlan::ConnectAndRefreshSchema,
            (_, false, false) => AccessPlan::ConnectAndRefreshSchema,
            (_, true, false) => AccessPlan::ReconnectAndRefreshSchema,
        })
    }

    pub fn mark_connecting(&mut self, server_id: &ServerId) -> Result<u32, &'static str> {
        let entry = self
            .servers
            .get_mut(server_id)
            .ok_or("MCP server is not registered")?;
        if entry.connection.state == ConnectionState::Connecting {
            return Err("MCP server is already connecting");
        }
        entry.connection.generation = entry.connection.generation.saturating_add(1);
        entry.connection.state = ConnectionState::Connecting;
        entry.connection.last_error = None;
        Ok(entry.connection.generation)
    }

    pub fn mark_ready(
        &mut self,
        server_id: &ServerId,
        generation: u32,
        now: OffsetDateTime,
    ) -> Result<(), &'static str> {
        let entry = self
            .servers
            .get_mut(server_id)
            .ok_or("MCP server is not registered")?;
        if entry.connection.state != ConnectionState::Connecting
            || entry.connection.generation != generation
        {
            return Err("stale or invalid MCP connection generation");
        }
        entry.connection.state = ConnectionState::Ready;
        entry.connection.last_used_at = Some(now);
        Ok(())
    }

    pub fn mark_failed(
        &mut self,
        server_id: &ServerId,
        generation: u32,
        error: impl Into<String>,
    ) -> Result<(), &'static str> {
        let error = error.into();
        if error.trim().is_empty() {
            return Err("connection failure must include an error");
        }
        let entry = self
            .servers
            .get_mut(server_id)
            .ok_or("MCP server is not registered")?;
        if entry.connection.generation != generation {
            return Err("stale MCP connection generation");
        }
        entry.connection.state = ConnectionState::Failed;
        entry.connection.last_error = Some(error);
        Ok(())
    }

    pub fn disconnect_idle(
        &mut self,
        now: OffsetDateTime,
        idle_timeout: Duration,
    ) -> Result<Vec<ServerId>, &'static str> {
        if idle_timeout <= Duration::ZERO {
            return Err("idle timeout must be positive");
        }
        let mut disconnected = Vec::new();
        for (id, entry) in &mut self.servers {
            if entry.connection.state == ConnectionState::Ready
                && entry
                    .connection
                    .last_used_at
                    .is_some_and(|last| now - last >= idle_timeout)
            {
                entry.connection.state = ConnectionState::Disconnected;
                disconnected.push(id.clone());
            }
        }
        Ok(disconnected)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        for (id, entry) in &self.servers {
            if id != &entry.config.id {
                return Err("MCP registry key does not match server config id");
            }
            entry.config.validate()?;
            if let Some(schema) = &entry.schema {
                schema.validate()?;
                if schema.server_id != *id {
                    return Err("schema server id does not match registry entry");
                }
            }
            if entry.connection.state == ConnectionState::Failed
                && entry
                    .connection
                    .last_error
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err("failed MCP connection must retain an error");
            }
        }
        Ok(())
    }
}

fn fingerprint_tools(tools: &[ToolSchema]) -> String {
    let bytes = serde_json::to_vec(tools).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn config() -> ServerConfig {
        ServerConfig {
            id: ServerId::parse("github").expect("id"),
            transport: TransportKind::Stdio,
            endpoint: "mcp-github".to_owned(),
            startup_arguments: Vec::new(),
            environment_keys: BTreeSet::new(),
        }
    }

    fn schema(now: OffsetDateTime) -> SchemaSnapshot {
        SchemaSnapshot::build(
            &config(),
            "2025-11-25",
            Some("1.0.0".to_owned()),
            now,
            Duration::hours(1),
            vec![ToolSchema {
                name: "search".to_owned(),
                description: String::new(),
                input_schema: json!({"type":"object"}),
            }],
        )
        .expect("schema")
    }

    #[test]
    fn fresh_schema_avoids_connection_for_discovery() {
        let now = datetime!(2026-07-24 15:00 UTC);
        let mut manager = McpManager::default();
        manager.register(config()).expect("register");
        manager.store_schema(schema(now)).expect("store");
        assert_eq!(
            manager
                .plan_access(&ServerId::parse("github").expect("id"), false, now)
                .expect("plan"),
            AccessPlan::UseCachedSchema
        );
    }

    #[test]
    fn tool_invocation_connects_lazily() {
        let now = datetime!(2026-07-24 15:00 UTC);
        let mut manager = McpManager::default();
        manager.register(config()).expect("register");
        manager.store_schema(schema(now)).expect("store");
        assert_eq!(
            manager
                .plan_access(&ServerId::parse("github").expect("id"), true, now)
                .expect("plan"),
            AccessPlan::ConnectAndRefreshSchema
        );
    }

    #[test]
    fn config_change_invalidates_cached_schema() {
        let now = datetime!(2026-07-24 15:00 UTC);
        let mut manager = McpManager::default();
        manager.register(config()).expect("register");
        manager.store_schema(schema(now)).expect("store");
        manager
            .servers
            .get_mut(&ServerId::parse("github").expect("id"))
            .expect("entry")
            .config
            .endpoint = "new-command".to_owned();
        assert_eq!(
            manager
                .plan_access(&ServerId::parse("github").expect("id"), false, now)
                .expect("plan"),
            AccessPlan::ConnectAndRefreshSchema
        );
    }

    #[test]
    fn stale_generation_cannot_mark_connection_ready() {
        let mut manager = McpManager::default();
        manager.register(config()).expect("register");
        let generation = manager
            .mark_connecting(&ServerId::parse("github").expect("id"))
            .expect("connecting");
        assert_eq!(
            manager.mark_ready(
                &ServerId::parse("github").expect("id"),
                generation + 1,
                datetime!(2026-07-24 15:00 UTC),
            ),
            Err("stale or invalid MCP connection generation")
        );
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let tool = ToolSchema {
            name: "search".to_owned(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
        };
        assert_eq!(
            SchemaSnapshot::build(
                &config(),
                "2025-11-25",
                None,
                datetime!(2026-07-24 15:00 UTC),
                Duration::hours(1),
                vec![tool.clone(), tool],
            ),
            Err("tool names must be unique per server")
        );
    }
}
