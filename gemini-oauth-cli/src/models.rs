//! Serde types for the Gemini `GenerateContent` request/response shapes.
//! These match the public Generative Language API schema; the Code Assist
//! backend wraps the same schema inside `{ "request": ... }` envelopes.

use serde::{Deserialize, Serialize};

/// Inline (base64-encoded) file payload — images, PDFs, text, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineData {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// Base64-encoded bytes.
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<InlineData>,
    /// Present on response parts when the model returns its reasoning
    /// ("thoughts"); we never send this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
}

impl Part {
    pub fn text(s: impl Into<String>) -> Self {
        Part { text: Some(s.into()), ..Default::default() }
    }
    pub fn inline(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Part {
            inline_data: Some(InlineData { mime_type: mime_type.into(), data: data.into() }),
            ..Default::default()
        }
    }
    pub fn is_thought(&self) -> bool {
        self.thought.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

/// Controls the model's internal reasoning ("thinking") for Gemini 2.5.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// Token budget for reasoning. `-1` = dynamic, `0` = disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    /// Ask the backend to return the reasoning as `thought` parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
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
    fn parts(&self) -> &[Part] {
        self.candidates
            .first()
            .and_then(|c| c.content.as_ref())
            .map(|content| content.parts.as_slice())
            .unwrap_or(&[])
    }

    /// Text segments of the first candidate, tagged with whether each is a
    /// reasoning ("thought") part. Order is preserved.
    pub fn text_segments(&self) -> Vec<(bool, String)> {
        self.parts()
            .iter()
            .filter_map(|p| p.text.as_ref().map(|t| (p.is_thought(), t.clone())))
            .collect()
    }

    /// Just the answer text (excludes reasoning parts).
    pub fn answer_text(&self) -> String {
        self.parts()
            .iter()
            .filter(|p| !p.is_thought())
            .filter_map(|p| p.text.clone())
            .collect()
    }

    /// Just the reasoning text, if any was returned.
    pub fn thought_text(&self) -> String {
        self.parts()
            .iter()
            .filter(|p| p.is_thought())
            .filter_map(|p| p.text.clone())
            .collect()
    }
}
