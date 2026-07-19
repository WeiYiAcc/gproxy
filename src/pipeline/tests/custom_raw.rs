use super::*;
use futures_util::StreamExt;

fn custom_bundle(settings: Value, with_body_rule: bool) -> String {
    let mut value: Value = serde_json::from_str(BUNDLE).unwrap();
    value["providers"] = json!([{
        "id": 1, "name": "custom-upstream", "channel": "custom", "label": null,
        "settings_json": settings, "credential_strategy": "round_robin",
        "proxy_url": null, "tls_fingerprint": null, "enabled": true
    }]);
    value["credentials"] = json!([
        { "id": 1, "provider_id": 1, "label": "first", "secret_json": { "api_key": "custom-1" }, "proxy_url": null, "tls_fingerprint": null, "enabled": true },
        { "id": 2, "provider_id": 1, "label": "second", "secret_json": { "api_key": "custom-2" }, "proxy_url": null, "tls_fingerprint": null, "enabled": true }
    ]);
    value["provider_models"] = json!([]);
    value["routes"] = json!([{
        "id": 1, "name": "client-model", "strategy": "failover", "enabled": true,
        "description": null
    }]);
    value["route_members"] = json!([{
        "id": 1, "route_id": 1, "provider_id": 1,
        "upstream_model_id": "server-model", "weight": 100, "tier": 0, "enabled": true
    }]);
    value["aliases"] = json!([]);
    value["routing_rules"] = json!([]);
    if with_body_rule {
        value["rule_sets"] = json!([{
            "id": 1, "name": "explicit", "enabled": true, "description": null
        }]);
        value["rules"] = json!([{
            "id": 1, "rule_set_id": 1, "kind": "system_text",
            "config_json": { "text": "EXPLICIT RULE" },
            "filter_model_pattern": null, "filter_operation_keys": null,
            "sort_order": 0, "enabled": true
        }]);
        value["provider_rule_sets"] = json!([{
            "id": 1, "provider_id": 1, "rule_set_id": 1,
            "sort_order": 0, "enabled": true
        }]);
    } else {
        value["rule_sets"] = json!([]);
        value["rules"] = json!([]);
        value["provider_rule_sets"] = json!([]);
    }
    serde_json::to_string(&value).unwrap()
}

fn settings(raw: bool, prefetch: bool) -> Value {
    json!({
        "base_url": "http://custom.local",
        "api_key_header": "x-api-key",
        "preserve_raw_request_body": raw,
        "prefetch_stream_before_commit": prefetch,
    })
}

fn custom_ctx(body: Bytes) -> RequestCtx {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer sk-test".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("accept", "text/event-stream".parse().unwrap());
    RequestCtx {
        request_id: "custom-raw-test".into(),
        method: Method::POST,
        path: "/v1/chat/completions".into(),
        query: None,
        headers,
        body,
        mode: RoutingMode::Aggregated,
        identity: None,
        op: None,
        stream: false,
        route_name: None,
        pending_micros: 0,
    }
}

fn streaming_body() -> Bytes {
    Bytes::from_static(
        br#"{ "model" : "client-model", "stream" : true, "messages" : [{"role":"user","content":"hi"}], "custom_extension" : {"unknown":[1,2,3]} }"#,
    )
}

fn data(text: &'static [u8]) -> FakeStreamItem {
    FakeStreamItem::Chunk(Bytes::from_static(text))
}

async fn collect(outcome: crate::pipeline::outcome::ExecOutcome) -> Vec<u8> {
    let ResponseBody::Stream(stream) = outcome.body else {
        panic!("expected stream")
    };
    stream
        .map(|item| item.expect("stream item"))
        .collect::<Vec<_>>()
        .await
        .concat()
}

#[tokio::test]
async fn raw_passthrough_preserves_exact_body_model_and_unknown_fields() {
    let body = streaming_body();
    let fake = Arc::new(FakeUpstream::new(
        Bytes::new(),
        vec![Bytes::from_static(b"data: {\"choices\":[]}\n\n")],
    ));
    let bundle = custom_bundle(settings(true, false), false);
    let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;

    let _ = crate::pipeline::execute(&state, custom_ctx(body.clone()))
        .await
        .expect("custom raw request");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[0].uri, "http://custom.local/v1/chat/completions");
    assert_eq!(seen[0].body, body);
    assert!(
        seen[0]
            .body
            .windows(b"custom_extension".len())
            .any(|w| w == b"custom_extension")
    );
    assert!(
        seen[0]
            .body
            .windows(b"server-model".len())
            .all(|w| w != b"server-model")
    );
    assert!(
        seen[0]
            .body
            .windows(b"include_usage".len())
            .all(|w| w != b"include_usage")
    );
}

