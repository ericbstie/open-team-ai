//! The mock server: real-loopback transport, stateless per request, owning
//! the response envelope (ADR 0019).
//!
//! `AppState` is immutable shared config only — behavior model, clock, token
//! counter — held as `Arc`; there is no run registration, no run lifecycle,
//! no cleanup. Every response is a pure function of the request body plus its
//! identity channels (`user`, `X-OpenTeam-Call-Seq`, `X-OpenTeam-Seed`), so
//! concurrent runs are isolated purely by their seed header. One
//! `build_router()` is mounted identically by the in-process default, the
//! standalone `openteam mock serve`, and the contract tests; embedded and
//! standalone differ only in who owns the listener's lifetime and shutdown.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use openteam_wire::{
    ApiError, CharCountTokenizer, ChatCompletionRequest, ChatCompletionResponse, Choice,
    EmbeddingData, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, EmbeddingVector,
    EncodingFormat, ErrorResponse, HEADER_CALL_SEQ, HEADER_SEED, ParsedUser, TokenCounter,
    WireIdentity, completion_tokens, encode_f32_le, prompt_tokens, usage,
};

use crate::arc::BuiltinArc;
use crate::behavior::BehaviorModel;
use crate::clock::{MockClock, SystemClock};
use crate::embeddings::{DEFAULT_DIMENSIONS, embed_text};
use crate::scenario::{Scenario, ScenarioPlayer};

/// Immutable shared mock configuration (ADR 0019): the behavior seam, the
/// clock for `created`, and the token counter for `usage`. The scenario
/// player IS the behavior when a scenario is loaded, else the built-in arc.
#[derive(Clone)]
pub struct AppState {
    pub behavior: Arc<dyn BehaviorModel>,
    pub clock: Arc<dyn MockClock>,
    pub tokens: Arc<dyn TokenCounter>,
}

impl AppState {
    /// A state over any behavior model and clock, with the shared
    /// `CharCountTokenizer` filling `usage` (ADR 0018).
    pub fn new(behavior: Arc<dyn BehaviorModel>, clock: Arc<dyn MockClock>) -> Self {
        Self {
            behavior,
            clock,
            tokens: Arc::new(CharCountTokenizer),
        }
    }

    /// The production default: the built-in arc on the system clock.
    pub fn builtin() -> Self {
        Self::new(Arc::new(BuiltinArc::new()), Arc::new(SystemClock))
    }

    /// A scenario-driven mock: the `ScenarioPlayer` becomes the behavior
    /// model, falling through to the built-in arc it owns (ADR 0023).
    pub fn with_scenario(scenario: Scenario) -> Self {
        Self::new(
            Arc::new(ScenarioPlayer::new(scenario)),
            Arc::new(SystemClock),
        )
    }
}

/// The single error path (ADR 0019): every rejection renders the standard
/// OpenAI error body with all four keys present.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    #[error("{message}")]
    BadRequest {
        message: String,
        param: Option<String>,
    },
    #[error("{message}")]
    NotFound {
        message: String,
        code: Option<String>,
    },
}

impl MockError {
    fn bad_request(message: impl Into<String>, param: Option<&str>) -> Self {
        Self::BadRequest {
            message: message.into(),
            param: param.map(str::to_owned),
        }
    }

    fn not_found(message: impl Into<String>, code: Option<&str>) -> Self {
        Self::NotFound {
            message: message.into(),
            code: code.map(str::to_owned),
        }
    }
}

impl IntoResponse for MockError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::BadRequest { message, param } => (
                axum::http::StatusCode::BAD_REQUEST,
                ApiError {
                    message,
                    kind: "invalid_request_error".into(),
                    param,
                    code: None,
                },
            ),
            Self::NotFound { message, code } => (
                axum::http::StatusCode::NOT_FOUND,
                ApiError {
                    message,
                    kind: "invalid_request_error".into(),
                    param: None,
                    code,
                },
            ),
        };
        (status, Json(ErrorResponse { error })).into_response()
    }
}

/// The one axum app (ADR 0019), mounted identically everywhere.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .fallback(unknown_route)
        .with_state(state)
}

/// A graceful-shutdown handle for a served mock: signals the listener and
/// awaits the serve task.
pub struct ShutdownHandle {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ShutdownHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// Bind `127.0.0.1:<port>` (`0` = OS-assigned ephemeral, ADR 0019), serve the
/// router on a background task, and hand back the bound address plus a
/// graceful-shutdown handle.
pub async fn serve(state: AppState, port: u16) -> std::io::Result<(SocketAddr, ShutdownHandle)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let (shutdown, rx) = oneshot::channel::<()>();
    let router = build_router(state);
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
        if let Err(fault) = served {
            tracing::error!(%fault, "mock server terminated abnormally");
        }
    });
    tracing::info!(%addr, "mock server listening");
    Ok((addr, ShutdownHandle { shutdown, task }))
}

