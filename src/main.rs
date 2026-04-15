mod db;
mod embeddings;
mod indexer;
mod mcp;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mcp::tools::ToolContext;
use rmcp::{ServiceExt, transport::stdio};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "herbalist-mcp",
    about = "Semantic search and graph navigation over an Obsidian vault.\n\
             Run without arguments to start the MCP server on stdio."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the MCP server on stdio (auto-indexes vault on startup).
    Serve {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        /// Path to a local ONNX model directory (bypasses download).
        #[arg(long)]
        model_path: Option<PathBuf>,
        /// Embedding model name (default: bge-small-en-v1.5).
        #[arg(long)]
        model: Option<String>,
    },
    /// Index or re-index the vault without starting the server.
    Index {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        #[arg(long)]
        model_path: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Ad-hoc search for testing without the MCP server.
    Search {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        #[arg(long)]
        query: String,
        #[arg(long, default_value = "10")]
        top_k: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Serve { .. }) => {
            let (vault, model_path, model) = match cli.command {
                Some(Command::Serve { vault, model_path, model }) => (vault, model_path, model),
                _ => {
                    eprintln!("No subcommand given. Use --help for usage.");
                    std::process::exit(1);
                }
            };
            run_serve(vault, model_path, model).await
        }
        Some(Command::Index { vault, model_path, model }) => {
            init_logging();
            run_index(vault, model_path, model)
        }
        Some(Command::Search { vault, query, top_k }) => {
            init_logging();
            run_search(vault, query, top_k)
        }
    }
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_env("HERBALIST_LOG")
                .add_directive("herbalist_mcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();
}

async fn run_serve(
    vault: PathBuf,
    model_path: Option<PathBuf>,
    model: Option<String>,
) -> Result<()> {
    // MCP server logs only to stderr so stdout is clean for JSON-RPC
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_env("HERBALIST_LOG")
                .add_directive("herbalist_mcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let vault = vault.canonicalize()?;
    tracing::info!("herbalist-mcp starting, vault={}", vault.display());

    let embedder = Arc::new(build_embedder(model_path.as_deref(), model.as_deref())?);
    let db = Arc::new(Mutex::new(db::Db::open(&vault)?));

    // Auto-index on startup (incremental — skips unchanged files)
    indexer::index_vault(&vault, &db, &embedder)?;

    // Spawn file watcher
    let vault_c = vault.clone();
    let db_c = Arc::clone(&db);
    let embedder_c = Arc::clone(&embedder);
    std::thread::spawn(move || {
        if let Err(e) = watch_vault(vault_c, db_c, embedder_c) {
            tracing::warn!("file watcher stopped: {e}");
        }
    });

    let ctx = ToolContext { vault, db, embedder };
    let server = mcp::HerbalistServer::new(ctx);
    tracing::info!("MCP server ready");

    server
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("MCP serve error: {e}"))?
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP wait error: {e}"))?;

    Ok(())
}

fn run_index(vault: PathBuf, model_path: Option<PathBuf>, model: Option<String>) -> Result<()> {
    let vault = vault.canonicalize()?;
    let embedder = Arc::new(build_embedder(model_path.as_deref(), model.as_deref())?);
    let db = Arc::new(Mutex::new(db::Db::open(&vault)?));
    indexer::index_vault(&vault, &db, &embedder)?;
    eprintln!("Indexing complete.");
    Ok(())
}

fn run_search(vault: PathBuf, query: String, top_k: usize) -> Result<()> {
    let vault = vault.canonicalize()?;
    // Search doesn't need an embedder loaded yet — we need it for query embedding
    let db_only = db::Db::open(&vault)?;
    let stored_model = db_only.get_config("model")?.unwrap_or_else(|| "bge-small-en-v1.5".to_owned());
    drop(db_only);

    let embedder = Arc::new(build_embedder(None, Some(&stored_model))?);
    let db = Arc::new(Mutex::new(db::Db::open(&vault)?));
    let ctx = ToolContext { vault, db, embedder };

    let result = mcp::tools::search_notes(&ctx, &serde_json::json!({
        "query": query,
        "top_k": top_k,
    }))?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn build_embedder(
    model_path: Option<&std::path::Path>,
    model_name: Option<&str>,
) -> Result<embeddings::Embedder> {
    if let Some(path) = model_path {
        tracing::info!("loading embedding model from {}", path.display());
        embeddings::Embedder::from_path(path)
    } else {
        let model = model_name
            .map(embeddings::model_from_name)
            .transpose()?
            .unwrap_or_else(embeddings::default_model);
        tracing::info!("loading embedding model from registry");
        embeddings::Embedder::from_registry(model)
    }
}

// ── file watcher ──────────────────────────────────────────────────────────────

fn watch_vault(
    vault: PathBuf,
    db: Arc<Mutex<db::Db>>,
    embedder: Arc<embeddings::Embedder>,
) -> Result<()> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&vault, RecursiveMode::Recursive)?;

    tracing::info!("file watcher active");

    for res in rx {
        let event = match res {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("watcher error: {e}");
                continue;
            }
        };

        let is_relevant = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );
        if !is_relevant {
            continue;
        }

        // Small debounce
        std::thread::sleep(Duration::from_millis(500));

        for path in event.paths {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Err(e) = indexer::reindex_file(&vault, &path, &db, &embedder) {
                tracing::warn!("reindex error for {}: {e}", path.display());
            }
        }
    }

    Ok(())
}
