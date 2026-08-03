use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{ContractError, Result, SCHEMA_VERSION};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStage {
    ProfileSaved,
    SecretPresent,
    EndpointReachable,
    Authenticated,
    CapabilityAvailable,
    LiveRequestVerified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReadinessCheck {
    pub stage: ReadinessStage,
    pub ready: bool,
    pub checked_at: OffsetDateTime,
    pub reason: Option<String>,
}

impl ReadinessCheck {
    #[must_use]
    pub fn ready(stage: ReadinessStage) -> Self {
        Self {
            stage,
            ready: true,
            checked_at: OffsetDateTime::now_utc(),
            reason: None,
        }
    }

    #[must_use]
    pub fn unavailable(stage: ReadinessStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            ready: false,
            checked_at: OffsetDateTime::now_utc(),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteIdentity {
    pub route_id: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub endpoint_origin: String,
    pub auth_source: String,
}

impl RouteIdentity {
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("route_id", &self.route_id),
            ("provider", &self.provider),
            ("model", &self.model),
            ("protocol", &self.protocol),
            ("endpoint_origin", &self.endpoint_origin),
            ("auth_source", &self.auth_source),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::Validation(format!(
                    "route identity field {field} cannot be empty"
                )));
            }
        }
        if self.endpoint_origin.contains('@') {
            return Err(ContractError::Validation(
                "endpoint origin cannot contain embedded credentials".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilitySet {
    pub image_input: bool,
    pub tool_calling: bool,
    pub streaming_text: bool,
    pub streaming_audio: bool,
    pub cancellation: bool,
    pub supported_image_media_types: Vec<String>,
    pub max_image_bytes: Option<u64>,
    pub max_images_per_request: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteReadiness {
    pub schema_version: u16,
    pub identity: RouteIdentity,
    pub capabilities: ProviderCapabilitySet,
    pub checks: Vec<ReadinessCheck>,
}

impl RouteReadiness {
    pub fn new(
        identity: RouteIdentity,
        capabilities: ProviderCapabilitySet,
        checks: Vec<ReadinessCheck>,
    ) -> Result<Self> {
        let report = Self {
            schema_version: SCHEMA_VERSION,
            identity,
            capabilities,
            checks,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::Validation(
                "route readiness schema is unsupported".to_owned(),
            ));
        }
        self.identity.validate()?;
        let mut previous = None;
        for check in &self.checks {
            if previous.is_some_and(|stage| stage >= check.stage) {
                return Err(ContractError::Validation(
                    "readiness checks must be strictly ordered and unique".to_owned(),
                ));
            }
            if !check.ready && check.reason.as_deref().is_none_or(str::is_empty) {
                return Err(ContractError::Validation(
                    "unavailable readiness checks require an actionable reason".to_owned(),
                ));
            }
            previous = Some(check.stage);
        }
        if self.capabilities.streaming_text
            && !self.stage_ready(ReadinessStage::CapabilityAvailable)
        {
            return Err(ContractError::Validation(
                "streaming cannot be advertised before capability availability is verified"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn stage_ready(&self, stage: ReadinessStage) -> bool {
        self.checks
            .iter()
            .find(|check| check.stage == stage)
            .is_some_and(|check| check.ready)
    }

    #[must_use]
    pub fn ready_for_requests(&self) -> bool {
        self.stage_ready(ReadinessStage::Authenticated)
            && self.stage_ready(ReadinessStage::CapabilityAvailable)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    NotRequested,
    Requested,
    TransportInterrupted,
    ProviderAcknowledged,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancellationReceipt {
    pub state: CancellationState,
    pub requested_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
    pub bounded_within_ms: Option<u64>,
}

impl CancellationReceipt {
    pub fn validate(&self) -> Result<()> {
        if self.state != CancellationState::NotRequested && self.requested_at.is_none() {
            return Err(ContractError::Validation(
                "requested cancellation requires a request timestamp".to_owned(),
            ));
        }
        if matches!(
            self.state,
            CancellationState::TransportInterrupted | CancellationState::ProviderAcknowledged
        ) && self.completed_at.is_none()
        {
            return Err(ContractError::Validation(
                "completed cancellation requires a completion timestamp".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> RouteIdentity {
        RouteIdentity {
            route_id: "primary".to_owned(),
            provider: "minimax".to_owned(),
            model: "MiniMax-M3".to_owned(),
            protocol: "anthropic".to_owned(),
            endpoint_origin: "https://api.minimax.io".to_owned(),
            auth_source: "api-key".to_owned(),
        }
    }

    #[test]
    fn streaming_requires_verified_capability() {
        let result = RouteReadiness::new(
            identity(),
            ProviderCapabilitySet {
                streaming_text: true,
                ..ProviderCapabilitySet::default()
            },
            vec![ReadinessCheck::ready(ReadinessStage::ProfileSaved)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn saved_profile_is_not_authenticated_readiness() {
        let report = RouteReadiness::new(
            identity(),
            ProviderCapabilitySet::default(),
            vec![
                ReadinessCheck::ready(ReadinessStage::ProfileSaved),
                ReadinessCheck::ready(ReadinessStage::SecretPresent),
                ReadinessCheck::unavailable(
                    ReadinessStage::EndpointReachable,
                    "gateway is not running",
                ),
            ],
        )
        .unwrap();
        assert!(!report.ready_for_requests());
    }

    #[test]
    fn cancellation_completion_requires_evidence() {
        let receipt = CancellationReceipt {
            state: CancellationState::TransportInterrupted,
            requested_at: Some(OffsetDateTime::now_utc()),
            completed_at: None,
            bounded_within_ms: Some(100),
        };
        assert!(receipt.validate().is_err());
    }
}
