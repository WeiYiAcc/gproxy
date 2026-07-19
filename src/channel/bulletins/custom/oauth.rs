//! Slate account OAuth for the custom channel. The one-time WorkOS device flow
//! yields a sealed `{access_token, refresh_token, expires_at_ms}` credential;
//! subsequent requests use the access token and the normal credential refresh
//! orchestrator rotates it without another user code.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::channel::{ChannelError, DeviceInit, DevicePoll};
use crate::http::client::UpstreamClient;

pub(super) const CLIENT_ID: &str = "client_01JCYX20PEAQN6NHBM1RM7ZTF4";
const DEVICE_URL: &str = "https://api.workos.com/user_management/authorize/device";
const TOKEN_URL: &str = "https://api.workos.com/user_management/authenticate";
const EXPIRY_SKEW_MS: i64 = 60_000;

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    #[serde(rename = "expires_in")]
    _expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

#[derive(Default, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

fn default_interval() -> u64 {
    5
}

pub(super) fn access_token(secret: &Value) -> Option<&str> {
    secret
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn refresh_token(secret: &Value) -> Option<&str> {
    secret
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn needs_refresh(secret: &Value) -> bool {
    if refresh_token(secret).is_none() {
        return false;
    }
    if access_token(secret).is_none() {
        return true;
    }
    let expires_at_ms = secret
        .get("expires_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    expires_at_ms != 0
        && crate::util::time::unix_now().saturating_mul(1000) > expires_at_ms - EXPIRY_SKEW_MS
}

pub(super) async fn device_start(
    client: &Arc<dyn UpstreamClient>,
) -> Result<DeviceInit, ChannelError> {
    let (status, body) = form_post(client, DEVICE_URL, &[("client_id", CLIENT_ID)]).await?;
    if !status.is_success() {
        return Err(status_error("slate device authorization", status, &body));
    }
    let response: DeviceResponse = serde_json::from_slice(&body)
        .map_err(|error| ChannelError::Build(format!("slate device response parse: {error}")))?;
    let verification_url = response
        .verification_uri_complete
        .unwrap_or(response.verification_uri);
    Ok(DeviceInit {
        device_code: response.device_code,
        user_code: response.user_code,
        verification_url,
        interval_secs: response.interval.max(1),
    })
}

pub(super) async fn device_poll(
    client: &Arc<dyn UpstreamClient>,
    device_code: &str,
) -> Result<DevicePoll, ChannelError> {
    let (status, body) = form_post(
        client,
        TOKEN_URL,
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", CLIENT_ID),
        ],
    )
    .await?;
    let response: TokenResponse = serde_json::from_slice(&body)
        .map_err(|error| ChannelError::Build(format!("slate device poll parse: {error}")))?;
    if status.is_success() {
        return Ok(DevicePoll::Ready(token_secret(
            Value::Object(Default::default()),
            response,
        )?));
    }
    match response.error.as_deref() {
        Some("authorization_pending") | Some("slow_down") => Ok(DevicePoll::Pending),
        Some("access_denied") | Some("expired_token") => Ok(DevicePoll::Denied),
        _ => Err(status_error("slate device poll", status, &body)),
    }
}

pub(super) async fn refresh(
    client: &Arc<dyn UpstreamClient>,
    secret: &Value,
) -> Result<Value, ChannelError> {
    let refresh_token = refresh_token(secret)
        .ok_or_else(|| ChannelError::InvalidCredential("missing refresh_token".into()))?;
    let (status, body) = form_post(
        client,
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ],
    )
    .await?;
    if !status.is_success() {
        return Err(status_error("slate token refresh", status, &body));
    }
    let response: TokenResponse = serde_json::from_slice(&body)
        .map_err(|error| ChannelError::Build(format!("slate refresh response parse: {error}")))?;
    token_secret(secret.clone(), response)
}

fn token_secret(mut secret: Value, response: TokenResponse) -> Result<Value, ChannelError> {
    let access_token = response
        .access_token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ChannelError::Build("slate token response missing access_token".into()))?;
    let expires_at_ms = response
        .expires_in
        .map(|seconds| crate::util::time::unix_now().saturating_mul(1000) + seconds as i64 * 1000)
        .or_else(|| jwt_expiry_ms(&access_token))
        .unwrap_or_else(|| crate::util::time::unix_now().saturating_add(3600) * 1000);
    let object = secret
        .as_object_mut()
        .ok_or_else(|| ChannelError::InvalidCredential("secret must be an object".into()))?;
    object.insert("access_token".into(), Value::String(access_token));
    object.insert("expires_at_ms".into(), json!(expires_at_ms));
    object.insert("auth_kind".into(), Value::String("slate_oauth".into()));
    if let Some(refresh_token) = response
        .refresh_token
        .filter(|value| !value.trim().is_empty())
    {
        object.insert("refresh_token".into(), Value::String(refresh_token));
    }
    Ok(secret)
}

