//! Serde types for the Gemini `GenerateContent` request/response shapes.
//! These match the public Generative Language API schema; the Code Assist
//! backend wraps the same schema inside `{ "request": ... }` envelopes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Part {
    pub fn text(s: impl Into<String>) -> Self {
        Part { text: Some(s.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

impl Content {
    pub fn user(text: impl Into<String>) -> Self {
        Content { role: "user".into(), parts: vec![Part::text(text)] }
    }
    pub fn model(text: impl Into<String>) -> Self {
        Content { role: "model".into(), parts: vec![Part::text(text)] }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// The inner Gemini request (the part Code Assist wraps).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
}

// --- Responses ---

#[derive(Debug, Clone, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default, rename = "finishReason")]
    #[allow(dead_code)] // kept for completeness / debugging
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

impl GenerateContentResponse {
    /// Concatenate all text parts of the first candidate.
    pub fn text(&self) -> String {
        self.candidates
            .first()
            .and_then(|c| c.content.as_ref())
            .map(|content| {
                content
                    .parts
                    .iter()
                    .filter_map(|p| p.text.clone())
                    .collect::<String>()
            })
            .unwrap_or_default()
    }
}
