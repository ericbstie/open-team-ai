//! The OpenAI error body (docs/research/openai-wire-subset.md §3).

use serde::{Deserialize, Serialize};

/// The top-level error response: `{ "error": { … } }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

/// The `error` object — all four keys are required; `param` and `code` are
/// nullable and serialize as explicit `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub param: Option<String>,
    pub code: Option<String>,
}

impl ApiError {
    /// The workhorse 400/404 validation error shape.
    pub fn invalid_request(message: impl Into<String>, param: Option<&str>) -> Self {
        Self {
            message: message.into(),
            kind: "invalid_request_error".into(),
            param: param.map(str::to_owned),
            code: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_four_keys_present_with_explicit_nulls() {
        let body = ErrorResponse {
            error: ApiError::invalid_request("Streaming is not supported.", Some("stream")),
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value,
            json!({
                "error": {
                    "message": "Streaming is not supported.",
                    "type": "invalid_request_error",
                    "param": "stream",
                    "code": null
                }
            })
        );
        let error = value["error"].as_object().unwrap();
        assert!(error.contains_key("code"), "code must be explicit null");
    }
}
