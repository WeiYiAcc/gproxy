//! Custom auth: protocol-driven by default, with a constrained
//! `settings_json.api_key_header` override for OpenAI-compatible endpoints.

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderName};
use http::{Request, header};
use serde_json::Value;

use crate::channel::ChannelError;
use crate::channel::bulletins::common;

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone, Copy)]
pub(super) enum Proto {
    OpenAi,
    Claude,
    Gemini,
}

#[derive(Clone, Copy)]
pub(super) enum ApiKeyHeader {
    Bearer,
    XApiKey,
    XGoogApiKey,
}

pub(super) fn configured(settings: &Value) -> Result<Option<ApiKeyHeader>, ChannelError> {
    match settings.get("api_key_header") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match value.as_str() {
            "bearer" => Ok(Some(ApiKeyHeader::Bearer)),
            "x-api-key" => Ok(Some(ApiKeyHeader::XApiKey)),
            "x-goog-api-key" => Ok(Some(ApiKeyHeader::XGoogApiKey)),
            _ => Err(ChannelError::Build(format!(
                "invalid custom api_key_header {value:?}; expected bearer, x-api-key, or x-goog-api-key"
            ))),
        },
        Some(_) => Err(ChannelError::Build(
            "custom api_key_header must be a string".into(),
        )),
    }
}

/// Classify the inbound path for backward-compatible auth when no override is set.
pub(super) fn detect(path: &str) -> Proto {
    if path.contains("/messages") {
        Proto::Claude
    } else if path.starts_with("/v1beta")
        || path.contains(":generateContent")
        || path.contains(":streamGenerateContent")
        || path.contains(":countTokens")
    {
        Proto::Gemini
    } else {
        Proto::OpenAi
    }
}

pub(super) fn apply(
    req: &mut Request<Bytes>,
    key: &str,
    configured: Option<ApiKeyHeader>,
    proto: Proto,
) -> Result<(), ChannelError> {
    req.headers_mut().remove(AUTHORIZATION);
    req.headers_mut().remove("x-api-key");
    req.headers_mut().remove("x-goog-api-key");

    match configured {
        Some(ApiKeyHeader::Bearer) => common::inject_bearer(req, key),
        Some(ApiKeyHeader::XApiKey) => {
            common::inject_header(req, HeaderName::from_static("x-api-key"), key)
        }
        Some(ApiKeyHeader::XGoogApiKey) => {
            common::inject_header(req, HeaderName::from_static("x-goog-api-key"), key)
        }
        None => match proto {
            Proto::OpenAi => common::inject_bearer(req, key),
            Proto::Claude => {
                common::inject_header(req, HeaderName::from_static("x-api-key"), key)?;
                common::inject_static(
                    req,
                    HeaderName::from_static("anthropic-version"),
                    ANTHROPIC_VERSION,
                );
                Ok(())
            }
            Proto::Gemini => {
                common::inject_header(req, HeaderName::from_static("x-goog-api-key"), key)
            }
        },
    }?;

    // An explicit override owns authentication completely; do not retain a
    // protocol-specific companion header from a process rule or inbound request.
    if configured.is_some() {
        req.headers_mut()
            .remove(header::HeaderName::from_static("anthropic-version"));
    }
    Ok(())
}
