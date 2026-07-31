use medusa_runtime::{
    openai_realtime::{
        OpenAiRealtimeRoute, OpenAiRealtimeSessionCredential, resolve_openai_realtime_route,
    },
    voice::VoiceAvailability,
};
use serde::Serialize;

use crate::config::active_config;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRealtimeCapability {
    pub available: bool,
    pub reason: Option<String>,
    pub supports_input_audio: bool,
    pub supports_output_audio: bool,
    pub supports_barge_in: bool,
}

impl From<VoiceAvailability> for DesktopRealtimeCapability {
    fn from(availability: VoiceAvailability) -> Self {
        Self {
            available: availability.available,
            reason: availability.reason,
            supports_input_audio: availability.supports_input_audio,
            supports_output_audio: availability.supports_output_audio,
            supports_barge_in: availability.supports_barge_in,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRealtimeSession {
    authorization_token: String,
    expires_at: u64,
    model: String,
    webrtc_call_url: String,
}

impl From<OpenAiRealtimeSessionCredential> for DesktopRealtimeSession {
    fn from(credential: OpenAiRealtimeSessionCredential) -> Self {
        Self {
            authorization_token: credential.authorization_token().to_owned(),
            expires_at: credential.expires_at(),
            model: credential.model().to_owned(),
            webrtc_call_url: credential.webrtc_call_url().to_owned(),
        }
    }
}

#[tauri::command]
pub fn desktop_realtime_capability() -> Result<DesktopRealtimeCapability, String> {
    let config = active_config()?;
    Ok(resolve_openai_realtime_route(&config).availability().into())
}

#[tauri::command]
pub fn desktop_establish_realtime_session() -> Result<DesktopRealtimeSession, String> {
    let config = active_config()?;
    let route = resolve_openai_realtime_route(&config);
    establish_route(route)
}

fn establish_route(route: OpenAiRealtimeRoute) -> Result<DesktopRealtimeSession, String> {
    route
        .establish_session()
        .map(DesktopRealtimeSession::from)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_response_preserves_fail_closed_reason() {
        let capability: DesktopRealtimeCapability =
            VoiceAvailability::unavailable("the existing account does not expose Realtime").into();
        assert!(!capability.available);
        assert_eq!(
            capability.reason.as_deref(),
            Some("the existing account does not expose Realtime")
        );
        assert!(!capability.supports_input_audio);
        assert!(!capability.supports_output_audio);
        assert!(!capability.supports_barge_in);
    }

    #[test]
    fn unsupported_route_cannot_mint_a_frontend_session() {
        let error = match establish_route(OpenAiRealtimeRoute::ExistingRouteUnsupported {
            provider: "local".to_owned(),
        }) {
            Ok(_) => panic!("unsupported route must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("Realtime unavailable"));
        assert!(error.contains("configured provider `local`"));
    }
}
