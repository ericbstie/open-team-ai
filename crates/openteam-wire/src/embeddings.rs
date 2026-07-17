//! The embeddings wire subset and the base64 f32-LE codec (ADR 0014/0018).
//!
//! Unknown-field posture is asymmetric, tracking the spec: the embeddings
//! request derives `deny_unknown_fields` (spec `additionalProperties: false` →
//! the mock 400s on stray fields) while chat and all responses accept-and-ignore.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

/// `POST /v1/embeddings` request body. Strict: unknown fields are denied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: EmbeddingInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<EncodingFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// The two input forms the harness speaks (token-array forms are out of the
/// subset; the mock 400s cleanly on them because they fail this union).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Text(String),
    Texts(Vec<String>),
}

impl EmbeddingInput {
    /// The inputs as a slice-of-texts view, single input first.
    pub fn texts(&self) -> Vec<&str> {
        match self {
            Self::Text(text) => vec![text.as_str()],
            Self::Texts(texts) => texts.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodingFormat {
    Float,
    Base64,
}

/// `POST /v1/embeddings` response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    /// Always `"list"`.
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    /// Always `"embedding"`.
    pub object: String,
    /// Position in the input array.
    pub index: u32,
    pub embedding: EmbeddingVector,
}

/// Base64 f32-LE by default (the openai-python posture, ADR 0018) or a plain
/// float array when `encoding_format: "float"` is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingVector {
    Base64(String),
    Float(Vec<f32>),
}

impl EmbeddingVector {
    /// Decode into a float vector regardless of the wire encoding.
    pub fn to_floats(&self) -> Result<Vec<f32>, CodecError> {
        match self {
            Self::Base64(encoded) => decode_f32_le(encoded),
            Self::Float(floats) => Ok(floats.clone()),
        }
    }
}

/// Embeddings usage has no `completion_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

/// A base64 f32-LE decode fault.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("byte length {0} is not a multiple of 4")]
    Truncated(usize),
}

/// Encode a float vector as base64 over its little-endian f32 bytes.
pub fn encode_f32_le(vector: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    BASE64.encode(bytes)
}

/// Decode a base64 string into the float vector of its little-endian f32 bytes.
pub fn decode_f32_le(encoded: &str) -> Result<Vec<f32>, CodecError> {
    let bytes = BASE64.decode(encoded)?;
    if bytes.len() % 4 != 0 {
        return Err(CodecError::Truncated(bytes.len()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_f32_le_round_trips() {
        let vector = vec![0.0_f32, 1.0, -1.0, 0.5, f32::MIN_POSITIVE, 12345.678];
        let encoded = encode_f32_le(&vector);
        let decoded = decode_f32_le(&encoded).unwrap();
        assert_eq!(decoded, vector);
    }

    #[test]
    fn decode_rejects_non_multiple_of_four() {
        let encoded = BASE64.encode([1_u8, 2, 3]);
        assert!(matches!(
            decode_f32_le(&encoded),
            Err(CodecError::Truncated(3))
        ));
    }

    #[test]
    fn decode_rejects_bad_base64() {
        assert!(matches!(
            decode_f32_le("!!not-base64!!"),
            Err(CodecError::Base64(_))
        ));
    }

    #[test]
    fn known_bytes_decode_little_endian() {
        // 1.0f32 LE = [0, 0, 128, 63] — a worked example, not a re-derivation.
        let encoded = BASE64.encode([0_u8, 0, 128, 63]);
        assert_eq!(decode_f32_le(&encoded).unwrap(), vec![1.0_f32]);
    }

    #[test]
    fn request_denies_unknown_fields() {
        let raw = json!({
            "model": "m",
            "input": "text",
            "stray": true
        });
        let result: Result<EmbeddingRequest, _> = serde_json::from_value(raw);
        assert!(result.is_err(), "stray fields must be a deserialize error");
    }

    #[test]
    fn request_accepts_both_input_forms() {
        let single: EmbeddingRequest =
            serde_json::from_value(json!({"model": "m", "input": "one"})).unwrap();
        assert_eq!(single.input.texts(), vec!["one"]);
        let many: EmbeddingRequest =
            serde_json::from_value(json!({"model": "m", "input": ["a", "b"]})).unwrap();
        assert_eq!(many.input.texts(), vec!["a", "b"]);
    }

    #[test]
    fn request_rejects_token_array_input() {
        let raw = json!({"model": "m", "input": [1, 2, 3]});
        assert!(serde_json::from_value::<EmbeddingRequest>(raw).is_err());
    }

    #[test]
    fn vector_union_deserializes_both_encodings() {
        let b64: EmbeddingVector = serde_json::from_value(json!("AACAPw==")).unwrap();
        assert_eq!(b64.to_floats().unwrap(), vec![1.0_f32]);
        let floats: EmbeddingVector = serde_json::from_value(json!([0.25, -0.5])).unwrap();
        assert_eq!(floats.to_floats().unwrap(), vec![0.25_f32, -0.5]);
    }

    #[test]
    fn encoding_format_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(EncodingFormat::Base64).unwrap(),
            json!("base64")
        );
    }
}
