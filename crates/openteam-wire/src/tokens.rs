//! Token counting: the single-method `TokenCounter` primitive plus the usage
//! free-fns (ADR 0018). One tokenizer serves both consumers — the mock's
//! `usage` fill and the context assembler's section budgets — and summation is
//! fixed policy, so it lives in free functions rather than on the trait.

use crate::chat::{ChatMessage, ResponseMessage, Usage};

/// The single-method token-counting primitive.
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

/// The default tokenizer: `ceil(chars / 4)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CharCountTokenizer;

impl TokenCounter for CharCountTokenizer {
    fn count(&self, text: &str) -> usize {
        text.chars().count().div_ceil(4)
    }
}

/// `prompt_tokens = Σ count()` over every request message's rendered content
/// plus each tool-call `arguments` string.
pub fn prompt_tokens(counter: &dyn TokenCounter, messages: &[ChatMessage]) -> u64 {
    let mut total = 0_u64;
    for message in messages {
        match message {
            ChatMessage::System { content, .. }
            | ChatMessage::Developer { content, .. }
            | ChatMessage::User { content, .. }
            | ChatMessage::Tool { content, .. } => {
                total += counter.count(&content.rendered_text()) as u64;
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(content) = content {
                    total += counter.count(&content.rendered_text()) as u64;
                }
                for call in tool_calls.iter().flatten() {
                    total += counter.count(&call.function.arguments) as u64;
                }
            }
        }
    }
    total
}

/// `completion_tokens = count()` over the generated assistant text, or the
/// serialized `tool_calls` on a tool-call turn.
pub fn completion_tokens(counter: &dyn TokenCounter, message: &ResponseMessage) -> u64 {
    if let Some(calls) = &message.tool_calls {
        let serialized = serde_json::to_string(calls).unwrap_or_default();
        return counter.count(&serialized) as u64;
    }
    let text = message.content.as_deref().unwrap_or_default();
    counter.count(text) as u64
}

/// Assemble a `Usage`: `total = prompt + completion`.
pub fn usage(prompt_tokens: u64, completion_tokens: u64) -> Usage {
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{FunctionCall, MessageContent, ToolCall, ToolType};

    #[test]
    fn char_count_is_ceil_chars_over_four() {
        let counter = CharCountTokenizer;
        assert_eq!(counter.count(""), 0);
        assert_eq!(counter.count("a"), 1);
        assert_eq!(counter.count("abcd"), 1);
        assert_eq!(counter.count("abcde"), 2);
        assert_eq!(counter.count("12345678"), 2);
        // chars, not bytes: four two-byte chars are one token.
        assert_eq!(counter.count("éééé"), 1);
    }

    #[test]
    fn prompt_tokens_sums_content_and_tool_call_arguments() {
        let counter = CharCountTokenizer;
        let messages = vec![
            ChatMessage::System {
                content: MessageContent::Text("abcd".into()), // 1
                name: None,
            },
            ChatMessage::User {
                content: MessageContent::Text("abcdefgh".into()), // 2
                name: None,
            },
            ChatMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    kind: ToolType::Function,
                    function: FunctionCall {
                        name: "claim_task".into(),
                        arguments: "{\"task\":1}".into(), // 10 chars -> 3
                    },
                }]),
                refusal: None,
                name: None,
            },
            ChatMessage::Tool {
                content: MessageContent::Text("abcd".into()), // 1
                tool_call_id: "call_1".into(),
            },
        ];
        assert_eq!(prompt_tokens(&counter, &messages), 1 + 2 + 3 + 1);
    }

    #[test]
    fn completion_tokens_counts_text_or_serialized_calls() {
        let counter = CharCountTokenizer;
        let text_turn = ResponseMessage {
            role: "assistant".into(),
            content: Some("abcdefgh".into()),
            refusal: None,
            tool_calls: None,
        };
        assert_eq!(completion_tokens(&counter, &text_turn), 2);

        let calls = vec![ToolCall {
            id: "call_1".into(),
            kind: ToolType::Function,
            function: FunctionCall {
                name: "claim_task".into(),
                arguments: "{\"task\":1}".into(),
            },
        }];
        let serialized_len = serde_json::to_string(&calls).unwrap().chars().count();
        let call_turn = ResponseMessage {
            role: "assistant".into(),
            content: None,
            refusal: None,
            tool_calls: Some(calls),
        };
        assert_eq!(
            completion_tokens(&counter, &call_turn),
            serialized_len.div_ceil(4) as u64
        );
    }

    #[test]
    fn usage_totals_prompt_plus_completion() {
        let u = usage(611, 78);
        assert_eq!(u.prompt_tokens, 611);
        assert_eq!(u.completion_tokens, 78);
        assert_eq!(u.total_tokens, 689);
    }
}
