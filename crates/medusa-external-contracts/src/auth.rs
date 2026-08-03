use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AttemptId, ContractError, RequestDigest, Result, SCHEMA_VERSION, TrustedHost,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMethod {
    None,
    ApiKey,
    OAuthDevice,
    OAuthBrowser,
    ManagedSession,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthStage {
    ProfileSaved,
    AuthorizationStarted,
    RedirectReceived,
    CodeExchanged,
    CredentialPersisted,
    RefreshVerified,
    Revoked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    NotPresent,
    Active,
    Expired,
    RefreshRequired,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PinnedOAuthComponent {
    pub name: String,
    pub version: String,
    pub source_digest: RequestDigest,
}

impl PinnedOAuthComponent {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(ContractError::Validation(
                "OAuth component name and version cannot be empty".to_owned(),
            ));
        }
        let version = self.version.to_ascii_lowercase();
        if version.contains("latest") || version.contains('*') || version == "next" {
            return Err(ContractError::Validation(
                "OAuth runtime components must be pinned to an immutable version".to_owned(),
            ));
        }
        RequestDigest::parse(self.source_digest.to_string())?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OAuthStageReceipt {
    pub stage: OAuthStage,
    pub completed_at: OffsetDateTime,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OAuthLifecycleReceipt {
    pub schema_version: u16,
    pub flow_id: AttemptId,
    pub provider: String,
    pub method: AuthenticationMethod,
    pub authorization_host: TrustedHost,
    pub redirect_host: TrustedHost,
    pub state_digest: RequestDigest,
    pub pkce_challenge_digest: RequestDigest,
    pub component: PinnedOAuthComponent,
    pub stages: Vec<OAuthStageReceipt>,
    pub credential_state: CredentialState,
    pub credential_backend: String,
    pub expires_at: Option<OffsetDateTime>,
    pub redacted: bool,
}

impl OAuthLifecycleReceipt {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::Validation(
                "OAuth lifecycle schema is unsupported".to_owned(),
            ));
        }
        AttemptId::parse(self.flow_id.to_string())?;
        if self.provider.trim().is_empty() || self.credential_backend.trim().is_empty() {
            return Err(ContractError::Validation(
                "OAuth lifecycle requires provider and credential backend identity".to_owned(),
            ));
        }
        if !matches!(
            self.method,
            AuthenticationMethod::OAuthBrowser | AuthenticationMethod::OAuthDevice
        ) {
            return Err(ContractError::Validation(
                "OAuth lifecycle receipt requires an OAuth authentication method".to_owned(),
            ));
        }
        RequestDigest::parse(self.state_digest.to_string())?;
        RequestDigest::parse(self.pkce_challenge_digest.to_string())?;
        self.component.validate()?;
        if !self.redacted {
            return Err(ContractError::Validation(
                "durable OAuth lifecycle receipts must be redacted".to_owned(),
            ));
        }
        let mut previous = None;
        for stage in &self.stages {
            if previous.is_some_and(|value| value >= stage.stage) {
                return Err(ContractError::Validation(
                    "OAuth stages must be strictly ordered and unique".to_owned(),
                ));
            }
            previous = Some(stage.stage);
        }
        if matches!(
            self.credential_state,
            CredentialState::Active
                | CredentialState::Expired
                | CredentialState::RefreshRequired
                | CredentialState::Revoked
        ) && !self.has_stage(OAuthStage::CredentialPersisted)
        {
            return Err(ContractError::Validation(
                "credential state requires a persisted credential stage".to_owned(),
            ));
        }
        if self.credential_state == CredentialState::Active
            && !self.has_stage(OAuthStage::CodeExchanged)
        {
            return Err(ContractError::Validation(
                "active OAuth credentials require a completed code exchange".to_owned(),
            ));
        }
        if self.credential_state == CredentialState::Revoked
            && !self.has_stage(OAuthStage::Revoked)
        {
            return Err(ContractError::Validation(
                "revoked credential state requires revocation evidence".to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn has_stage(&self, expected: OAuthStage) -> bool {
        self.stages.iter().any(|stage| stage.stage == expected)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn digest(value: &str) -> RequestDigest {
        RequestDigest::from_canonical(&json!({"value": value})).unwrap()
    }

    fn receipt() -> OAuthLifecycleReceipt {
        OAuthLifecycleReceipt {
            schema_version: SCHEMA_VERSION,
            flow_id: AttemptId::new(),
            provider: "openai".to_owned(),
            method: AuthenticationMethod::OAuthBrowser,
            authorization_host: TrustedHost::parse("https://auth.openai.com").unwrap(),
            redirect_host: TrustedHost::parse("http://127.0.0.1:1455").unwrap(),
            state_digest: digest("state"),
            pkce_challenge_digest: digest("pkce"),
            component: PinnedOAuthComponent {
                name: "medusa-openai-oauth".to_owned(),
                version: "1.4.2".to_owned(),
                source_digest: digest("component"),
            },
            stages: vec![
                OAuthStageReceipt {
                    stage: OAuthStage::ProfileSaved,
                    completed_at: OffsetDateTime::now_utc(),
                    request_id: None,
                },
                OAuthStageReceipt {
                    stage: OAuthStage::AuthorizationStarted,
                    completed_at: OffsetDateTime::now_utc(),
                    request_id: Some("request-1".to_owned()),
                },
                OAuthStageReceipt {
                    stage: OAuthStage::RedirectReceived,
                    completed_at: OffsetDateTime::now_utc(),
                    request_id: Some("request-1".to_owned()),
                },
                OAuthStageReceipt {
                    stage: OAuthStage::CodeExchanged,
                    completed_at: OffsetDateTime::now_utc(),
                    request_id: Some("request-1".to_owned()),
                },
                OAuthStageReceipt {
                    stage: OAuthStage::CredentialPersisted,
                    completed_at: OffsetDateTime::now_utc(),
                    request_id: Some("request-1".to_owned()),
                },
            ],
            credential_state: CredentialState::Active,
            credential_backend: "os-keychain".to_owned(),
            expires_at: Some(OffsetDateTime::now_utc()),
            redacted: true,
        }
    }

    #[test]
    fn pinned_redacted_lifecycle_is_valid() {
        receipt().validate().unwrap();
    }

    #[test]
    fn mutable_latest_component_is_rejected() {
        let mut receipt = receipt();
        receipt.component.version = "latest".to_owned();
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn active_state_requires_exchange_and_persistence() {
        let mut receipt = receipt();
        receipt
            .stages
            .retain(|stage| stage.stage != OAuthStage::CodeExchanged);
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn revocation_requires_explicit_evidence() {
        let mut receipt = receipt();
        receipt.credential_state = CredentialState::Revoked;
        assert!(receipt.validate().is_err());
    }
}
