mod db;
mod embeddings;
mod indexer;
mod mcp;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use fastembed::EmbeddingModel;
use mcp::tools::ToolContext;
use rmcp::{transport::stdio, ServiceExt};
use std::path::{Path, PathBuf};
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
    ///
    /// The vault must have been indexed at least once with `herbalist-mcp index`
    /// before serving — that is where the embedding model is chosen and configured.
    Serve {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        /// Custom database path (default: <vault>/.herbalist.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Override the configured embedding model for this run.
        #[arg(long)]
        model_path: Option<PathBuf>,
        /// Override the configured embedding model name for this run.
        #[arg(long)]
        model: Option<String>,
    },
    /// Index or re-index the vault without starting the server.
    ///
    /// On first run, prompts for an embedding model choice and saves it to
    /// the vault index. Subsequent runs reuse the saved model automatically.
    Index {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        /// Custom database path (default: <vault>/.herbalist.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Use a local ONNX model directory instead of downloading (bypasses prompt).
        #[arg(long)]
        model_path: Option<PathBuf>,
        /// Embedding model name — skips the interactive prompt and updates the
        /// saved choice. Run with --help to see available names.
        #[arg(long)]
        model: Option<String>,
        /// Index only files under this directory (relative to vault). Repeatable.
        #[arg(long)]
        include: Vec<PathBuf>,
    },
    /// Ad-hoc search for testing without the MCP server.
    Search {
        #[arg(long, env = "HERBALIST_VAULT")]
        vault: PathBuf,
        /// Custom database path (default: <vault>/.herbalist.db).
        #[arg(long)]
        db: Option<PathBuf>,
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
            let (vault, db, model_path, model) = match cli.command {
                Some(Command::Serve {
                    vault,
                    db,
                    model_path,
                    model,
                }) => (vault, db, model_path, model),
                _ => {
                    eprintln!("No subcommand given. Use --help for usage.");
                    std::process::exit(1);
                }
            };
            run_serve(vault, db, model_path, model).await
        }
        Some(Command::Index {
            vault,
            db,
            model_path,
            model,
            include,
        }) => {
            init_logging();
            run_index(vault, db, model_path, model, include)
        }
        Some(Command::Search {
            vault,
            db,
            query,
            top_k,
        }) => {
            init_logging();
            run_search(vault, db, query, top_k)
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

fn resolve_db(vault: &Path, db: Option<PathBuf>) -> PathBuf {
    db.unwrap_or_else(|| vault.join(".herbalist.db"))
}

// ── serve ─────────────────────────────────────────────────────────────────────

async fn run_serve(
    vault: PathBuf,
    db_opt: Option<PathBuf>,
    model_path: Option<PathBuf>,
    model: Option<String>,
) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_env("HERBALIST_LOG")
                .add_directive("herbalist_mcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let vault = vault.canonicalize()?;
    tracing::info!("herbalist-mcp starting, vault={}", vault.display());

    // Resolve model: flag > stored config > error
    let embedder = Arc::new(resolve_embedder_for_serve(
        &resolve_db(&vault, db_opt.clone()),
        model_path.as_deref(),
        model.as_deref(),
    )?);

    let db = Arc::new(Mutex::new(db::Db::open(&resolve_db(&vault, db_opt))?));

    // Auto-index on startup (incremental — skips unchanged files)
    indexer::index_vault(&vault, &db, &embedder, &[])?;

    // Spawn file watcher
    let vault_c = vault.clone();
    let db_c = Arc::clone(&db);
    let embedder_c = Arc::clone(&embedder);
    std::thread::spawn(move || {
        if let Err(e) = watch_vault(vault_c, db_c, embedder_c) {
            tracing::warn!("file watcher stopped: {e}");
        }
    });

    let ctx = ToolContext {
        vault,
        db,
        embedder,
    };
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

/// For `serve`: resolve the embedder without any interactive prompt.
/// Priority: --model-path > --model flag > stored config > error.
fn resolve_embedder_for_serve(
    db_path: &Path,
    model_path: Option<&Path>,
    model_name: Option<&str>,
) -> Result<embeddings::Embedder> {
    if let Some(path) = model_path {
        tracing::info!("loading embedding model from {}", path.display());
        return embeddings::Embedder::from_path(path);
    }

    if let Some(name) = model_name {
        let model = embeddings::model_from_name(name)?;
        tracing::info!("loading embedding model '{}' from registry", name);
        return embeddings::Embedder::from_registry(model);
    }

    // Fall back to stored config
    let db = db::Db::open(db_path)?;
    match db.get_config("model")? {
        Some(stored) => {
            tracing::info!("using configured embedding model: {}", stored);
            let model = embeddings::model_from_name(&stored)?;
            embeddings::Embedder::from_registry(model)
        }
        None => bail!(
            "No embedding model configured. \
             Run `herbalist-mcp index --vault <vault>` first to index the vault \
             and choose an embedding model."
        ),
    }
}

// ── index ─────────────────────────────────────────────────────────────────────

fn run_index(
    vault: PathBuf,
    db_opt: Option<PathBuf>,
    model_path: Option<PathBuf>,
    model: Option<String>,
    includes: Vec<PathBuf>,
) -> Result<()> {
    let vault = vault.canonicalize()?;
    let db_path = resolve_db(&vault, db_opt);

    // Resolve model: --model-path > --model flag > stored config > interactive prompt
    let (embedder, model_key) =
        resolve_embedder_for_index(&db_path, model_path.as_deref(), model.as_deref())?;
    let embedder = Arc::new(embedder);

    let db = Arc::new(Mutex::new(db::Db::open(&db_path)?));

    // Persist the model choice so `serve` and `search` can pick it up
    if let Some(key) = model_key {
        db.lock().unwrap().set_config("model", &key)?;
    }

    indexer::index_vault(&vault, &db, &embedder, &includes)?;
    eprintln!("Indexing complete.");
    Ok(())
}

/// For `index`: resolve the embedder, showing an interactive prompt when needed.
/// Returns the embedder and the model key to persist (None for --model-path).
fn resolve_embedder_for_index(
    db_path: &Path,
    model_path: Option<&Path>,
    model_name: Option<&str>,
) -> Result<(embeddings::Embedder, Option<String>)> {
    // --model-path: use local file, no key to persist
    if let Some(path) = model_path {
        eprintln!("Loading embedding model from {}", path.display());
        return Ok((embeddings::Embedder::from_path(path)?, None));
    }

    // --model flag: explicit override, update stored config
    if let Some(name) = model_name {
        let model = embeddings::model_from_name(name)?;
        eprintln!("Loading embedding model '{name}' from registry...");
        return Ok((
            embeddings::Embedder::from_registry(model)?,
            Some(name.to_owned()),
        ));
    }

    // Check for previously saved choice
    let db = db::Db::open(db_path)?;
    if let Some(stored) = db.get_config("model")? {
        eprintln!("Using configured embedding model: {stored}");
        let model = embeddings::model_from_name(&stored)?;
        return Ok((embeddings::Embedder::from_registry(model)?, Some(stored)));
    }
    drop(db);

    // First-time setup: interactive model selection
    let (name, model) = prompt_model_choice()?;
    eprintln!("\nDownloading '{name}'...");
    Ok((embeddings::Embedder::from_registry(model)?, Some(name)))
}

/// Interactive model-selection menu printed to stderr.
/// Returns the canonical model name string and the EmbeddingModel value.
fn prompt_model_choice() -> Result<(String, EmbeddingModel)> {
    const MODELS: &[(&str, &str, EmbeddingModel)] = &[
        (
            "bge-small-en-v1.5",
            "~130 MB  Fast, good quality (recommended)",
            EmbeddingModel::BGESmallENV15,
        ),
        (
            "all-minilm-l6-v2",
            " ~90 MB  Faster, lower quality",
            EmbeddingModel::AllMiniLML6V2,
        ),
        (
            "bge-base-en-v1.5",
            "~440 MB  Better quality, slower indexing",
            EmbeddingModel::BGEBaseENV15,
        ),
        (
            "nomic-embed-text-v1.5",
            "~550 MB  Best quality",
            EmbeddingModel::NomicEmbedTextV15,
        ),
    ];

    eprintln!();
    eprintln!("No embedding model configured for this vault.");
    eprintln!("Choose a model to download (one-time setup):");
    eprintln!();
    for (i, (name, desc, _)) in MODELS.iter().enumerate() {
        eprintln!("  [{}] {:<26} {}", i + 1, name, desc);
    }
    eprintln!();
    eprintln!("  Or re-run with --model-path <dir> to use a local ONNX model.");
    eprintln!();
    eprint!("Choice [1-{}]: ", MODELS.len());

    // Flush stderr so the prompt appears before we block on stdin
    use std::io::Write;
    std::io::stderr().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice: usize = input.trim().parse().unwrap_or(0);

    if choice < 1 || choice > MODELS.len() {
        bail!(
            "Invalid choice '{}'. Re-run and enter a number between 1 and {}.",
            input.trim(),
            MODELS.len()
        );
    }

    let (name, _, model) = &MODELS[choice - 1];
    Ok((name.to_string(), model.clone()))
}

// ── search ────────────────────────────────────────────────────────────────────

fn run_search(vault: PathBuf, db_opt: Option<PathBuf>, query: String, top_k: usize) -> Result<()> {
    let vault = vault.canonicalize()?;
    let db_path = resolve_db(&vault, db_opt);

    let db_only = db::Db::open(&db_path)?;
    let stored = db_only.get_config("model")?;
    drop(db_only);

    let embedder = match stored {
        Some(name) => {
            let model = embeddings::model_from_name(&name)?;
            embeddings::Embedder::from_registry(model)?
        }
        None => bail!(
            "No embedding model configured for this vault.\n\
             Run `herbalist-mcp index --vault {}` first.",
            vault.display()
        ),
    };

    let db = Arc::new(Mutex::new(db::Db::open(&db_path)?));
    let ctx = ToolContext {
        vault,
        db,
        embedder: Arc::new(embedder),
    };

    let result =
        mcp::tools::search_notes(&ctx, &serde_json::json!({ "query": query, "top_k": top_k }))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

// ── file watcher ──────────────────────────────────────────────────────────────

fn watch_vault(
    vault: PathBuf,
    db: Arc<Mutex<db::Db>>,
    embedder: Arc<embeddings::Embedder>,
) -> Result<()> {
    use notify::{Event, RecursiveMode, Watcher};
    use std::collections::HashSet;
    use std::sync::mpsc;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    const DEBOUNCE: Duration = Duration::from_millis(500);

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&vault, RecursiveMode::Recursive)?;

    tracing::info!("file watcher active");

    let mut dirty: HashSet<PathBuf> = HashSet::new();

    while let Ok(first) = rx.recv() {
        collect_dirty(&first, &mut dirty);

        // Drain any further events that arrive within the debounce window.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(r) => collect_dirty(&r, &mut dirty),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        if dirty.is_empty() {
            continue;
        }

        // Build name map once for the whole batch.
        let md_files = indexer::collect_md_files(&vault, &[]);
        let name_map = indexer::wikilinks::build_name_map(&md_files);

        let mut any_changed = false;
        for path in dirty.drain() {
            match indexer::reindex_file(&vault, &path, &db, &embedder, &name_map) {
                Ok(changed) => any_changed |= changed,
                Err(e) => tracing::warn!("reindex error for {}: {e}", path.display()),
            }
        }

        if any_changed {
            if let Err(e) = embeddings::cleora::compute(&db) {
                tracing::warn!("Cleora recompute failed: {e}");
            }
        }
    }

    Ok(())
}

fn collect_dirty(
    res: &notify::Result<notify::Event>,
    dirty: &mut std::collections::HashSet<PathBuf>,
) {
    use notify::EventKind;
    let event = match res {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("watcher error: {e}");
            return;
        }
    };
    if matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        for path in &event.paths {
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                dirty.insert(path.clone());
            }
        }
    }
}
