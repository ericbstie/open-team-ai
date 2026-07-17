//! The chat-completions wire subset (ADR 0018, docs/research/openai-wire-subset.md).
//!
//! Every type derives both `Serialize` and `Deserialize` because the harness and
//! the mock sit on opposite ends of the same type. Two serde idioms carry the
//! spec's nullable distinction: request optional params are
//! `Option<T>` + `skip_serializing_if` (omit when absent), while response
//! required-but-nullable keys (`content`, `refusal`, `logprobs`) are plain
//! `Option<T>` with no skip, so `None` serializes as an explicit `null`. The chat
//! request and all responses accept-and-ignore unknown fields (the embeddings
//! request is the strict one — see `embeddings`).

use serde::{Deserialize, Serialize};

/// `POST /v1/chat/completions` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// The identity channel (ADR 0008): the rendered ADR 0012 grammar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Spec successor to `user`; the mock tolerates it (ADR 0019).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    /// Spec successor to `user`; the mock tolerates it (ADR 0019).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Streaming is out of scope in v1 — the mock 400s on `true` (ADR 0019).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Typed so the mock can refuse `n > 1` rather than silently under-deliver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
}

/// One input message — a discriminated union on `role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// o1+ replacement for `system`; accepted for wire faithfulness.
    Developer {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        /// Nullable on tool-call turns; serialized as explicit `null` so the
        /// fed-back assistant message matches the response shape verbatim.
        content: Option<MessageContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Tool {
        content: MessageContent,
        tool_call_id: String,
    },
}

/// Input `content` is string-or-parts everywhere on the wire; the harness only
/// ever builds `Text`, and the mock never reads parts (ADR 0018).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<serde_json::Value>),
}

impl MessageContent {
    /// The rendered text of this content for token counting: `Text` verbatim,
    /// `Parts` as their serialized JSON (the mock never interprets parts).
    pub fn rendered_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
        }
    }
}

/// One entry of the request `tools` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: ToolType,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    Function,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Arbitrary JSON Schema object (rendered by schemars in the harness).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// `tool_choice`: a mode string or a named function.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolChoiceMode),
    Named {
        #[serde(rename = "type")]
        kind: ToolType,
        function: NamedFunction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceMode {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedFunction {
    pub name: String,
}

/// One tool call in an assistant message (request or response side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ToolType,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// A JSON-encoded *string*, never parsed at the wire layer (ADR 0018).
    pub arguments: String,
}

/// `POST /v1/chat/completions` response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    /// Always `"chat.completion"`.
    pub object: String,
    /// Unix seconds, from the injected `Clock` (ADR 0019).
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    /// Formally optional in the spec but always emitted by the mock.
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    /// Required-but-nullable key: emitted as explicit `null`.
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: FinishReason,
}

/// The assistant message of a response choice. `content` and `refusal` are
/// required-but-nullable keys (no skip → explicit `null`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    /// Always `"assistant"`.
    pub role: String,
    pub content: Option<String>,
    pub refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn request_optionals_omit_when_absent() {
        let req = ChatCompletionRequest {
            model: "openteam-mock".into(),
            messages: vec![ChatMessage::User {
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
        };
        let value: Value = serde_json::to_value(&req).unwrap();
        let obj = value.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["messages", "model"],
            "absent optionals must be omitted, not null"
        );
    }

    #[test]
    fn request_ignores_unknown_fields() {
        let raw = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7,
            "metadata": {"a": 1},
            "reasoning_effort": "high"
        });
        let req: ChatCompletionRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.model, "m");
    }

    #[test]
    fn response_nullable_keys_serialize_explicit_null() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-42-orchestrator-0".into(),
            object: "chat.completion".into(),
            created: 1_752_710_400,
            model: "openteam-mock".into(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: None,
                    refusal: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_a0_1".into(),
                        kind: ToolType::Function,
                        function: FunctionCall {
                            name: "claim_task".into(),
                            arguments: "{\"task\":1}".into(),
                        },
                    }]),
                },
                logprobs: None,
                finish_reason: FinishReason::ToolCalls,
            }],
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            },
        };
        let value: Value = serde_json::to_value(&resp).unwrap();
        let choice = &value["choices"][0];
        assert!(choice["logprobs"].is_null());
        assert!(
            choice.as_object().unwrap().contains_key("logprobs"),
            "logprobs must be present as explicit null"
        );
        let message = choice["message"].as_object().unwrap();
        assert!(message["content"].is_null());
        assert!(message.contains_key("content"));
        assert!(message["refusal"].is_null());
        assert!(message.contains_key("refusal"));
        assert_eq!(choice["finish_reason"], "tool_calls");
    }

    #[test]
    fn response_omits_tool_calls_on_text_turns() {
        let message = ResponseMessage {
            role: "assistant".into(),
            content: Some("done".into()),
            refusal: None,
            tool_calls: None,
        };
        let value: Value = serde_json::to_value(&message).unwrap();
        assert!(!value.as_object().unwrap().contains_key("tool_calls"));
    }

    #[test]
    fn message_union_round_trips_all_roles() {
        let messages = vec![
            ChatMessage::System {
                content: MessageContent::Text("skeleton".into()),
                name: None,
            },
            ChatMessage::User {
                content: MessageContent::Text("## Goal\n…".into()),
                name: None,
            },
            ChatMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_c0_1".into(),
                    kind: ToolType::Function,
                    function: FunctionCall {
                        name: "claim_task".into(),
                        arguments: "{\"task\":1}".into(),
                    },
                }]),
                refusal: None,
                name: None,
            },
            ChatMessage::Tool {
                content: MessageContent::Text("{\"status\":\"ok\"}".into()),
                tool_call_id: "call_c0_1".into(),
            },
        ];
        let json = serde_json::to_string(&messages).unwrap();
        let back: Vec<ChatMessage> = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
        // The assistant tool-call message serializes content as explicit null.
        let value: Value = serde_json::from_str(&json).unwrap();
        assert!(value[2].as_object().unwrap().contains_key("content"));
        assert!(value[2]["content"].is_null());
    }

    #[test]
    fn content_accepts_part_arrays() {
        let raw = json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}]
        });
        let msg: ChatMessage = serde_json::from_value(raw).unwrap();
        match msg {
            ChatMessage::User {
                content: MessageContent::Parts(parts),
                ..
            } => assert_eq!(parts.len(), 1),
            other => panic!("expected user parts content, got {other:?}"),
        }
    }

    #[test]
    fn tool_choice_forms_round_trip() {
        let auto: ToolChoice = serde_json::from_value(json!("auto")).unwrap();
        assert!(matches!(auto, ToolChoice::Mode(ToolChoiceMode::Auto)));
        let named: ToolChoice =
            serde_json::from_value(json!({"type": "function", "function": {"name": "f"}})).unwrap();
        match named {
            ToolChoice::Named { function, .. } => assert_eq!(function.name, "f"),
            other => panic!("expected named tool choice, got {other:?}"),
        }
        assert_eq!(
            serde_json::to_value(ToolChoice::Mode(ToolChoiceMode::Required)).unwrap(),
            json!("required")
        );
    }
}
