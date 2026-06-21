//! gemini-oauth-cli — talk to Google Gemini using only an OAuth login
//! (no API key), via the Code Assist backend, exposed as a simple CLI/"API".

mod auth;
mod codeassist;
mod config;
mod models;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{Read, Write};

use codeassist::CodeAssist;
use config::Store;
use models::{Content, GenerateContentRequest, GenerationConfig};

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
        /// Instrukcja systemowa.
        #[arg(short, long)]
        system: Option<String>,
        /// Temperatura próbkowania.
        #[arg(short, long)]
        temperature: Option<f32>,
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
        Command::Ask { prompt, model, system, temperature, no_stream } => {
            let prompt = resolve_prompt(prompt)?;
            ask(&mut store, &model, system, temperature, !no_stream, prompt).await?;
        }
        Command::Chat { model, system } => chat(&mut store, &model, system).await?,
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
fn resolve_prompt(parts: Vec<String>) -> Result<String> {
    if !parts.is_empty() {
        return Ok(parts.join(" "));
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let buf = buf.trim().to_string();
    anyhow::ensure!(!buf.is_empty(), "brak promptu (podaj argument lub przekaż przez stdin)");
    Ok(buf)
}

fn build_request(
    contents: Vec<Content>,
    system: &Option<String>,
    temperature: Option<f32>,
) -> GenerateContentRequest {
    GenerateContentRequest {
        contents,
        system_instruction: system.as_ref().map(|s| Content {
            role: "user".into(),
            parts: vec![models::Part::text(s)],
        }),
        generation_config: temperature.map(|t| GenerationConfig {
            temperature: Some(t),
            ..Default::default()
        }),
    }
}

async fn ask(
    store: &mut Store,
    model: &str,
    system: Option<String>,
    temperature: Option<f32>,
    stream: bool,
    prompt: String,
) -> Result<()> {
    let token = auth::valid_access_token(store).await?;
    let client = CodeAssist::new(token, store).await?;
    let request = build_request(vec![Content::user(prompt)], &system, temperature);

    if stream {
        client.stream(model, &request).await?;
    } else {
        let resp = client.generate(model, &request).await?;
        println!("{}", resp.text());
    }
    Ok(())
}

async fn chat(store: &mut Store, model: &str, system: Option<String>) -> Result<()> {
    let token = auth::valid_access_token(store).await?;
    let client = CodeAssist::new(token, store).await?;

    println!("Czat z {model}. Wpisz pytanie. /exit aby wyjść, /reset aby wyczyścić kontekst.\n");
    let mut history: Vec<Content> = Vec::new();
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
                println!("(kontekst wyczyszczony)\n");
                continue;
            }
            _ => {}
        }

        history.push(Content::user(line));
        let request = build_request(history.clone(), &system, None);
        let answer = client.stream(model, &request).await?;
        history.push(Content::model(answer));
        println!();
    }
    Ok(())
}
