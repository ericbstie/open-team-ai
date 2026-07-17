//! Mock contract tests (ADR 0025 Tier 3): synthetic fixed requests against
//! the router bound on **real loopback** via `serve()`.
//!
//! The schema oracle is a third party's types — `async-openai`'s response
//! structs — never our own `openteam-wire` types, which would be tautological
//! (ADR 0025). Exact-envelope determinism runs on a frozen injected clock.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};

use openteam_mock::{
    AppState, BuiltinArc, FrozenClock, Scenario, ScenarioPlayer, ShutdownHandle, serve,
};
use openteam_wire::{HEADER_CALL_SEQ, HEADER_SEED, decode_f32_le};

/// The transcript-§2 registries (pins-§1 arg shapes), shared with the unit
/// sweep via one fixture file.
const REGISTRIES: &str = include_str!("../src/testdata/registries.json");

const FROZEN_UNIX: u64 = 1_752_710_400;

async fn start_builtin() -> (SocketAddr, ShutdownHandle) {
    let state = AppState::new(
        Arc::new(BuiltinArc::new()),
        Arc::new(FrozenClock(FROZEN_UNIX)),
    );
    serve(state, 0).await.expect("bind loopback")
}

fn tools_json(role: &str) -> Value {
    let registries: Value = serde_json::from_str(REGISTRIES).expect("registries parse");
    registries[role].clone()
}

fn chat_body(user: Option<&str>, sections: &str, role: &str) -> Value {
    let mut body = json!({
        "model": "openteam-mock",
        "messages": [
            { "role": "system", "content": "(skeleton — inert to the mock)" },
            { "role": "user", "content": sections }
        ],
        "tools": tools_json(role),
        "tool_choice": "auto",
        "parallel_tool_calls": role != "meta"
    });
    if let Some(user) = user {
        body["user"] = json!(user);
    }
    body
}

const EMPTY_BOARD_SECTIONS: &str = "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Board digest\n(empty)\nrun-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0\n\n## Knowledge retrievals\n(none)\n\n## Fresh messages\n(none)\n\n## Directives\n(none)";

const TWO_OPEN_TASKS_SECTIONS: &str = "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Board digest\n- task 1 [Open] team:t1  \"Draft the setup section\"\n- task 2 [Open] team:t1  \"Draft the architecture overview\"\n\n## Claimed task\n(none)\n\n## Recent activity\n(none)\n\n## Fresh messages\n(none)";

const META_FRESH_SECTIONS: &str = "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Metrics digest\nthroughput: 0 task_completed / 15 EventIds · latency: work n/a\nutilization:\n  - agent-1: Working (task 1), generalist\n  - agent-2: Working (task 2), generalist\n  - agent-3: Idle, generalist (idle 8)\nmailbox: depth 0, max 1, oldest-pending-age 0\ntokens: run 2.6k · faults: parks 0 · directives: issued 0/ful 0/dec 0\n\n## Directive outcomes\n(none issued)\n\n## Recent events\n- event 5 task_claimed (agent-1)";

async fn post_chat(addr: SocketAddr, body: &Value, seed: u64, call_seq: u64) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .header(HEADER_SEED, seed.to_string())
        .header(HEADER_CALL_SEQ, call_seq.to_string())
        .json(body)
        .send()
        .await
        .expect("request succeeds")
}

async fn post_embeddings(addr: SocketAddr, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/embeddings"))
        .json(body)
        .send()
        .await
        .expect("request succeeds")
}

/// The wire-side schema check: every tool call names a tool from the request
/// with JSON-object args carrying the required keys and nothing outside the
/// schema's properties.
fn assert_calls_valid(tools: &Value, response: &Value) {
    let message = &response["choices"][0]["message"];
    let Some(calls) = message["tool_calls"].as_array() else {
        return;
    };
    for call in calls {
        assert_eq!(call["type"], "function");
        let name = call["function"]["name"].as_str().expect("verb name");
        let schema = tools
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool["function"]["name"] == name)
            .unwrap_or_else(|| panic!("verb {name} not in the tools array"))["function"]
            ["parameters"]
            .clone();
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().expect("args string"))
                .expect("arguments parse as JSON");
        let object = args.as_object().expect("arguments are an object");
        for required in schema["required"].as_array().into_iter().flatten() {
            assert!(
                object.contains_key(required.as_str().expect("key")),
                "{name}: missing required {required}"
            );
        }
        let properties = schema["properties"].as_object().expect("properties");
        for key in object.keys() {
            assert!(properties.contains_key(key), "{name}: stray key {key}");
        }
    }
}

