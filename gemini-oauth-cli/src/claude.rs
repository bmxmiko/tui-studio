//! Claude.ai access over OAuth (no API key), reusing the public Claude Code
//! OAuth client. This authorizes against the user's Claude subscription and
//! calls the Anthropic Messages API as Claude Code does.
//!
//! NOTE: Anthropic gates this behind the `oauth-2025-04-20` beta and expects
//! the Claude Code identity as the first system block. This is more fragile
//! and more ToS-sensitive than the Gemini path, and Anthropic may restrict it
//! at any time. Use only with your own account.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::StreamExt;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;

use crate::config::{now_secs, Credentials, Store};
use crate::provider::{GenOptions, Message, Role};

// Public Claude Code OAuth client — embedded in a distributed app, only grants
// access on behalf of the user who completes the interactive consent.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20,claude-code-20250219";
// Required first system block for OAuth/Claude-Code access to be accepted.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;
const B64STD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

// --- OAuth (PKCE + manual code paste) ---

fn random_b64url(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    B64URL.encode(bytes)
}

fn pkce() -> (String, String) {
    let verifier = random_b64url(32);
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub async fn login(store: &mut Store) -> Result<()> {
    let (verifier, challenge) = pkce();
    let state = random_b64url(24);

    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .finish();
    let auth_url = format!("{AUTHORIZE_URL}?{query}");

    println!("Otwieram przeglądarkę, aby zalogować się do Claude…");
    println!("Jeśli nic się nie otworzyło, wklej ten adres ręcznie:\n\n{auth_url}\n");
    let _ = open::that(&auth_url);

    println!(
        "Po zalogowaniu zobaczysz kod autoryzacyjny. Skopiuj go i wklej tutaj."
    );
    print!("Kod: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    anyhow::ensure!(!line.is_empty(), "nie podano kodu");

    // The pasted value is usually "<code>#<state>".
    let (code, ret_state) = match line.split_once('#') {
        Some((c, s)) => (c, s),
        None => (line, state.as_str()),
    };

    let creds = exchange(code, ret_state, &verifier).await?;
    store.claude.credentials = Some(creds);
    store.save()?;
    println!("✓ Zalogowano do Claude. Tokeny zapisane lokalnie.");
    Ok(())
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

async fn exchange(code: &str, state: &str, verifier: &str) -> Result<Credentials> {
    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": state,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });
    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("exchanging authorization code for tokens")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("token exchange failed ({status}): {text}");
    }
    let token: TokenResponse = resp.json().await?;
    Ok(to_creds(token, None))
}

async fn refresh(creds: &Credentials) -> Result<Credentials> {
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": creds.refresh_token,
        "client_id": CLIENT_ID,
    });
    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("refreshing access token")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("token refresh failed ({status}): {text} — uruchom `gemini -p claude login` ponownie");
    }
    let token: TokenResponse = resp.json().await?;
    Ok(to_creds(token, Some(creds)))
}

fn to_creds(token: TokenResponse, prev: Option<&Credentials>) -> Credentials {
    Credentials {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .or_else(|| prev.map(|c| c.refresh_token.clone()))
            .unwrap_or_default(),
        expiry: now_secs() + token.expires_in,
        token_type: token.token_type.unwrap_or_else(|| "Bearer".into()),
        scope: token.scope.or_else(|| prev.and_then(|c| c.scope.clone())),
    }
}

/// Return a valid Claude access token, refreshing and persisting if needed.
pub async fn valid_access_token(store: &mut Store) -> Result<String> {
    let creds = store
        .claude
        .credentials
        .clone()
        .ok_or_else(|| anyhow!("nie jesteś zalogowany (Claude) — uruchom `gemini -p claude login`"))?;

    if creds.is_expired() && !creds.refresh_token.is_empty() {
        let refreshed = refresh(&creds).await?;
        let token = refreshed.access_token.clone();
        store.claude.credentials = Some(refreshed);
        store.save()?;
        Ok(token)
    } else {
        Ok(creds.access_token)
    }
}

// --- Messages API client ---

pub struct ClaudeClient {
    http: reqwest::Client,
    token: String,
}

impl ClaudeClient {
    pub fn new(token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("claude-cli/1.0.0 (external; gemini-oauth-cli)")
            .build()?;
        Ok(ClaudeClient { http, token })
    }

