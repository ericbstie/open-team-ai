//! The LLM transport seam: `LlmClient`, the reqwest adapter, and per-agent
//! `AgentChannel`s (ADR 0018).
//!
//! The `dyn LlmClient` transport is a single stateless value shared as `Arc`
//! (one connection pool); each agent holds a cheap `AgentChannel` owning its
//! monotonic call-sequence counter and rendering its `user` field. The
//! adapter stamps `user` into the schema-pure body and the auxiliary
//! channels into `X-OpenTeam-*` headers (ADR 0008); embeddings carry no
//! auxiliary headers (mock embeddings are seed-independent, ADR 0014).

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use openteam_wire::{
    AgentId, ChatCompletionRequest, ChatCompletionResponse, EmbeddingInput, EmbeddingRequest,
    EmbeddingResponse, EncodingFormat, HEADER_CALL_SEQ, HEADER_SEED, ParsedUser, Role,
    SpecialtySlug, WireIdentity,
};
use url::Url;

use crate::knowledge::{EmbedError, Embedder, FeatureHashEmbedder};

/// A transport-layer fault. Carries no reqwest types, so the in-memory fake
/// adapter satisfies the seam cleanly (ADR 0018).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// A non-2xx response with a parseable OpenAI error body.
    #[error("llm endpoint returned {status}: {}", error.message)]
    Http {
        status: u16,
        error: openteam_wire::ApiError,
    },
    /// A connection/IO-level fault.
    #[error("llm transport error: {0}")]
    Transport(String),
    /// A 2xx response whose body did not decode as the wire type.
    #[error("malformed llm response: {0}")]
    Malformed(String),
}

/// The harness-side transport seam (ADR 0018) — one of the two
/// `#[async_trait]` dyn seams (with `VectorStore`, ADR 0013).
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// One chat completion. The adapter stamps `id.user` into the body and
    /// the call-seq/seed into headers.
    async fn complete(
        &self,
        id: &WireIdentity,
        req: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError>;

    /// One embeddings call. No identity channels (ADR 0018).
    async fn embed(&self, req: &EmbeddingRequest) -> Result<EmbeddingResponse, LlmError>;
}

/// Transport configuration, built from clap in the bin (ADR 0018/0024).
/// `base_url: None` means "use the in-process mock's bound address" — the
/// bin resolves it before constructing the adapter.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: Option<Url>,
    pub api_key: Option<String>,
    pub model: String,
    pub embedding_model: String,
}

/// The default HTTP adapter (ADR 0018): real requests over reqwest, so the
/// in-process mock is reached byte-identically to a real endpoint
/// (ADR 0019).
#[derive(Debug)]
pub struct ReqwestLlmClient {
    http: reqwest::Client,
    base_url: Url,
    api_key: Option<String>,
}

/// Ensure the base URL ends in `/` so relative endpoints join *under* its path
/// instead of replacing the last segment (`https://host/api` → `https://host/api/`).
fn with_trailing_slash(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let with_slash = format!("{}/", base_url.path());
        base_url.set_path(&with_slash);
    }
    base_url
}

