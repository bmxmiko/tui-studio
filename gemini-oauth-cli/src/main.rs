//! gemini-oauth-cli — talk to Google Gemini using only an OAuth login
//! (no API key), via the Code Assist backend, exposed as a simple CLI/"API".

mod auth;
mod codeassist;
mod config;
mod models;

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use codeassist::CodeAssist;
use config::Store;
use models::{
    Content, GenerateContentRequest, GenerationConfig, Part, ThinkingConfig,
};

const DEFAULT_MODEL: &str = "gemini-2.5-flash";

#[derive(Parser)]
#[command(
    name = "gemini",
    version,
    about = "CLI Gemini przez OAuth (bez API key), oparte na backendzie Code Assist"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Zaloguj się do Google (OAuth, otwiera przeglądarkę).
    Login,
    /// Usuń zapisane tokeny i cache projektu.
    Logout,
    /// Pokaż stan logowania i wykryty projekt.
    Status,
    /// Zadaj jedno pytanie. Prompt można też podać przez stdin (pipe).
    Ask {
        /// Treść promptu (jeśli pominięta, czytana jest z stdin).
        prompt: Vec<String>,
        /// Model, np. gemini-2.5-flash lub gemini-2.5-pro.
        #[arg(short, long, default_value = DEFAULT_MODEL)]
        model: String,
        /// Dołącz plik (obraz/PDF/tekst). Można podać wielokrotnie: -f a.png -f b.pdf
        #[arg(short, long = "file", value_name = "ŚCIEŻKA")]
        files: Vec<PathBuf>,
        /// Instrukcja systemowa.
        #[arg(short, long)]
        system: Option<String>,
        /// Temperatura próbkowania.
        #[arg(short, long)]
        temperature: Option<f32>,
        /// Budżet myślenia w tokenach: -1 = dynamiczny, 0 = wyłączone.
        #[arg(long, value_name = "TOKENY")]
        thinking: Option<i32>,
        /// Pokaż tok rozumowania modelu (wymaga modelu 2.5).
        #[arg(long)]
        show_thoughts: bool,
        /// Wyłącz streaming (czekaj na pełną odpowiedź).
        #[arg(long)]
        no_stream: bool,
    },
    /// Interaktywny czat (REPL) z pamięcią rozmowy.
    Chat {
        #[arg(short, long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(short, long)]
        system: Option<String>,
        /// Budżet myślenia w tokenach: -1 = dynamiczny, 0 = wyłączone.
        #[arg(long, value_name = "TOKENY")]
        thinking: Option<i32>,
        /// Pokaż tok rozumowania modelu.
        #[arg(long)]
        show_thoughts: bool,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Błąd: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut store = Store::load()?;

    match cli.command {
        Command::Login => auth::login(&mut store).await?,
        Command::Logout => {
            Store::clear()?;
            println!("✓ Wylogowano (usunięto lokalne tokeny i cache).");
        }
        Command::Status => status(&store),
        Command::Ask {
            prompt,
            model,
            files,
            system,
            temperature,
            thinking,
            show_thoughts,
            no_stream,
        } => {
            // Prompt is optional when files are attached.
            let prompt = resolve_prompt(prompt, files.is_empty())?;
            let parts = build_parts(&prompt, &files)?;
            ask(
                &mut store,
                &model,
                system,
                temperature,
                thinking,
                show_thoughts,
                !no_stream,
                parts,
            )
            .await?;
        }
        Command::Chat { model, system, thinking, show_thoughts } => {
            chat(&mut store, &model, system, thinking, show_thoughts).await?
        }
    }
    Ok(())
}

fn status(store: &Store) {
    match &store.credentials {
        Some(c) => {
            let remaining = c.expiry.saturating_sub(config::now_secs());
            println!("Zalogowany: tak");
            println!("Access token wygasa za: {remaining}s");
            println!(
                "Scope: {}",
                c.scope.clone().unwrap_or_else(|| "(brak)".into())
            );
        }
        None => println!("Zalogowany: nie (uruchom `gemini login`)"),
    }
    match &store.project_id {
        Some(p) => println!("Projekt Code Assist: {p}"),
        None => println!("Projekt Code Assist: (jeszcze nie wykryty)"),
    }
}

