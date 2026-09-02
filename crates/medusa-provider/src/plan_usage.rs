use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Provider-reported subscription/model-plan usage for one rolling window.
///
/// This is deliberately separate from generic HTTP rate-limit state: only provider-specific
/// plan-window headers are accepted so an RPM throttle cannot masquerade as subscription usage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderPlanUsage {
    pub provider: String,
    pub model: String,
    pub window_seconds: u64,
    /// Used portion of the plan window in basis points (`10_000 == 100%`).
    pub used_basis_points: u16,
    pub reset_at_unix: Option<i64>,
    pub observed_at_unix: i64,
    pub source: String,
}

impl ProviderPlanUsage {
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.used_basis_points >= 10_000
    }

    #[must_use]
    pub fn reset_after_seconds(&self, now_unix: i64) -> Option<u64> {
        self.reset_at_unix
            .and_then(|reset| u64::try_from(reset.saturating_sub(now_unix)).ok())
            .filter(|seconds| *seconds > 0)
    }
}

pub(crate) fn parse_provider_plan_usage(
    provider: &str,
    model: &str,
    headers: &HeaderMap,
) -> Option<ProviderPlanUsage> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.contains("anthropic") {
        parse_anthropic(provider, model, headers)
    } else if normalized.contains("openai") || normalized.contains("codex") {
        parse_openai(provider, model, headers)
    } else {
        None
    }
}

fn parse_openai(provider: &str, model: &str, headers: &HeaderMap) -> Option<ProviderPlanUsage> {
    let used = header_f64(headers, "x-codex-primary-used-percent")?;
    let window_minutes = header_u64(headers, "x-codex-primary-window-minutes").unwrap_or(300);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let reset_at_unix = header_i64(headers, "x-codex-primary-reset-at").or_else(|| {
        header_u64(headers, "x-codex-primary-reset-after-seconds")
            .and_then(|seconds| i64::try_from(seconds).ok())
            .map(|seconds| now.saturating_add(seconds))
    });
    Some(ProviderPlanUsage {
        provider: provider.to_owned(),
        model: model.to_owned(),
        window_seconds: window_minutes.saturating_mul(60),
        used_basis_points: percent_to_basis_points(used),
        reset_at_unix,
        observed_at_unix: now,
        source: "openai_codex_primary".to_owned(),
    })
}

fn parse_anthropic(
    provider: &str,
    model: &str,
    headers: &HeaderMap,
) -> Option<ProviderPlanUsage> {
    let utilization = header_f64(headers, "anthropic-ratelimit-unified-5h-utilization")?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let reset_at_unix = header_str(headers, "anthropic-ratelimit-unified-5h-reset")
        .and_then(parse_timestamp);
    Some(ProviderPlanUsage {
        provider: provider.to_owned(),
        model: model.to_owned(),
        window_seconds: 5 * 60 * 60,
        used_basis_points: fraction_to_basis_points(utilization),
        reset_at_unix,
        observed_at_unix: now,
        source: "anthropic_unified_5h".to_owned(),
    })
}

fn percent_to_basis_points(value: f64) -> u16 {
    scaled_basis_points(value, 100.0)
}

fn fraction_to_basis_points(value: f64) -> u16 {
    // Anthropic utilization is normally a 0..=1 ratio. Tolerate a percentage-shaped value from
    // compatible gateways without turning it into 100x usage.
    if value > 1.0 {
        percent_to_basis_points(value)
    } else {
        scaled_basis_points(value, 1.0)
    }
}

fn scaled_basis_points(value: f64, scale: f64) -> u16 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let basis_points = (value * 10_000.0 / scale).round().clamp(0.0, 10_000.0);
    basis_points as u16
}

fn parse_timestamp(value: &str) -> Option<i64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .or_else(|| OffsetDateTime::parse(value.trim(), &Rfc3339).ok().map(|value| value.unix_timestamp()))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_str(headers, name)?.parse().ok()
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    header_str(headers, name)?.parse().ok()
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    header_str(headers, name)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::*;

    #[test]
    fn parses_openai_codex_primary_window() {
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", HeaderValue::from_static("42.5"));
        headers.insert("x-codex-primary-window-minutes", HeaderValue::from_static("300"));
        headers.insert("x-codex-primary-reset-at", HeaderValue::from_static("2000000000"));
        let usage = parse_provider_plan_usage("openai_oauth", "gpt-test", &headers).expect("usage");
        assert_eq!(usage.window_seconds, 18_000);
        assert_eq!(usage.used_basis_points, 4_250);
        assert_eq!(usage.reset_at_unix, Some(2_000_000_000));
    }

    #[test]
    fn parses_anthropic_five_hour_window() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-ratelimit-unified-5h-utilization",
            HeaderValue::from_static("0.875"),
        );
        headers.insert(
            "anthropic-ratelimit-unified-5h-reset",
            HeaderValue::from_static("2033-05-18T03:33:20Z"),
        );
        let usage = parse_provider_plan_usage("anthropic", "claude-test", &headers).expect("usage");
        assert_eq!(usage.window_seconds, 18_000);
        assert_eq!(usage.used_basis_points, 8_750);
        assert_eq!(usage.reset_at_unix, Some(2_000_000_000));
    }

    #[test]
    fn ignores_generic_rate_limit_headers_for_unknown_provider() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("60"));
        assert!(parse_provider_plan_usage("other", "model", &headers).is_none());
    }
}