impl ReqwestLlmClient {
    pub fn new(base_url: Url, api_key: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: with_trailing_slash(base_url),
            api_key,
        }
    }

    /// Resolve a relative endpoint (`chat/completions`, `embeddings`) against
    /// the base URL. The base carries the full API path prefix — `…/v1/` for
    /// an OpenAI-schema server or the in-process mock, `…/api/` for Open WebUI
    /// — so the same client reaches both without a hardcoded path (ADR 0001).
    fn endpoint(&self, path: &str) -> Result<Url, LlmError> {
        self.base_url
            .join(path)
            .map_err(|e| LlmError::Transport(format!("bad endpoint url: {e}")))
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        body: &impl serde::Serialize,
        headers: &[(&str, String)],
    ) -> Result<T, LlmError> {
        let mut request = self.http.post(url).json(body);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        if (200..300).contains(&status) {
            serde_json::from_slice(&bytes).map_err(|e| LlmError::Malformed(e.to_string()))
        } else {
            let error = serde_json::from_slice::<openteam_wire::ErrorResponse>(&bytes)
                .map(|body| body.error)
                .unwrap_or_else(|_| {
                    openteam_wire::ApiError::invalid_request(
                        String::from_utf8_lossy(&bytes).into_owned(),
                        None,
                    )
                });
            Err(LlmError::Http { status, error })
        }
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn complete(
        &self,
        id: &WireIdentity,
        req: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError> {
        // Stamp the identity: `user` in the schema-pure body, the auxiliary
        // channels as headers (ADR 0008/0018).
        let mut body = req.clone();
        body.user = Some(id.user.clone());
        let headers = [
            (HEADER_CALL_SEQ, id.call_seq.to_string()),
            (HEADER_SEED, id.seed.to_string()),
        ];
        self.post_json(self.endpoint("chat/completions")?, &body, &headers)
            .await
    }

    async fn embed(&self, req: &EmbeddingRequest) -> Result<EmbeddingResponse, LlmError> {
        self.post_json(self.endpoint("embeddings")?, req, &[]).await
    }
}

/// A cheap per-agent handle over the shared transport (ADR 0018): owns the
/// agent's monotonic call-sequence counter (`fetch_add` per completion) and
/// renders its `user` field from the current handle-and-specialty. The
/// counter never resets on respecialization — only the rendered slug
/// changes.
pub struct AgentChannel {
    transport: Arc<dyn LlmClient>,
    agent: AgentId,
    specialty: RwLock<SpecialtySlug>,
    seed: u64,
    call_seq: AtomicU64,
}

impl AgentChannel {
    pub fn new(transport: Arc<dyn LlmClient>, agent: AgentId, seed: u64) -> Self {
        Self {
            transport,
            agent,
            specialty: RwLock::new(SpecialtySlug::generalist()),
            seed,
            call_seq: AtomicU64::new(0),
        }
    }

    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// The next call sequence this channel will use (for the
    /// `turn_completed` first/last span, ADR 0022).
    pub fn next_call_seq(&self) -> u64 {
        self.call_seq.load(Ordering::SeqCst)
    }

    /// Swap the specialty slug rendered into `user` (respecialization,
    /// ADR 0003/0018). The call-seq counter keeps climbing.
    pub fn set_specialty(&self, slug: SpecialtySlug) {
        let mut guard = self
            .specialty
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = slug;
    }

    fn rendered_user(&self) -> String {
        match self.agent.role() {
            Role::Orchestrator => ParsedUser::Orchestrator.render(),
            Role::MetaAgent => ParsedUser::MetaAgent {
                agent: self.agent.clone(),
            }
            .render(),
            Role::TeamAgent => {
                let specialty = self
                    .specialty
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                ParsedUser::TeamAgent {
                    agent: self.agent.clone(),
                    specialty,
                }
                .render()
            }
        }
    }

    /// One completion: mint the call sequence, render identity, delegate.
    /// Returns the call sequence used alongside the response.
    pub async fn complete(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<(u64, ChatCompletionResponse), LlmError> {
        let call_seq = self.call_seq.fetch_add(1, Ordering::SeqCst);
        let identity = WireIdentity {
            user: self.rendered_user(),
            call_seq,
            seed: self.seed,
        };
        let response = self.transport.complete(&identity, req).await?;
        Ok((call_seq, response))
    }
}

impl std::fmt::Debug for AgentChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentChannel")
            .field("agent", &self.agent)
            .field("seed", &self.seed)
            .field("call_seq", &self.call_seq)
            .finish_non_exhaustive()
    }
}

/// The production `Embedder` adapter: embeds over the wire `/v1/embeddings`
/// call (ADR 0014), requesting base64 explicitly so every offline run
/// exercises the committed base64 f32-LE path (ADR 0018).
#[derive(Clone)]
pub struct WireEmbedder {
    transport: Arc<dyn LlmClient>,
    model: String,
    /// When set, embeddings are computed locally by feature hashing instead of
    /// over the wire — for endpoints without an OpenAI `/v1/embeddings` route
    /// (e.g. Open WebUI). The `FeatureHashEmbedder` is offline and seed-free
    /// (ADR 0014), so the knowledge store still works without a remote model.
    local: Option<FeatureHashEmbedder>,
}

