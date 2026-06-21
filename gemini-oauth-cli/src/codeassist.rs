//! Client for Google's internal Code Assist endpoint
//! (`cloudcode-pa.googleapis.com`). This is the same backend the official
//! gemini-cli uses to offer Gemini access via a plain Google login instead of
//! a Generative Language API key.
//!
//! The backend wraps the normal Gemini request in an envelope that also
//! carries the resolved GCP/Code-Assist project id, which we discover once via
//! `loadCodeAssist` + `onboardUser` and then cache.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;

use crate::config::Store;
use crate::models::{GenerateContentRequest, GenerateContentResponse};

const BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal";

pub struct CodeAssist {
    http: reqwest::Client,
    token: String,
    project_id: String,
}

fn client_metadata() -> Value {
    json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    })
}

impl CodeAssist {
    /// Build a client, resolving (and caching) the Code Assist project id.
    pub async fn new(token: String, store: &mut Store) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("gemini-oauth-cli/0.1 (rust)")
            .build()?;

        let project_id = match store.gemini.project_id.clone() {
            Some(p) => p,
            None => {
                let p = discover_project(&http, &token).await?;
                store.gemini.project_id = Some(p.clone());
                store.save()?;
                p
            }
        };

        Ok(CodeAssist { http, token, project_id })
    }

    fn envelope(&self, model: &str, request: &GenerateContentRequest) -> Value {
        json!({
            "model": model,
            "project": self.project_id,
            "request": request,
        })
    }

    /// One-shot, non-streaming generation.
    pub async fn generate(
        &self,
        model: &str,
        request: &GenerateContentRequest,
    ) -> Result<GenerateContentResponse> {
        let url = format!("{BASE}:generateContent");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&self.envelope(model, request))
            .send()
            .await
            .context("calling generateContent")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("generateContent failed ({status}): {text}");
        }

        // Code Assist wraps the payload as { "response": { ... } }.
        let wrapped: Wrapped<GenerateContentResponse> = resp.json().await?;
        Ok(wrapped.response)
    }

    /// Streaming generation; prints text chunks to stdout as they arrive and
    /// returns the full concatenated answer text. Reasoning ("thought") parts
    /// are printed dimmed only when `show_thoughts` is set, and are never part
    /// of the returned answer.
    pub async fn stream(
        &self,
        model: &str,
        request: &GenerateContentRequest,
        show_thoughts: bool,
    ) -> Result<String> {
        let url = format!("{BASE}:streamGenerateContent?alt=sse");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&self.envelope(model, request))
            .send()
            .await
            .context("calling streamGenerateContent")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("streamGenerateContent failed ({status}): {text}");
        }

        let mut full = String::new();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        let stdout = std::io::stdout();
        // Track whether we're inside the dimmed "thoughts" section so we can
        // print a header once and reset styling when the answer begins.
        let mut in_thoughts = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading SSE stream")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines; keep the trailing partial in `buf`.
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].trim_end_matches('\r').to_string();
                buf.drain(..=idx);

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(wrapped) =
                    serde_json::from_str::<Wrapped<GenerateContentResponse>>(data)
                else {
                    continue;
                };

                let mut lock = stdout.lock();
                for (is_thought, piece) in wrapped.response.text_segments() {
                    if piece.is_empty() {
                        continue;
                    }
                    if is_thought {
                        if !show_thoughts {
                            continue;
                        }
                        if !in_thoughts {
                            let _ = lock.write_all(b"\x1b[2m\xf0\x9f\x92\xad thinking:\n");
                            in_thoughts = true;
                        }
                        let _ = lock.write_all(piece.as_bytes());
                    } else {
                        if in_thoughts {
                            // Close the dimmed section before the real answer.
                            let _ = lock.write_all(b"\x1b[0m\n\n");
                            in_thoughts = false;
                        }
                        full.push_str(&piece);
                        let _ = lock.write_all(piece.as_bytes());
                    }
                    let _ = lock.flush();
                }
            }
        }
        if in_thoughts {
            print!("\x1b[0m");
        }
        println!();
        Ok(full)
    }
}

#[derive(Deserialize)]
struct Wrapped<T> {
    response: T,
}

#[derive(Serialize)]
struct LoadCodeAssistRequest {
    #[serde(rename = "cloudaicompanionProject", skip_serializing_if = "Option::is_none")]
    cloudaicompanion_project: Option<String>,
    metadata: Value,
}

#[derive(Deserialize)]
struct AllowedTier {
    id: String,
    #[serde(default, rename = "isDefault")]
    is_default: bool,
}

#[derive(Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
    #[serde(default, rename = "allowedTiers")]
    allowed_tiers: Vec<AllowedTier>,
}

/// Resolve the Code Assist project id for the logged-in user, provisioning a
/// free-tier project via onboarding when necessary.
async fn discover_project(http: &reqwest::Client, token: &str) -> Result<String> {
    // Allow overriding (paid/Workspace users with an existing GCP project).
    let preset = std::env::var("GOOGLE_CLOUD_PROJECT").ok().filter(|s| !s.is_empty());

    let load: LoadCodeAssistResponse = {
        let body = LoadCodeAssistRequest {
            cloudaicompanion_project: preset.clone(),
            metadata: client_metadata(),
        };
        let resp = http
            .post(format!("{BASE}:loadCodeAssist"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("calling loadCodeAssist")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("loadCodeAssist failed ({status}): {text}");
        }
        resp.json().await?
    };

    // Already provisioned? Done.
    if let Some(p) = load.cloudaicompanion_project.or(preset.clone()) {
        if !p.is_empty() {
            return Ok(p);
        }
    }

    // Otherwise onboard. Pick the default tier (free-tier for personal accounts).
    let tier_id = load
        .allowed_tiers
        .iter()
        .find(|t| t.is_default)
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "free-tier".to_string());

    onboard(http, token, &tier_id, preset).await
}

async fn onboard(
    http: &reqwest::Client,
    token: &str,
    tier_id: &str,
    project: Option<String>,
) -> Result<String> {
    let body = json!({
        "tierId": tier_id,
        "cloudaicompanionProject": project,
        "metadata": client_metadata(),
    });

    // onboardUser returns a long-running operation; poll until done.
    for _ in 0..30 {
        let resp = http
            .post(format!("{BASE}:onboardUser"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("calling onboardUser")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("onboardUser failed ({status}): {text}");
        }

        let op: Value = resp.json().await?;
        if op.get("done").and_then(Value::as_bool).unwrap_or(false) {
            let id = op
                .pointer("/response/cloudaicompanionProject/id")
                .and_then(Value::as_str)
                .map(str::to_string);
            return id.ok_or_else(|| anyhow!("onboardUser finished but returned no project id"));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    bail!("onboardUser did not complete in time — spróbuj ponownie")
}
