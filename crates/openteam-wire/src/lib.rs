//! `openteam-wire` — the contract crate (ADR 0013).
//!
//! Holds everything the harness and the mock must agree on, and only that:
//! the OpenAI wire subset (chat completions with tool calling, embeddings, the
//! error body — ADR 0018), the identity grammar (`AgentId`/role/specialty-slug
//! ⇄ the `user` field, ADR 0012), the `X-OpenTeam-*` header names and the
//! `Seed` (ADR 0008), the base64 f32-LE embedding codec (ADR 0014), and the
//! `TokenCounter` plus the usage free-fns (ADR 0018).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod chat;
mod embeddings;
mod error;
mod identity;
mod tokens;

pub use chat::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, FinishReason, FunctionCall,
    FunctionDef, MessageContent, NamedFunction, ResponseMessage, ToolCall, ToolChoice,
    ToolChoiceMode, ToolDef, ToolType, Usage,
};
pub use embeddings::{
    CodecError, EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage,
    EmbeddingVector, EncodingFormat, decode_f32_le, encode_f32_le,
};
pub use error::{ApiError, ErrorResponse};
pub use identity::{
    AgentId, HEADER_CALL_SEQ, HEADER_SEED, IdentityError, ParsedUser, Role, Seed, SpecialtySlug,
    WireIdentity,
};
pub use tokens::{CharCountTokenizer, TokenCounter, completion_tokens, prompt_tokens, usage};