impl WireEmbedder {
    pub fn new(transport: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            transport,
            model: model.into(),
            local: None,
        }
    }

    /// A `WireEmbedder` that embeds locally, never calling the transport's
    /// `/embeddings` endpoint. Used when `--local-embeddings` is set.
    pub fn local(transport: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            transport,
            model: model.into(),
            local: Some(FeatureHashEmbedder::new()),
        }
    }
}

impl Embedder for WireEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        if let Some(local) = &self.local {
            return Ok(local.embed_text(text));
        }
        let request = EmbeddingRequest {
            model: self.model.clone(),
            input: EmbeddingInput::Text(text.to_string()),
            encoding_format: Some(EncodingFormat::Base64),
            dimensions: None,
            user: None,
        };
        let response = self
            .transport
            .embed(&request)
            .await
            .map_err(|e| EmbedError::Backend(e.to_string()))?;
        let first = response
            .data
            .first()
            .ok_or_else(|| EmbedError::Backend("empty embeddings response".into()))?;
        first
            .embedding
            .to_floats()
            .map_err(|e| EmbedError::Backend(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openteam_wire::{Choice, FinishReason, MessageContent, ResponseMessage, Usage};
    use std::sync::Mutex;

    /// The in-memory fake adapter (ADR 0018's second adapter): scripts
    /// responses per call and records the identities it saw.
    pub(crate) struct FakeLlm {
        pub seen: Mutex<Vec<WireIdentity>>,
    }

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn complete(
            &self,
            id: &WireIdentity,
            _req: &ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, LlmError> {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(id.clone());
            Ok(ChatCompletionResponse {
                id: "chatcmpl-test".into(),
                object: "chat.completion".into(),
                created: 0,
                model: "openteam-mock".into(),
                choices: vec![Choice {
                    index: 0,
                    message: ResponseMessage {
                        role: "assistant".into(),
                        content: Some("ok".into()),
                        refusal: None,
                        tool_calls: None,
                    },
                    logprobs: None,
                    finish_reason: FinishReason::Stop,
                }],
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                },
            })
        }

        async fn embed(&self, _req: &EmbeddingRequest) -> Result<EmbeddingResponse, LlmError> {
            Err(LlmError::Transport("no embeddings in this fake".into()))
        }
    }

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "openteam-mock".into(),
            messages: vec![openteam_wire::ChatMessage::User {
                content: MessageContent::Text("hi".into()),
                name: None,
            }],
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            user: None,
            safety_identifier: None,
            prompt_cache_key: None,
            stream: None,
            n: None,
        }
    }

    #[tokio::test]
    async fn channel_counts_monotonically_and_survives_respecialize() {
        let fake = Arc::new(FakeLlm {
            seen: Mutex::new(Vec::new()),
        });
        let channel = AgentChannel::new(fake.clone(), AgentId::team(1), 42);
        let req = request();
        let (seq0, _) = channel.complete(&req).await.unwrap();
        let (seq1, _) = channel.complete(&req).await.unwrap();
        channel.set_specialty(SpecialtySlug::parse("doc-reviewer").unwrap());
        let (seq2, _) = channel.complete(&req).await.unwrap();
        assert_eq!((seq0, seq1, seq2), (0, 1, 2), "monotonic, never reset");

        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].user, "team-agent:agent-1:generalist");
        assert_eq!(seen[2].user, "team-agent:agent-1:doc-reviewer");
        assert_eq!(seen[2].call_seq, 2);
        assert_eq!(seen[2].seed, 42);
    }

    #[test]
    fn orchestrator_and_meta_render_their_grammar() {
        let fake = Arc::new(FakeLlm {
            seen: Mutex::new(Vec::new()),
        });
        let orch = AgentChannel::new(fake.clone(), AgentId::orchestrator(), 1);
        assert_eq!(orch.rendered_user(), "orchestrator");
        let meta = AgentChannel::new(fake, AgentId::meta(1), 1);
        assert_eq!(meta.rendered_user(), "meta-agent:meta-1");
    }
}