    fn build_body(
        &self,
        model: &str,
        system: Option<&str>,
        history: &[Message],
        opts: &GenOptions,
        stream: bool,
    ) -> Value {
        let messages: Vec<Value> = history
            .iter()
            .map(|m| {
                let role = if m.role == Role::User { "user" } else { "assistant" };
                let mut content: Vec<Value> = Vec::new();
                for f in &m.files {
                    content.push(file_block(f));
                }
                if !m.text.is_empty() {
                    content.push(json!({ "type": "text", "text": m.text }));
                }
                json!({ "role": role, "content": content })
            })
            .collect();

        // The Claude Code identity must come first; the user's system prompt
        // (if any) follows as a second block.
        let mut system_blocks = vec![json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY })];
        if let Some(s) = system {
            if !s.is_empty() {
                system_blocks.push(json!({ "type": "text", "text": s }));
            }
        }

        let mut body = json!({
            "model": model,
            "max_tokens": opts.max_tokens,
            "system": system_blocks,
            "messages": messages,
            "stream": stream,
        });

        // Extended thinking: enabled when a positive budget is given (min 1024).
        // Temperature is not allowed together with thinking.
        let thinking_on = opts.thinking.map(|b| b > 0).unwrap_or(false);
        if thinking_on {
            let budget = opts.thinking.unwrap().max(1024) as u64;
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            if body["max_tokens"].as_u64().unwrap_or(0) <= budget {
                body["max_tokens"] = json!(budget + 4096);
            }
        } else if let Some(t) = opts.temperature {
            body["temperature"] = json!(t);
        }

        body
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        self.http
            .post(MESSAGES_URL)
            .bearer_auth(&self.token)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true")
            .json(body)
    }

    pub async fn complete(
        &self,
        model: &str,
        system: Option<&str>,
        history: &[Message],
        opts: &GenOptions,
        stream: bool,
    ) -> Result<String> {
        let body = self.build_body(model, system, history, opts, stream);
        if stream {
            self.stream_req(body, opts.show_thoughts).await
        } else {
            self.generate_req(body, opts.show_thoughts).await
        }
    }

    async fn generate_req(&self, body: Value, show_thoughts: bool) -> Result<String> {
        let resp = self.request(&body).send().await.context("calling Messages API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Messages API failed ({status}): {text}");
        }
        let v: Value = resp.json().await?;
        let mut answer = String::new();
        let mut thoughts = String::new();
        if let Some(blocks) = v["content"].as_array() {
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => answer.push_str(b["text"].as_str().unwrap_or("")),
                    Some("thinking") => thoughts.push_str(b["thinking"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
        }
        if show_thoughts && !thoughts.is_empty() {
            println!("\x1b[2m💭 thinking:\n{thoughts}\x1b[0m\n");
        }
        println!("{answer}");
        Ok(answer)
    }

    async fn stream_req(&self, body: Value, show_thoughts: bool) -> Result<String> {
        let resp = self.request(&body).send().await.context("calling Messages API (stream)")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Messages API failed ({status}): {text}");
        }

        let mut full = String::new();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        let stdout = std::io::stdout();
        let mut in_thoughts = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading SSE stream")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].trim_end_matches('\r').to_string();
                buf.drain(..=idx);

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if event["type"].as_str() != Some("content_block_delta") {
                    continue;
                }
                let delta = &event["delta"];
                let mut lock = stdout.lock();
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if in_thoughts {
                            let _ = lock.write_all(b"\x1b[0m\n\n");
                            in_thoughts = false;
                        }
                        let piece = delta["text"].as_str().unwrap_or("");
                        full.push_str(piece);
                        let _ = lock.write_all(piece.as_bytes());
                        let _ = lock.flush();
                    }
                    Some("thinking_delta") if show_thoughts => {
                        if !in_thoughts {
                            let _ = lock.write_all("\x1b[2m💭 thinking:\n".as_bytes());
                            in_thoughts = true;
                        }
                        let _ = lock.write_all(delta["thinking"].as_str().unwrap_or("").as_bytes());
                        let _ = lock.flush();
                    }
                    _ => {}
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

/// Map an attachment to a Messages API content block.
fn file_block(f: &crate::provider::Attachment) -> Value {
    if f.mime.starts_with("image/") {
        json!({
            "type": "image",
            "source": { "type": "base64", "media_type": f.mime, "data": f.data_b64 }
        })
    } else if f.mime == "application/pdf" {
        json!({
            "type": "document",
            "source": { "type": "base64", "media_type": "application/pdf", "data": f.data_b64 }
        })
    } else {
        // Treat anything else as text: decode and inline as a text block.
        let text = B64STD
            .decode(f.data_b64.as_bytes())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        json!({ "type": "text", "text": text })
    }
}