#[tokio::test]
async fn raw_false_retains_model_rewrite_and_usage_injection() {
    let fake = Arc::new(FakeUpstream::new(
        Bytes::new(),
        vec![Bytes::from_static(b"data: {\"choices\":[]}\n\n")],
    ));
    let bundle = custom_bundle(settings(false, false), false);
    let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;

    let _ = crate::pipeline::execute(&state, custom_ctx(streaming_body()))
        .await
        .expect("default transform behavior");
    let seen = fake.seen.lock().unwrap();
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["model"], "server-model");
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["custom_extension"]["unknown"], json!([1, 2, 3]));
}

#[tokio::test]
async fn explicit_process_body_rules_still_apply_in_raw_mode() {
    let original = streaming_body();
    let fake = Arc::new(FakeUpstream::new(
        Bytes::new(),
        vec![Bytes::from_static(b"data: {\"choices\":[]}\n\n")],
    ));
    let bundle = custom_bundle(settings(true, false), true);
    let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;

    let _ = crate::pipeline::execute(&state, custom_ctx(original.clone()))
        .await
        .expect("explicit rule");
    let seen = fake.seen.lock().unwrap();
    assert_ne!(seen[0].body, original);
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["model"], "client-model");
    assert!(body["messages"].to_string().contains("EXPLICIT RULE"));
    assert!(body.get("stream_options").is_none());
}

#[tokio::test]
async fn prefetch_replays_success_once_and_fails_over_before_output() {
    let success = b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n";
    let cases = vec![
        vec![FakeStreamItem::Error("read failed")],
        vec![],
        vec![data(
            b"event: error\ndata: {\"error\":{\"message\":\"bad\"}}\n\n",
        )],
        vec![
            data(b": keep-alive\n\n"),
            FakeStreamItem::Error("after keepalive"),
        ],
    ];

    for first in cases {
        let fake = Arc::new(
            FakeUpstream::new(Bytes::new(), vec![])
                .with_stream_scripts(vec![first, vec![data(success)]]),
        );
        let bundle = custom_bundle(settings(true, true), false);
        let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;
        let output = collect(
            crate::pipeline::execute(&state, custom_ctx(streaming_body()))
                .await
                .expect("second credential succeeds"),
        )
        .await;
        assert_eq!(output, success);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(
            seen[0].headers.get("x-api-key"),
            seen[1].headers.get("x-api-key")
        );
    }
}

#[tokio::test]
async fn prefetch_preserves_keepalive_and_effective_data_order() {
    let first = b": keep-alive\n\n";
    let second = b"data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n";
    let third = b"data: {\"choices\":[{\"delta\":{\"content\":\"two\"}}]}\n\n";
    let fake = Arc::new(FakeUpstream::new(
        Bytes::new(),
        vec![
            Bytes::from_static(first),
            Bytes::from_static(second),
            Bytes::from_static(third),
        ],
    ));
    let bundle = custom_bundle(settings(true, true), false);
    let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;

    let output = collect(
        crate::pipeline::execute(&state, custom_ctx(streaming_body()))
            .await
            .expect("prefetched stream"),
    )
    .await;
    assert_eq!(
        output,
        [first.as_slice(), second.as_slice(), third.as_slice()].concat()
    );
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn error_after_effective_output_does_not_switch_credentials() {
    let first = b"data: {\"choices\":[{\"delta\":{\"content\":\"committed\"}}]}\n\n";
    let fake = Arc::new(
        FakeUpstream::new(Bytes::new(), vec![]).with_stream_scripts(vec![
            vec![data(first), FakeStreamItem::Error("late failure")],
            vec![data(b"data: {\"unexpected\":true}\n\n")],
        ]),
    );
    let bundle = custom_bundle(settings(true, true), false);
    let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;

    let output = collect(
        crate::pipeline::execute(&state, custom_ctx(streaming_body()))
            .await
            .expect("committed stream"),
    )
    .await;
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
    assert!(output.starts_with(first));
    assert_eq!(
        output
            .windows(first.len())
            .filter(|window| *window == first)
            .count(),
        1
    );
}

#[tokio::test]
async fn prefetch_limit_and_timeout_fail_over() {
    let success = b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n";
    let oversized_comment = Bytes::from(format!(": {}\n\n", "x".repeat(300)));
    for first in [
        vec![FakeStreamItem::Chunk(oversized_comment)],
        vec![FakeStreamItem::Pending],
    ] {
        let fake = Arc::new(
            FakeUpstream::new(Bytes::new(), vec![])
                .with_stream_scripts(vec![first, vec![data(success)]]),
        );
        let bundle = custom_bundle(settings(true, true), false);
        let (state, _dir) = state_with_bundle(Arc::clone(&fake), &bundle).await;
        let output = collect(
            crate::pipeline::execute(&state, custom_ctx(streaming_body()))
                .await
                .expect("bounded prefetch fails over"),
        )
        .await;
        assert_eq!(output, success);
        assert_eq!(fake.seen.lock().unwrap().len(), 2);
    }
}
