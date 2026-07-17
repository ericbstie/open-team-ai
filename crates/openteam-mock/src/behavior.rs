//! The `ChatDecision` behavior seam (ADR 0019).
//!
//! The behavior model is reached through a **synchronous** seam that returns
//! only the semantic decision, never the envelope: the mock server wraps a
//! `ChatDecision` into a valid `ChatCompletionResponse`, owning `id`,
//! `created`, the `model` echo, `choices[]`, and `usage` — so "every response
//! is schema-valid OpenAI" is structural. Two adapters justify the trait: the
//! built-in arc (`BuiltinArc`, ADR 0021) and the scenario player
//! (`ScenarioPlayer`, ADR 0023).

use openteam_wire::{ChatCompletionRequest, FinishReason, ResponseMessage, WireIdentity};

/// The mock's engine for deciding a chat response (CONTEXT.md: Behavior model).
///
/// Synchronous by design (ADR 0019): the behavior model is pure computation
/// over `(request, identity, seed)` with no I/O and no run-state access.
pub trait BehaviorModel: Send + Sync {
    fn chat(&self, req: &ChatCompletionRequest, id: &WireIdentity) -> ChatDecision;
}

/// What the behavior seam returns — the assistant `ResponseMessage` (text or
/// `tool_calls`) plus its `FinishReason`, and nothing else (CONTEXT.md: Chat
/// decision). The decision only, never the envelope.
#[derive(Debug, Clone)]
pub struct ChatDecision {
    pub message: ResponseMessage,
    pub finish: FinishReason,
}