#[tokio::test]
async fn every_response_deserializes_into_the_third_party_oracle() {
    let (addr, handle) = start_builtin().await;
    let cases = [
        (
            Some("orchestrator"),
            EMPTY_BOARD_SECTIONS,
            "orchestrator",
            0,
        ),
        (
            Some("team-agent:agent-1:generalist"),
            TWO_OPEN_TASKS_SECTIONS,
            "team",
            0,
        ),
        (Some("meta-agent:meta-1"), META_FRESH_SECTIONS, "meta", 0),
        // A plain client outside the identity grammar is served identically.
        (None, "What is the capital of France?", "team", 3),
    ];
    for (user, sections, role, call_seq) in cases {
        let body = chat_body(user, sections, role);
        let response = post_chat(addr, &body, 42, call_seq).await;
        assert_eq!(response.status(), 200, "case {user:?}");
        let bytes = response.bytes().await.expect("body");
        let oracle: async_openai::types::chat::CreateChatCompletionResponse =
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|fault| panic!("oracle rejects {user:?}: {fault}"));
        assert_eq!(oracle.object, "chat.completion");
        assert_eq!(oracle.choices.len(), 1);
        assert!(oracle.usage.is_some(), "usage always present");
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn exact_envelope_determinism_on_a_frozen_clock() {
    let (addr, handle) = start_builtin().await;
    let body = chat_body(Some("orchestrator"), EMPTY_BOARD_SECTIONS, "orchestrator");
    let first = post_chat(addr, &body, 42, 0)
        .await
        .bytes()
        .await
        .expect("body");
    let second = post_chat(addr, &body, 42, 0)
        .await
        .bytes()
        .await
        .expect("body");
    assert_eq!(
        first, second,
        "same (user, call_seq, seed) → byte-identical"
    );

    let parsed: Value = serde_json::from_slice(&first).expect("json");
    assert_eq!(
        parsed["id"], "chatcmpl-42-orchestrator-0",
        "pinned id format"
    );
    assert_eq!(parsed["created"], FROZEN_UNIX);
    assert_eq!(parsed["model"], "openteam-mock", "model echoed");

    // A different tuple diverges (different call_seq → different id).
    let third = post_chat(addr, &body, 42, 1)
        .await
        .bytes()
        .await
        .expect("body");
    let parsed_third: Value = serde_json::from_slice(&third).expect("json");
    assert_eq!(parsed_third["id"], "chatcmpl-42-orchestrator-1");
    handle.shutdown().await;
}

#[tokio::test]
async fn anonymous_and_unparseable_users_get_stable_ids() {
    let (addr, handle) = start_builtin().await;
    let anonymous = chat_body(None, "hello", "team");
    let response: Value = post_chat(addr, &anonymous, 7, 0)
        .await
        .json()
        .await
        .expect("json");
    assert_eq!(response["id"], "chatcmpl-7-anon-0", "absent user → anon");

    let raw = chat_body(Some("my-custom-client"), "hello", "team");
    let response: Value = post_chat(addr, &raw, 7, 2)
        .await
        .json()
        .await
        .expect("json");
    assert_eq!(
        response["id"], "chatcmpl-7-my-custom-client-2",
        "unparseable user → raw user string"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn arc_never_emits_an_invalid_call_at_the_wire() {
    let (addr, handle) = start_builtin().await;
    let cases = [
        ("orchestrator", EMPTY_BOARD_SECTIONS, "orchestrator"),
        (
            "team-agent:agent-1:generalist",
            TWO_OPEN_TASKS_SECTIONS,
            "team",
        ),
        ("meta-agent:meta-1", META_FRESH_SECTIONS, "meta"),
    ];
    for seed in [0_u64, 1, 42, 999, u64::MAX] {
        for (user, sections, role) in cases {
            let body = chat_body(Some(user), sections, role);
            let response: Value = post_chat(addr, &body, seed, 0)
                .await
                .json()
                .await
                .expect("json");
            assert_calls_valid(&body["tools"], &response);
        }
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn embeddings_are_deterministic_and_seed_independent() {
    let (addr, handle) = start_builtin().await;
    let body = json!({ "model": "openteam-mock", "input": "install mise then build" });
    // Differing seed headers must not matter: embeddings read no identity.
    let client = reqwest::Client::new();
    let mut vectors = Vec::new();
    for seed in ["1", "999"] {
        let response = client
            .post(format!("http://{addr}/v1/embeddings"))
            .header(HEADER_SEED, seed)
            .header(HEADER_CALL_SEQ, "0")
            .json(&body)
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.expect("body");
        let oracle: async_openai::types::embeddings::CreateEmbeddingResponse =
            serde_json::from_slice(&bytes).expect("oracle accepts the embeddings response");
        assert_eq!(oracle.object, "list");
        assert_eq!(oracle.data[0].object, "embedding");
        assert_eq!(oracle.usage.total_tokens, oracle.usage.prompt_tokens);
        vectors.push(oracle.data[0].embedding.clone());
    }
    assert_eq!(vectors[0], vectors[1], "identical text → identical vector");
    assert_eq!(vectors[0].len(), 256, "default dimensions");
    handle.shutdown().await;
}

#[tokio::test]
async fn embeddings_float_is_the_default_and_base64_round_trips() {
    let (addr, handle) = start_builtin().await;
    // encoding_format unspecified → float (the spec default).
    let float_body = json!({ "model": "m", "input": "install mise", "dimensions": 32 });
    let float_response: Value = post_embeddings(addr, &float_body)
        .await
        .json()
        .await
        .expect("json");
    let floats: Vec<f32> = float_response["data"][0]["embedding"]
        .as_array()
        .expect("float array by default")
        .iter()
        .map(|v| v.as_f64().expect("number") as f32)
        .collect();
    assert_eq!(floats.len(), 32, "dimensions honored");

    // base64 requested → f32-LE base64, decodable by the wire codec.
    let base64_body = json!({
        "model": "m", "input": "install mise", "dimensions": 32, "encoding_format": "base64"
    });
    let bytes = post_embeddings(addr, &base64_body)
        .await
        .bytes()
        .await
        .expect("body");
    let oracle: async_openai::types::embeddings::CreateBase64EmbeddingResponse =
        serde_json::from_slice(&bytes).expect("oracle accepts the base64 response");
    let decoded = decode_f32_le(&oracle.data[0].embedding.0).expect("valid f32-LE base64");
    assert_eq!(decoded, floats, "base64 round-trips to the float vector");

    // Multi-input: one data entry per input, index = position.
    let multi = json!({ "model": "m", "input": ["a", "b", "c"] });
    let response: Value = post_embeddings(addr, &multi)
        .await
        .json()
        .await
        .expect("json");
    let data = response["data"].as_array().expect("data");
    assert_eq!(data.len(), 3);
    for (index, entry) in data.iter().enumerate() {
        assert_eq!(entry["index"], index as u64);
    }
    handle.shutdown().await;
}

fn assert_error_shape(body: &Value, param: Option<&str>) {
    let error = body["error"].as_object().expect("error object");
    for key in ["message", "type", "param", "code"] {
        assert!(error.contains_key(key), "error body must carry {key}");
    }
    assert_eq!(body["error"]["type"], "invalid_request_error");
    match param {
        Some(param) => assert_eq!(body["error"]["param"], param),
        None => assert!(body["error"]["param"].is_null()),
    }
}

#[tokio::test]
async fn embeddings_reject_stray_fields_and_token_arrays() {
    let (addr, handle) = start_builtin().await;
    let stray = json!({ "model": "m", "input": "x", "stray": true });
    let response = post_embeddings(addr, &stray).await;
    assert_eq!(response.status(), 400, "deny_unknown_fields → 400");
    assert_error_shape(&response.json().await.expect("json"), None);

    let tokens = json!({ "model": "m", "input": [1, 2, 3] });
    let response = post_embeddings(addr, &tokens).await;
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.expect("json");
    assert_error_shape(&body, Some("input"));
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("token-array"),
        "precise token-array message"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn stream_true_is_rejected_with_param_stream() {
    let (addr, handle) = start_builtin().await;
    let mut body = chat_body(Some("orchestrator"), EMPTY_BOARD_SECTIONS, "orchestrator");
    body["stream"] = json!(true);
    let response = post_chat(addr, &body, 42, 0).await;
    assert_eq!(response.status(), 400);
    assert_error_shape(&response.json().await.expect("json"), Some("stream"));
    handle.shutdown().await;
}

#[tokio::test]
async fn chat_validation_rejections() {
    let (addr, handle) = start_builtin().await;
    // n > 1 → 400 param "n".
    let mut body = chat_body(Some("orchestrator"), EMPTY_BOARD_SECTIONS, "orchestrator");
    body["n"] = json!(2);
    let response = post_chat(addr, &body, 42, 0).await;
    assert_eq!(response.status(), 400);
    assert_error_shape(&response.json().await.expect("json"), Some("n"));

    // Empty messages → 400 param "messages".
    let body = json!({ "model": "m", "messages": [] });
    let response = post_chat(addr, &body, 42, 0).await;
    assert_eq!(response.status(), 400);
    assert_error_shape(&response.json().await.expect("json"), Some("messages"));

    // Missing model → 400 at parse time (wire-subset checklist).
    let body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    let response = post_chat(addr, &body, 42, 0).await;
    assert_eq!(response.status(), 400);

    // Empty model → 404 model_not_found (the ADR 0019 "unknown model" read).
    let body = json!({ "model": "", "messages": [{ "role": "user", "content": "hi" }] });
    let response = post_chat(addr, &body, 42, 0).await;
    assert_eq!(response.status(), 404);
    let parsed: Value = response.json().await.expect("json");
    assert_eq!(parsed["error"]["code"], "model_not_found");
    handle.shutdown().await;
}

#[tokio::test]
async fn unknown_routes_get_the_standard_404_body() {
    let (addr, handle) = start_builtin().await;
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/v1/does-not-exist"))
        .json(&json!({}))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 404);
    assert_error_shape(&response.json().await.expect("json"), None);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_scenario_scripts_an_invalid_call_inside_a_valid_envelope() {
    // ADR 0023: the scenario player can emit an invalid *call* but never an
    // invalid *response* — the server owns the envelope either way.
    let scenario = Scenario::from_json_str(
        r#"{"version": 1, "scripts": [
            { "agent": "agent-1", "responses": [
                {"tool_calls": [{"name": "not_a_real_verb", "arguments": {"junk": true}}]}
            ] }
        ]}"#,
    )
    .expect("valid scenario");
    let state = AppState::new(
        Arc::new(ScenarioPlayer::new(scenario)),
        Arc::new(FrozenClock(FROZEN_UNIX)),
    );
    let (addr, handle) = serve(state, 0).await.expect("bind");
    let body = chat_body(
        Some("team-agent:agent-1:generalist"),
        TWO_OPEN_TASKS_SECTIONS,
        "team",
    );
    let bytes = post_chat(addr, &body, 42, 0)
        .await
        .bytes()
        .await
        .expect("body");
    let oracle: async_openai::types::chat::CreateChatCompletionResponse =
        serde_json::from_slice(&bytes).expect("still a schema-valid envelope");
    let message = &oracle.choices[0].message;
    let calls = message.tool_calls.as_ref().expect("scripted call");
    assert_eq!(calls.len(), 1);
    handle.shutdown().await;
}
