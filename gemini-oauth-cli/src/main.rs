//! gemini-oauth-cli — talk to Google Gemini *and* Claude using only an OAuth
//! login (no API key), exposed as a simple CLI/"API". Gemini goes through the
//! Code Assist backend; Claude goes through the Claude Code OAuth flow.

mod auth;
mod claude;
mod codeassist;
mod config;
mod models;
mod provider;

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use claude::ClaudeClient;
use codeassist::CodeAssist;
use config::{Gem, ProviderKind, Store};
use provider::{Attachment, Client, GenOptions, Message, Role};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

#[derive(Parser)]
#[command(
    name = "gemini",
    version,
    about = "CLI do Gemini i Claude przez OAuth (bez API key)"
)]
struct Cli {
    /// Wybór dostawcy: gemini (Code Assist) lub claude (Claude Code OAuth).
    /// Domyślnie gemini (chyba że gem wskazuje innego).
    #[arg(short, long, global = true, value_enum)]
    provider: Option<ProviderKind>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Zaloguj się (OAuth, otwiera przeglądarkę).
    Login {
        /// Tryb bezgłowy: tylko wypisz URL i zapisz stan (nie otwieraj przeglądarki).
        #[arg(long)]
        no_browser: bool,
        /// Dokończ logowanie wklejonym kodem (lub URL-em przekierowania).
        #[arg(long, value_name = "KOD")]
        code: Option<String>,
    },
    /// Usuń zapisane tokeny wybranego dostawcy.
    Logout,
    /// Pokaż stan logowania obu dostawców.
    Status,
    /// Zadaj jedno pytanie. Prompt można też podać przez stdin (pipe).
    Ask {
        /// Treść promptu (jeśli pominięta, czytana jest z stdin).
        prompt: Vec<String>,
        /// Model (domyślny zależy od dostawcy).
        #[arg(short, long)]
        model: Option<String>,
        /// Dołącz plik (obraz/PDF/tekst). Można podać wielokrotnie.
        #[arg(short, long = "file", value_name = "ŚCIEŻKA")]
        files: Vec<PathBuf>,
        /// Instrukcja systemowa.
        #[arg(short, long)]
        system: Option<String>,
        /// Użyj gema (persony) o tej nazwie jako domyślnych ustawień.
        #[arg(short = 'g', long)]
        gem: Option<String>,
        /// Temperatura próbkowania.
        #[arg(short, long)]
        temperature: Option<f32>,
        /// Budżet myślenia w tokenach. Gemini: -1 dynamiczny, 0 off. Claude: >0 włącza (min 1024).
        #[arg(long, value_name = "TOKENY")]
        thinking: Option<i32>,
        /// Pokaż tok rozumowania modelu.
        #[arg(long)]
        show_thoughts: bool,
        /// Maksymalna liczba tokenów odpowiedzi (Claude).
        #[arg(long, default_value_t = 4096)]
        max_tokens: u32,
        /// Wyłącz streaming (czekaj na pełną odpowiedź).
        #[arg(long)]
        no_stream: bool,
    },
    /// Interaktywny czat (REPL) z pamięcią rozmowy.
    Chat {
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        system: Option<String>,
        /// Użyj gema (persony) o tej nazwie.
        #[arg(short = 'g', long)]
        gem: Option<String>,
        #[arg(long, value_name = "TOKENY")]
        thinking: Option<i32>,
        #[arg(long)]
        show_thoughts: bool,
        #[arg(long, default_value_t = 4096)]
        max_tokens: u32,
    },
    /// Zarządzaj gemami — własnymi asystentami/personami (instrukcja + domyślne ustawienia).
    Gem {
        #[command(subcommand)]
        action: GemAction,
    },
}

