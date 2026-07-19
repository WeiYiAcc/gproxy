//! Optional per-credential usage query for custom OpenAI-compatible providers.
//! Disabled unless `settings_json.usage_enabled` is true so arbitrary custom
//! upstreams are never probed for a billing endpoint. Slate field names here are
//! based on its shipped CLI binary: sandbox requests were blocked by Cloudflare
//! and the successful response shape still needs verification in a real network.

use bytes::Bytes;
use http::header::{ACCEPT, HeaderValue};
use http::{HeaderMap, Method, Request, StatusCode};
use serde::Deserialize;
use serde_json::Value;

use super::auth;
use crate::channel::ChannelError;
use crate::channel::http_util::{build_request, join_url};
use crate::channel::usage::{UsageCredits, UsageSnapshot, UsageWindow};

const DEFAULT_USAGE_BASE_URL: &str = "https://api.randomlabs.ai";
const DEFAULT_USAGE_PATH: &str = "/billing/usage";

pub(super) fn request(
    secret: &Value,
    settings: &Value,
) -> Result<Option<Request<Bytes>>, ChannelError> {
    if settings.get("usage_enabled").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }

    let access_token = super::oauth::access_token(secret);
    let api_key = secret
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if access_token.is_none() && api_key.is_none() {
        return Err(ChannelError::InvalidCredential(
            "missing access_token or api_key".into(),
        ));
    }
    let base_url = string_setting(settings, "usage_base_url", DEFAULT_USAGE_BASE_URL);
    let path = string_setting(settings, "usage_path", DEFAULT_USAGE_PATH);
    let uri = join_url(base_url, path, None)?;
    let mut request = build_request(Method::GET, uri, HeaderMap::new(), Bytes::new())?;
    if let Some(access_token) = access_token {
        crate::channel::bulletins::common::inject_bearer(&mut request, access_token)?;
    } else if let Some(api_key) = api_key {
        auth::apply(
            &mut request,
            api_key,
            auth::configured(settings)?,
            auth::Proto::OpenAi,
        )?;
    }
    request
        .headers_mut()
        .insert(ACCEPT, HeaderValue::from_static("application/json"));
    Ok(Some(request))
}

pub(super) fn parse(status: StatusCode, body: &Bytes) -> Option<UsageSnapshot> {
    if !status.is_success() {
        return None;
    }
    let raw: Value = serde_json::from_slice(body).ok()?;
    let payload: UsagePayload = serde_json::from_value(raw.clone()).ok()?;
    let payload = payload.state.as_deref().unwrap_or(&payload);
    if payload.credits.is_none() && payload.subscription.is_none() && payload.plan.is_none() {
        return None;
    }

    let credits = payload.credits.as_ref().map(|credits| {
        let used = credits
            .used
            .or_else(|| match (credits.total, credits.available) {
                (Some(total), Some(available)) => Some((total - available).max(0.0)),
                _ => None,
            });
        UsageCredits {
            has_credits: Some(credits.available.unwrap_or(0.0) > 0.0),
            balance: credits.available.map(format_number),
            available_credits: credits.available,
            used_credits: used,
            monthly_limit: credits.total,
            ..Default::default()
        }
    });

    let mut windows = Vec::new();
    if let Some(subscription) = &payload.subscription {
        if subscription.usage.is_some() || subscription.allowance.is_some() {
            let mut window = UsageWindow {
                name: "subscription".into(),
                used: subscription.usage,
                limit: subscription.allowance,
                ..Default::default()
            };
            if let (Some(start), Some(duration)) =
                (subscription.period_start, subscription.period_duration_secs)
            {
                window.resets_at_unix = Some(start.saturating_add(duration));
                window.window_seconds = Some(duration);
            }
            if let Some(reset) = subscription.reset_at {
                window.resets_at_unix = Some(reset);
            }
            windows.push(window);
        }
    }

    Some(UsageSnapshot {
        plan: payload
            .plan
            .clone()
            .or_else(|| payload.subscription.as_ref().and_then(|s| s.plan.clone()))
            .filter(|plan| !plan.is_empty()),
        windows,
        credits,
        rate_limit_reset_credits: None,
        raw,
    })
}