/// Use the provided args, or fall back to reading the prompt from stdin so the
/// tool can be used in pipes like an API: `echo "hi" | gemini ask`.
/// `required` is false when files are attached (a prompt is then optional).
fn resolve_prompt(parts: Vec<String>, required: bool) -> Result<String> {
    if !parts.is_empty() {
        return Ok(parts.join(" "));
    }
    // Only read stdin when it's piped, so we don't block an interactive shell
    // when the user just wants to ask about a file.
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let buf = buf.trim().to_string();
        if !buf.is_empty() {
            return Ok(buf);
        }
    }
    anyhow::ensure!(
        !required,
        "brak promptu (podaj argument, przekaż przez stdin albo dołącz plik -f)"
    );
    Ok(String::new())
}

/// Best-effort MIME type from a file extension.
fn guess_mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "xml" => "text/xml",
        "txt" | "log" | "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "cpp" | "h"
        | "toml" | "yaml" | "yml" | "sh" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Build the user message parts: attached files first, then the text prompt.
fn build_parts(prompt: &str, files: &[PathBuf]) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    for path in files {
        let bytes = std::fs::read(path)
            .with_context(|| format!("czytanie pliku {}", path.display()))?;
        let mime = guess_mime(path);
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        eprintln!(
            "↑ dołączono {} ({}, {} B)",
            path.display(),
            mime,
            bytes.len()
        );
        parts.push(Part::inline(mime, data));
    }
    if !prompt.is_empty() {
        parts.push(Part::text(prompt));
    }
    anyhow::ensure!(!parts.is_empty(), "brak treści do wysłania");
    Ok(parts)
}

fn build_request(
    contents: Vec<Content>,
    system: &Option<String>,
    temperature: Option<f32>,
    thinking: Option<i32>,
    show_thoughts: bool,
) -> GenerateContentRequest {
    let thinking_config = (thinking.is_some() || show_thoughts).then(|| ThinkingConfig {
        thinking_budget: thinking,
        include_thoughts: show_thoughts.then_some(true),
    });

    let generation_config = (temperature.is_some() || thinking_config.is_some()).then(|| {
        GenerationConfig {
            temperature,
            thinking_config,
            ..Default::default()
        }
    });

    GenerateContentRequest {
        contents,
        system_instruction: system.as_ref().map(|s| Content {
            role: "user".into(),
            parts: vec![Part::text(s)],
        }),
        generation_config,
    }
}

#[allow(clippy::too_many_arguments)]
async fn ask(
    store: &mut Store,
    model: &str,
    system: Option<String>,
    temperature: Option<f32>,
    thinking: Option<i32>,
    show_thoughts: bool,
    stream: bool,
    parts: Vec<Part>,
) -> Result<()> {
    let token = auth::valid_access_token(store).await?;
    let client = CodeAssist::new(token, store).await?;
    let request = build_request(
        vec![Content::user_parts(parts)],
        &system,
        temperature,
        thinking,
        show_thoughts,
    );

    if stream {
        client.stream(model, &request, show_thoughts).await?;
    } else {
        let resp = client.generate(model, &request).await?;
        if show_thoughts {
            let thoughts = resp.thought_text();
            if !thoughts.is_empty() {
                println!("\x1b[2m💭 thinking:\n{thoughts}\x1b[0m\n");
            }
        }
        println!("{}", resp.answer_text());
    }
    Ok(())
}

async fn chat(
    store: &mut Store,
    model: &str,
    system: Option<String>,
    thinking: Option<i32>,
    show_thoughts: bool,
) -> Result<()> {
    let token = auth::valid_access_token(store).await?;
    let client = CodeAssist::new(token, store).await?;

    println!("Czat z {model}. /exit wyjście, /reset czyści kontekst, /file <ścieżka> dołącza plik.\n");
    let mut history: Vec<Content> = Vec::new();
    let mut pending_files: Vec<PathBuf> = Vec::new();
    let stdin = std::io::stdin();

    loop {
        print!("» ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "/exit" | "/quit" => break,
            "/reset" => {
                history.clear();
                pending_files.clear();
                println!("(kontekst wyczyszczony)\n");
                continue;
            }
            _ if line.starts_with("/file ") => {
                let path = PathBuf::from(line[6..].trim());
                if path.exists() {
                    println!("(plik dołączony do następnej wiadomości: {})\n", path.display());
                    pending_files.push(path);
                } else {
                    println!("(nie znaleziono pliku: {})\n", path.display());
                }
                continue;
            }
            _ => {}
        }

        let parts = build_parts(line, &pending_files)?;
        pending_files.clear();
        history.push(Content::user_parts(parts));

        let request = build_request(history.clone(), &system, None, thinking, show_thoughts);
        let answer = client.stream(model, &request, show_thoughts).await?;
        history.push(Content::model(answer));
        println!();
    }
    Ok(())
}