fn header_u64(headers: &HeaderMap, name: &str) -> u64 {
    // Missing or unparseable headers default to 0: a real OpenAI-schema
    // client without the X-OpenTeam-* channel must still be served.
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// The `<handle>` slot of the response id: the parsed ADR 0012 handle, the
/// raw `user` string when unparseable, `anon` when absent or empty (pins §4).
fn response_handle(user: Option<&str>) -> String {
    match user {
        None | Some("") => "anon".to_owned(),
        Some(raw) => ParsedUser::parse(raw)
            .map(|parsed| parsed.agent().as_str().to_owned())
            .unwrap_or_else(|_| raw.to_owned()),
    }
}

/// Guard the model string. ADR 0019 pins "404 on an unknown route or model";
/// a mock with no model registry reads that as: accept and echo ANY non-empty
/// model string, and 404 (`code: "model_not_found"`) on the one detectably
/// unknown model — the empty string. Missing `model` is a 400 at parse time
/// per the wire-subset checklist.
fn require_model(model: &str) -> Result<(), MockError> {
    if model.is_empty() {
        return Err(MockError::not_found(
            "The model `` does not exist or you do not have access to it.",
            Some("model_not_found"),
        ));
    }
    Ok(())
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ChatCompletionResponse>, MockError> {
    let req: ChatCompletionRequest = serde_json::from_slice(&body).map_err(|fault| {
        MockError::bad_request(
            format!("invalid chat completion request body: {fault}"),
            None,
        )
    })?;
    require_model(&req.model)?;
    if req.messages.is_empty() {
        return Err(MockError::bad_request(
            "messages must contain at least one message.",
            Some("messages"),
        ));
    }
    if req.stream == Some(true) {
        return Err(MockError::bad_request(
            "Streaming is not supported by this mock.",
            Some("stream"),
        ));
    }
    if let Some(n) = req.n
        && n != 1
    {
        return Err(MockError::bad_request(
            format!("n must be 1 for this mock, got {n}."),
            Some("n"),
        ));
    }

    let identity = WireIdentity {
        user: req.user.clone().unwrap_or_default(),
        call_seq: header_u64(&headers, HEADER_CALL_SEQ),
        seed: header_u64(&headers, HEADER_SEED),
    };
    tracing::debug!(
        user = %identity.user,
        call_seq = identity.call_seq,
        seed = identity.seed,
        "chat completion"
    );

    let decision = state.behavior.chat(&req, &identity);

    // The server owns the envelope (ADR 0019): id derived from the
    // determinism key, created from the injected clock, model echoed, usage
    // from the wire free-fns.
    let prompt = prompt_tokens(state.tokens.as_ref(), &req.messages);
    let completion = completion_tokens(state.tokens.as_ref(), &decision.message);
    let handle = response_handle(req.user.as_deref());
    let response = ChatCompletionResponse {
        id: format!("chatcmpl-{}-{handle}-{}", identity.seed, identity.call_seq),
        object: "chat.completion".into(),
        created: state.clock.unix_seconds(),
        model: req.model,
        choices: vec![Choice {
            index: 0,
            message: decision.message,
            logprobs: None,
            finish_reason: decision.finish,
        }],
        usage: usage(prompt, completion),
    };
    Ok(Json(response))
}

/// True when the embeddings `input` is one of the token-array forms our
/// subset rejects (wire-subset research §2.1).
fn is_token_array_input(input: &serde_json::Value) -> bool {
    input.as_array().is_some_and(|items| {
        items
            .first()
            .is_some_and(|first| first.is_number() || first.is_array())
    })
}

async fn embeddings(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<EmbeddingResponse>, MockError> {
    // Parse in two steps so token-array inputs get a precise message rather
    // than an untagged-enum mismatch.
    let raw: serde_json::Value = serde_json::from_slice(&body).map_err(|fault| {
        MockError::bad_request(format!("invalid embeddings request body: {fault}"), None)
    })?;
    if raw.get("input").is_some_and(is_token_array_input) {
        return Err(MockError::bad_request(
            "token-array input is not supported by this mock; send a string or an array of strings.",
            Some("input"),
        ));
    }
    // The strict endpoint (ADR 0018): deny_unknown_fields makes a stray field
    // a 400 here, unlike the lenient chat request.
    let req: EmbeddingRequest = serde_json::from_value(raw).map_err(|fault| {
        MockError::bad_request(format!("invalid embeddings request: {fault}"), None)
    })?;
    require_model(&req.model)?;
    if req.dimensions == Some(0) {
        return Err(MockError::bad_request(
            "dimensions must be at least 1.",
            Some("dimensions"),
        ));
    }
    let inputs = req.input.texts();
    if inputs.is_empty() {
        return Err(MockError::bad_request(
            "input must not be an empty array.",
            Some("input"),
        ));
    }
    tracing::debug!(inputs = inputs.len(), "embeddings");

    let dimensions = req
        .dimensions
        .map_or(DEFAULT_DIMENSIONS, |dims| dims as usize);
    // Honor encoding_format (ADR 0019): the spec default is `float` when
    // unspecified; openai-python explicitly requests base64 (f32-LE via the
    // wire codec), so real traffic is typically base64 — both are served.
    let base64 = req.encoding_format == Some(EncodingFormat::Base64);
    let mut prompt = 0_u64;
    let data: Vec<EmbeddingData> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| {
            prompt += state.tokens.count(text) as u64;
            let vector = embed_text(text, dimensions);
            EmbeddingData {
                object: "embedding".into(),
                index: index as u32,
                embedding: if base64 {
                    EmbeddingVector::Base64(encode_f32_le(&vector))
                } else {
                    EmbeddingVector::Float(vector)
                },
            }
        })
        .collect();

    Ok(Json(EmbeddingResponse {
        object: "list".into(),
        data,
        model: req.model,
        usage: EmbeddingUsage {
            prompt_tokens: prompt,
            total_tokens: prompt,
        },
    }))
}

/// 404 with the standard error body on any unknown route (ADR 0019).
async fn unknown_route(method: Method, uri: Uri) -> MockError {
    MockError::not_found(
        format!("Unknown request URL: {method} {}", uri.path()),
        None,
    )
}