#[derive(Subcommand)]
enum GemAction {
    /// Dodaj lub nadpisz gema.
    Add {
        /// Nazwa gema (identyfikator).
        name: String,
        /// Instrukcja systemowa / persona.
        #[arg(short, long)]
        system: Option<String>,
        /// Wczytaj instrukcję systemową z pliku.
        #[arg(long, value_name = "ŚCIEŻKA")]
        system_file: Option<PathBuf>,
        /// Krótki opis.
        #[arg(short, long)]
        description: Option<String>,
        /// Domyślny model.
        #[arg(short, long)]
        model: Option<String>,
        /// Domyślny dostawca (gemini/claude).
        #[arg(short, long, value_enum)]
        provider: Option<ProviderKind>,
        /// Domyślna temperatura.
        #[arg(short, long)]
        temperature: Option<f32>,
        /// Domyślny budżet myślenia.
        #[arg(long)]
        thinking: Option<i32>,
    },
    /// Wypisz wszystkie gemy.
    List,
    /// Pokaż szczegóły gema.
    Show { name: String },
    /// Usuń gema.
    Remove { name: String },
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
        Command::Login { no_browser, code } => {
            let provider = cli.provider.unwrap_or_default();
            login(provider, &mut store, no_browser, code).await?
        }
        Command::Logout => {
            let provider = cli.provider.unwrap_or_default();
            match provider {
                ProviderKind::Gemini => store.gemini = Default::default(),
                ProviderKind::Claude => store.claude = Default::default(),
            }
            store.save()?;
            println!("✓ Wylogowano ({}).", provider.name());
        }
        Command::Status => status(&store),
        Command::Gem { action } => gem_command(&mut store, action)?,
        Command::Ask {
            prompt,
            model,
            files,
            system,
            gem,
            temperature,
            thinking,
            show_thoughts,
            max_tokens,
            no_stream,
        } => {
            let g = lookup_gem(&store, gem.as_deref())?;
            let provider = cli
                .provider
                .or_else(|| g.as_ref().and_then(|g| g.provider))
                .unwrap_or_default();
            let model = model
                .or_else(|| g.as_ref().and_then(|g| g.model.clone()))
                .unwrap_or_else(|| provider.default_model().to_string());
            let system = system.or_else(|| g.as_ref().map(|g| g.system.clone()));
            let temperature = temperature.or_else(|| g.as_ref().and_then(|g| g.temperature));
            let thinking = thinking.or_else(|| g.as_ref().and_then(|g| g.thinking));

            let prompt = resolve_prompt(prompt, files.is_empty())?;
            let attachments = read_attachments(&files)?;
            let msg = Message { role: Role::User, text: prompt, files: attachments };
            let client = make_client(provider, &mut store).await?;
            let opts = GenOptions { temperature, thinking, show_thoughts, max_tokens };
            client
                .complete(&model, system.as_deref(), &[msg], &opts, !no_stream)
                .await?;
        }
        Command::Chat { model, system, gem, thinking, show_thoughts, max_tokens } => {
            let g = lookup_gem(&store, gem.as_deref())?;
            let provider = cli
                .provider
                .or_else(|| g.as_ref().and_then(|g| g.provider))
                .unwrap_or_default();
            let model = model
                .or_else(|| g.as_ref().and_then(|g| g.model.clone()))
                .unwrap_or_else(|| provider.default_model().to_string());
            let system = system.or_else(|| g.as_ref().map(|g| g.system.clone()));
            let temperature = g.as_ref().and_then(|g| g.temperature);
            let thinking = thinking.or_else(|| g.as_ref().and_then(|g| g.thinking));

            let client = make_client(provider, &mut store).await?;
            let opts = GenOptions { temperature, thinking, show_thoughts, max_tokens };
            chat(client, provider, &model, system, &opts).await?;
        }
    }
    Ok(())
}

/// Look up a gem by name, erroring if a name was given but not found.
fn lookup_gem(store: &Store, name: Option<&str>) -> Result<Option<Gem>> {
    match name {
        None => Ok(None),
        Some(n) => store
            .gems
            .get(n)
            .cloned()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("nie ma gema '{n}' (zobacz: gemini gem list)")),
    }
}

fn gem_command(store: &mut Store, action: GemAction) -> Result<()> {
    match action {
        GemAction::Add {
            name,
            system,
            system_file,
            description,
            model,
            provider,
            temperature,
            thinking,
        } => {
            let system = match (system, system_file) {
                (Some(s), _) => s,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("czytanie {}", path.display()))?
                    .trim()
                    .to_string(),
                (None, None) => {
                    anyhow::bail!("podaj instrukcję systemową: --system \"...\" albo --system-file <plik>")
                }
            };
            anyhow::ensure!(!system.is_empty(), "instrukcja systemowa jest pusta");
            let existed = store.gems.contains_key(&name);
            store.gems.insert(
                name.clone(),
                Gem { system, description, model, provider, temperature, thinking },
            );
            store.save()?;
            println!("✓ Gem '{name}' {}.", if existed { "zaktualizowany" } else { "dodany" });
        }
        GemAction::List => {
            if store.gems.is_empty() {
                println!("Brak gemów. Dodaj: gemini gem add <nazwa> --system \"...\"");
            } else {
                for (name, g) in &store.gems {
                    let prov = g.provider.map(|p| p.name()).unwrap_or("-");
                    let model = g.model.as_deref().unwrap_or("-");
                    let desc = g.description.as_deref().unwrap_or("");
                    println!("• {name}  [provider: {prov}, model: {model}]  {desc}");
                }
            }
        }
        GemAction::Show { name } => {
            let g = store
                .gems
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("nie ma gema '{name}'"))?;
            println!("Gem: {name}");
            if let Some(d) = &g.description {
                println!("Opis: {d}");
            }
            println!("Dostawca: {}", g.provider.map(|p| p.name()).unwrap_or("(domyślny)"));
            println!("Model: {}", g.model.as_deref().unwrap_or("(domyślny)"));
            if let Some(t) = g.temperature {
                println!("Temperatura: {t}");
            }
            if let Some(t) = g.thinking {
                println!("Myślenie: {t}");
            }
            println!("\nInstrukcja systemowa:\n{}", g.system);
        }
        GemAction::Remove { name } => {
            if store.gems.remove(&name).is_some() {
                store.save()?;
                println!("✓ Usunięto gema '{name}'.");
            } else {
                println!("Nie ma gema '{name}'.");
            }
        }
    }
    Ok(())
}

