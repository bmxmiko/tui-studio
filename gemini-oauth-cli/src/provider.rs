//! Provider-neutral chat abstraction. The CLI builds these neutral types and
//! dispatches to a concrete backend (Gemini Code Assist or Claude Messages),
//! each of which converts to its own wire format, prints output, and returns
//! the assistant's answer text.

use anyhow::Result;

use crate::claude::ClaudeClient;
use crate::codeassist::CodeAssist;
use crate::models;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Model,
}

#[derive(Clone)]
pub struct Attachment {
    pub mime: String,
    /// Base64-encoded file bytes.
    pub data_b64: String,
}

#[derive(Clone)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub files: Vec<Attachment>,
}

/// Generation knobs shared across providers (mapped per-backend).
pub struct GenOptions {
    pub temperature: Option<f32>,
    /// Thinking budget in tokens. Semantics differ per provider:
    /// Gemini: -1 dynamic, 0 off. Claude: >0 enables (min 1024), else off.
    pub thinking: Option<i32>,
    pub show_thoughts: bool,
    pub max_tokens: u32,
}

/// A ready-to-use, authenticated client for one provider.
pub enum Client {
    Gemini(CodeAssist),
    Claude(ClaudeClient),
}

impl Client {
    /// Run one completion against `history`. Streams to stdout when `stream`
    /// is set; returns the assistant's answer text (without reasoning).
    pub async fn complete(
        &self,
        model: &str,
        system: Option<&str>,
        history: &[Message],
        opts: &GenOptions,
        stream: bool,
    ) -> Result<String> {
        match self {
            Client::Gemini(c) => gemini_complete(c, model, system, history, opts, stream).await,
            Client::Claude(c) => c.complete(model, system, history, opts, stream).await,
        }
    }
}

/// Convert neutral messages into a Gemini request and run it.
async fn gemini_complete(
    client: &CodeAssist,
    model: &str,
    system: Option<&str>,
    history: &[Message],
    opts: &GenOptions,
    stream: bool,
) -> Result<String> {
    use models::{Content, GenerateContentRequest, GenerationConfig, Part, ThinkingConfig};

    let contents: Vec<Content> = history
        .iter()
        .map(|m| {
            let role = if m.role == Role::User { "user" } else { "model" };
            let mut parts = Vec::new();
            for f in &m.files {
                parts.push(Part::inline(f.mime.clone(), f.data_b64.clone()));
            }
            if !m.text.is_empty() {
                parts.push(Part::text(m.text.clone()));
            }
            Content { role: role.into(), parts }
        })
        .collect();

    let thinking_config = (opts.thinking.is_some() || opts.show_thoughts).then(|| ThinkingConfig {
        thinking_budget: opts.thinking,
        include_thoughts: opts.show_thoughts.then_some(true),
    });
    let generation_config = (opts.temperature.is_some() || thinking_config.is_some()).then(|| {
        GenerationConfig {
            temperature: opts.temperature,
            thinking_config,
            ..Default::default()
        }
    });

    let request = GenerateContentRequest {
        contents,
        system_instruction: system.map(|s| Content {
            role: "user".into(),
            parts: vec![Part::text(s)],
        }),
        generation_config,
    };

    if stream {
        client.stream(model, &request, opts.show_thoughts).await
    } else {
        let resp = client.generate(model, &request).await?;
        if opts.show_thoughts {
            let thoughts = resp.thought_text();
            if !thoughts.is_empty() {
                println!("\x1b[2m💭 thinking:\n{thoughts}\x1b[0m\n");
            }
        }
        let answer = resp.answer_text();
        println!("{answer}");
        Ok(answer)
    }
}
