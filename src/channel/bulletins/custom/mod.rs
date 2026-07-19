//! Custom (universal) channel — a generic passthrough to any OpenAI / Claude /
//! Gemini-compatible endpoint. `base_url` is REQUIRED (no baked default); the
//! auth header is chosen by the inbound protocol (see [`auth`]).

mod auth;
mod oauth;
mod usage;

use std::sync::Arc;

use serde_json::Value;

use crate::channel::bulletins::common::{self, ApiKeyDefaults};
use crate::channel::{
    Channel, ChannelError, ChannelLogin, DeviceInit, DevicePoll, PrepareCtx, PreparedRequest,
};
use crate::http::client::UpstreamClient;
use crate::protocol::Provider;

const DEFAULTS: ApiKeyDefaults = ApiKeyDefaults {
    default_base_url: None, // base_url must be supplied in settings_json
    forward_headers: &[],
    forward_query: &[],
};

pub struct CustomChannel;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Channel for CustomChannel {
    fn id(&self) -> &'static str {
        "custom"
    }

    fn provider_family(&self) -> Provider {
        Provider::OpenAi
    }

    fn routing_table(&self) -> crate::channel::routes::RouteList {
        // Universal transparent passthrough: every (operation, kind) cell the v1
        // custom channel served, mapped to v2 cells. v1 emitted all protocols ×
        // all ops as passthrough; the OpenAI-family protocols collapse to a
        // single provider/content cell each. WebSocket/Live, the *Stream* image
        // ops, the bare-`OpenAi` content cell, and GeminiNDJson have no v2
        // representation and are dropped.
        use crate::channel::routes::{cg, pass, pv};
        use crate::protocol::{ContentGenerationKind::*, Operation::*, Provider as P};
        vec![
            pass(ListModels, pv(P::OpenAi)),
            pass(ListModels, pv(P::Claude)),
            pass(ListModels, pv(P::Gemini)),
            pass(GetModel, pv(P::OpenAi)),
            pass(GetModel, pv(P::Claude)),
            pass(GetModel, pv(P::Gemini)),
            pass(CountTokens, pv(P::OpenAi)),
            pass(CountTokens, pv(P::Claude)),
            pass(CountTokens, pv(P::Gemini)),
            pass(GenerateContent, cg(OpenAiResponses)),
            pass(GenerateContent, cg(OpenAiChatCompletions)),
            pass(GenerateContent, cg(ClaudeMessages)),
            pass(GenerateContent, cg(GeminiGenerateContent)),
            pass(StreamGenerateContent, cg(OpenAiResponses)),
            pass(StreamGenerateContent, cg(OpenAiChatCompletions)),
            pass(StreamGenerateContent, cg(ClaudeMessages)),
            pass(StreamGenerateContent, cg(GeminiGenerateContent)),
            pass(CreateEmbedding, pv(P::OpenAi)),
            pass(CreateEmbedding, pv(P::Claude)),
            pass(CreateEmbedding, pv(P::Gemini)),
            pass(CreateImage, pv(P::OpenAi)),
            pass(CreateImage, pv(P::Claude)),
            pass(CreateImage, pv(P::Gemini)),
            pass(EditImage, pv(P::OpenAi)),
            pass(EditImage, pv(P::Claude)),
            pass(EditImage, pv(P::Gemini)),
            pass(CompactContent, pv(P::OpenAi)),
            pass(CompactContent, pv(P::Claude)),
            pass(CompactContent, pv(P::Gemini)),
        ]
    }

    fn prepare(&self, ctx: PrepareCtx<'_>) -> Result<PreparedRequest, ChannelError> {
        // A connected Slate account stores OAuth tokens instead of `api_key`.
        // Keep the legacy API-key path unchanged for every other custom provider.
        if let Some(access_token) = oauth::access_token(ctx.secret).map(str::to_owned) {
            let base_url = common::resolve_base_url(&ctx, &DEFAULTS)?;
            let query = crate::channel::http_util::allow_query(ctx.query, DEFAULTS.forward_query);
            let uri = crate::channel::http_util::join_url(&base_url, ctx.path, query.as_deref())?;
            let mut req = crate::channel::http_util::build_request(
                ctx.method,
                uri,
                crate::channel::http_util::allow_headers(ctx.headers, DEFAULTS.forward_headers),
                ctx.body,
            )?;
            common::inject_bearer(&mut req, &access_token)?;
            return Ok(PreparedRequest::new(req));
        }

        // Resolve settings and path-driven compatibility before `ctx` is consumed.
        let configured_header = auth::configured(ctx.provider_settings)?;
        let proto = auth::detect(ctx.path);
        let (mut req, key) = common::build_request(ctx, &DEFAULTS)?;
        auth::apply(&mut req, &key, configured_header, proto)?;
        Ok(PreparedRequest::new(req))
    }

    fn needs_refresh(&self, secret: &Value) -> bool {
        oauth::needs_refresh(secret)
    }

    async fn refresh(
        &self,
        client: &Arc<dyn UpstreamClient>,
        secret: &Value,
    ) -> Result<Value, ChannelError> {
        oauth::refresh(client, secret).await
    }

    fn preserve_raw_request_body(&self, provider_settings: &serde_json::Value) -> bool {
        provider_settings
            .get("preserve_raw_request_body")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn prefetch_stream_before_commit(&self, provider_settings: &serde_json::Value) -> bool {
        provider_settings
            .get("prefetch_stream_before_commit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn prepare_usage_request(
        &self,
        secret: &serde_json::Value,
        settings: &serde_json::Value,
    ) -> Result<Option<http::Request<bytes::Bytes>>, ChannelError> {
        usage::request(secret, settings)
    }

    fn parse_usage(
        &self,
        status: http::StatusCode,
        _headers: &http::HeaderMap,
        body: &bytes::Bytes,
    ) -> Option<crate::channel::UsageSnapshot> {
        usage::parse(status, body)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ChannelLogin for CustomChannel {
    async fn device_start(
        &self,
        client: &Arc<dyn UpstreamClient>,
        _params: &Value,
    ) -> Result<DeviceInit, ChannelError> {
        oauth::device_start(client).await
    }

    async fn device_poll(
        &self,
        client: &Arc<dyn UpstreamClient>,
        device_code: &str,
    ) -> Result<DevicePoll, ChannelError> {
        oauth::device_poll(client, device_code).await
    }
}