fn jwt_expiry_ms(access_token: &str) -> Option<i64> {
    let payload = access_token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get("exp")?
        .as_i64()?
        .checked_mul(1000)
}

async fn form_post(
    client: &Arc<dyn UpstreamClient>,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(StatusCode, Bytes), ChannelError> {
    let body = fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                crate::channel::oauth::percent_encode(key),
                crate::channel::oauth::percent_encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let request = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(http::header::ACCEPT, "application/json")
        .body(Bytes::from(body))
        .map_err(|error| ChannelError::Build(format!("slate oauth request build: {error}")))?;
    let response = client
        .send(request)
        .await
        .map_err(|error| ChannelError::Build(format!("slate oauth request failed: {error}")))?;
    let (parts, body) = response.into_parts();
    Ok((parts.status, body))
}

fn status_error(context: &str, status: StatusCode, body: &Bytes) -> ChannelError {
    let snippet: String = String::from_utf8_lossy(body).chars().take(256).collect();
    ChannelError::Build(format!("{context} {status}: {snippet}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct QueueUpstream(Mutex<Vec<(u16, &'static [u8])>>);

    #[async_trait::async_trait]
    impl UpstreamClient for QueueUpstream {
        async fn send(
            &self,
            _request: Request<Bytes>,
        ) -> Result<http::Response<Bytes>, crate::http::client::ClientError> {
            let (status, body) = self.0.lock().unwrap().remove(0);
            Ok(http::Response::builder()
                .status(status)
                .body(Bytes::from_static(body))
                .unwrap())
        }
    }

    fn client(responses: Vec<(u16, &'static [u8])>) -> Arc<dyn UpstreamClient> {
        Arc::new(QueueUpstream(Mutex::new(responses)))
    }

    #[test]
    fn refresh_is_only_requested_for_expiring_oauth_credentials() {
        assert!(!needs_refresh(&json!({"api_key":"key"})));
        assert!(needs_refresh(&json!({"refresh_token":"refresh"})));
        assert!(needs_refresh(&json!({
            "access_token":"access",
            "refresh_token":"refresh",
            "expires_at_ms":1
        })));
        assert!(!needs_refresh(&json!({
            "access_token":"access",
            "refresh_token":"refresh",
            "expires_at_ms":i64::MAX
        })));
    }

    #[tokio::test]
    async fn device_poll_maps_pending_and_ready() {
        let pending = client(vec![(
            400,
            br#"{"error":"authorization_pending","error_description":"pending"}"#,
        )]);
        assert!(matches!(
            device_poll(&pending, "device").await.unwrap(),
            DevicePoll::Pending
        ));

        let ready = client(vec![(
            200,
            br#"{"access_token":"access","refresh_token":"refresh","expires_in":60,"user":{}}"#,
        )]);
        let secret = match device_poll(&ready, "device").await.unwrap() {
            DevicePoll::Ready(secret) => secret,
            _ => panic!("expected ready"),
        };
        assert_eq!(secret["access_token"], "access");
        assert_eq!(secret["refresh_token"], "refresh");
        assert_eq!(secret["auth_kind"], "slate_oauth");
    }

    #[tokio::test]
    async fn refresh_rotates_tokens_and_preserves_fields() {
        let upstream = client(vec![(
            200,
            br#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":120}"#,
        )]);
        let output = refresh(
            &upstream,
            &json!({"access_token":"old","refresh_token":"old-refresh","keep":true}),
        )
        .await
        .unwrap();
        assert_eq!(output["access_token"], "new-access");
        assert_eq!(output["refresh_token"], "new-refresh");
        assert_eq!(output["keep"], true);
        assert!(output["expires_at_ms"].as_i64().unwrap() > 0);
    }
}