async fn login(
    provider: ProviderKind,
    store: &mut Store,
    no_browser: bool,
    code: Option<String>,
) -> Result<()> {
    // Step 2: finish with a pasted code.
    if let Some(code) = code {
        match provider {
            ProviderKind::Gemini => auth::finish_login(store, &code).await?,
            ProviderKind::Claude => claude::finish_login(store, &code).await?,
        }
        println!("✓ Zalogowano ({}). Tokeny zapisane lokalnie.", provider.name());
        return Ok(());
    }

    // Headless step 1: print URL, save pending, don't block.
    if no_browser {
        let url = match provider {
            ProviderKind::Gemini => auth::start_headless(store)?,
            ProviderKind::Claude => claude::start_headless(store)?,
        };
        println!("Otwórz ten adres w przeglądarce (np. na telefonie) i zaloguj się:\n\n{url}\n");
        match provider {
            ProviderKind::Claude => println!(
                "Po zatwierdzeniu skopiuj wyświetlony kod autoryzacyjny."
            ),
            ProviderKind::Gemini => println!(
                "Strona przekierowania (localhost) się nie wczyta — skopiuj z paska adresu\n\
                 cały URL `http://localhost:8765/?code=...` albo samą wartość `code`."
            ),
        }
        println!(
            "Następnie dokończ:  gemini -p {} login --code \"<WKLEJONY_KOD>\"",
            match provider { ProviderKind::Gemini => "gemini", ProviderKind::Claude => "claude" }
        );
        return Ok(());
    }

    // Interactive (local machine with a browser).
    match provider {
        ProviderKind::Gemini => auth::login(store).await,
        ProviderKind::Claude => claude::login(store).await,
    }
}

async fn make_client(provider: ProviderKind, store: &mut Store) -> Result<Client> {
    match provider {
        ProviderKind::Gemini => {
            let token = auth::valid_access_token(store).await?;
            Ok(Client::Gemini(CodeAssist::new(token, store).await?))
        }
        ProviderKind::Claude => {
            let token = claude::valid_access_token(store).await?;
            Ok(Client::Claude(ClaudeClient::new(token)?))
        }
    }
}

fn status(store: &Store) {
    for (name, ps) in [("Gemini", &store.gemini), ("Claude", &store.claude)] {
        println!("[{name}]");
        match &ps.credentials {
            Some(c) => {
                let remaining = c.expiry.saturating_sub(config::now_secs());
                println!("  Zalogowany: tak (token wygasa za {remaining}s)");
                if let Some(s) = &c.scope {
                    println!("  Scope: {s}");
                }
                if let Some(p) = &ps.project_id {
                    println!("  Projekt Code Assist: {p}");
                }
            }
            None => println!("  Zalogowany: nie"),
        }
    }
}

/// Use the provided args, or fall back to reading the prompt from stdin so the
/// tool can be used in pipes like an API: `echo "hi" | gemini ask`.
/// `required` is false when files are attached (a prompt is then optional).
fn resolve_prompt(parts: Vec<String>, required: bool) -> Result<String> {
    if !parts.is_empty() {
        return Ok(parts.join(" "));
    }
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

fn read_attachments(files: &[PathBuf]) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    for path in files {
        let bytes = std::fs::read(path)
            .with_context(|| format!("czytanie pliku {}", path.display()))?;
        let mime = guess_mime(path);
        eprintln!("↑ dołączono {} ({}, {} B)", path.display(), mime, bytes.len());
        out.push(Attachment { mime: mime.to_string(), data_b64: B64.encode(&bytes) });
    }
    Ok(out)
}

async fn chat(
    client: Client,
    provider: ProviderKind,
    model: &str,
    system: Option<String>,
    opts: &GenOptions,
) -> Result<()> {
    println!(
        "Czat [{}] z {model}. /exit wyjście, /reset czyści kontekst, /file <ścieżka> dołącza plik.\n",
        provider.name()
    );
    let mut history: Vec<Message> = Vec::new();
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

        let attachments = read_attachments(&pending_files)?;
        pending_files.clear();
        history.push(Message { role: Role::User, text: line.to_string(), files: attachments });

        let answer = client
            .complete(model, system.as_deref(), &history, opts, true)
            .await?;
        history.push(Message { role: Role::Model, text: answer, files: vec![] });
        println!();
    }
    Ok(())
}