fn string_setting<'a>(settings: &'a Value, key: &str, default: &'a str) -> &'a str {
    settings
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[derive(Deserialize)]
struct UsagePayload {
    #[serde(default)]
    state: Option<Box<UsagePayload>>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    credits: Option<CreditsPayload>,
    #[serde(default)]
    subscription: Option<SubscriptionPayload>,
}

#[derive(Deserialize)]
struct CreditsPayload {
    #[serde(default)]
    total: Option<f64>,
    #[serde(default)]
    available: Option<f64>,
    #[serde(default)]
    used: Option<f64>,
}

#[derive(Deserialize)]
struct SubscriptionPayload {
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    usage: Option<f64>,
    #[serde(default, alias = "limit", alias = "quota")]
    allowance: Option<f64>,
    #[serde(default, alias = "periodStart")]
    period_start: Option<i64>,
    #[serde(default, alias = "periodDurationSecs")]
    period_duration_secs: Option<i64>,
    #[serde(default, alias = "resetAt")]
    reset_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::AUTHORIZATION;
    use serde_json::json;

    #[test]
    fn usage_is_disabled_by_default() {
        assert!(
            request(&json!({"api_key":"key"}), &json!({}))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn request_uses_configured_host_path_and_authentication() {
        let request = request(
            &json!({"api_key":"key"}),
            &json!({
                "usage_enabled": true,
                "usage_base_url": "https://quota.example/prefix",
                "usage_path": "/account/quota",
                "api_key_header": "x-api-key"
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.uri(), "https://quota.example/prefix/account/quota");
        assert_eq!(request.headers().get("x-api-key").unwrap(), "key");
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn oauth_access_token_uses_bearer_even_with_api_key_override() {
        let request = request(
            &json!({"access_token":"oauth-token","refresh_token":"refresh"}),
            &json!({"usage_enabled":true,"api_key_header":"x-api-key"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            request.headers().get(AUTHORIZATION).unwrap(),
            "Bearer oauth-token"
        );
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn request_defaults_to_random_labs_endpoint_and_bearer_auth() {
        let request = request(&json!({"api_key":"key"}), &json!({"usage_enabled":true}))
            .unwrap()
            .unwrap();
        assert_eq!(request.uri(), "https://api.randomlabs.ai/billing/usage");
        assert_eq!(request.headers().get(AUTHORIZATION).unwrap(), "Bearer key");
    }

    #[test]
    fn parses_credits_and_subscription_window() {
        let snapshot = parse(
            StatusCode::OK,
            &Bytes::from_static(
                br#"{"credits":{"total":1000,"available":625},"subscription":{"usage":20,"allowance":100,"plan":"pro","periodStart":1700000000,"periodDurationSecs":604800}}"#,
            ),
        )
        .unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("pro"));
        let credits = snapshot.credits.unwrap();
        assert_eq!(credits.available_credits, Some(625.0));
        assert_eq!(credits.used_credits, Some(375.0));
        assert_eq!(credits.monthly_limit, Some(1000.0));
        assert_eq!(snapshot.windows[0].used, Some(20.0));
        assert_eq!(snapshot.windows[0].limit, Some(100.0));
        assert_eq!(snapshot.windows[0].resets_at_unix, Some(1700604800));
    }

    #[test]
    fn rejects_non_success_and_invalid_payloads() {
        assert!(parse(StatusCode::UNAUTHORIZED, &Bytes::from_static(b"{}")).is_none());
        assert!(
            parse(
                StatusCode::FORBIDDEN,
                &Bytes::from_static(br#"{"cloudflare_error":true,"ray_id":"redacted"}"#),
            )
            .is_none()
        );
        assert!(parse(StatusCode::OK, &Bytes::from_static(b"not json")).is_none());
    }
}
