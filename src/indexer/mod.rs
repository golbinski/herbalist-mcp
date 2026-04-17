pub mod chunker;
pub mod frontmatter;
pub mod wikilinks;

use crate::db::Db;
use crate::embeddings::Embedder;
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Index (or incrementally re-index) a vault.
/// Skips files whose SHA256 hasn't changed since last index.
pub fn index_vault(vault: &Path, db: &Arc<Mutex<Db>>, embedder: &Arc<Embedder>) -> Result<()> {
    tracing::info!("indexing vault: {}", vault.display());

    let md_files = collect_md_files(vault);
    tracing::info!("found {} markdown files", md_files.len());

    let name_map = wikilinks::build_name_map(&md_files);

    // Track which paths are currently on disk for stale-entry cleanup
    let on_disk: std::collections::HashSet<String> =
        md_files.iter().map(|p| vault_relative(vault, p)).collect();

    // Remove DB entries for files no longer on disk
    {
        let db = db.lock().unwrap();
        let stored = db.all_note_paths()?;
        for path in stored {
            if !on_disk.contains(&path) {
                tracing::debug!("removing deleted note: {}", path);
                db.delete_note(&path)?;
            }
        }
    }

    // Process each file — compute SHA256, skip if unchanged; cache content
    let mut to_reindex: Vec<(PathBuf, String)> = Vec::new(); // (path, cached content)
    for file in &md_files {
        let rel = vault_relative(vault, file);
        let mtime = file_mtime(file).unwrap_or(0);
        let content =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let sha = sha256(&content);

        let db = db.lock().unwrap();
        match db.get_note_meta(&rel)? {
            Some(meta) if meta.sha256 == sha => {
                tracing::debug!("unchanged: {}", rel);
                continue;
            }
            _ => {
                db.upsert_note(&rel, mtime, &sha)?;
                db.delete_chunks(&rel)?;
                db.delete_tags(&rel)?;
                db.delete_frontmatter(&rel)?;
                db.delete_links(&rel)?;
            }
        }
        drop(db);

        to_reindex.push((file.clone(), content));
    }

    tracing::info!("{} files need reindexing", to_reindex.len());

    if to_reindex.is_empty() {
        tracing::info!("vault index up to date");
        return Ok(());
    }

    // Parse and store chunks/tags/frontmatter/links (no embedding yet)
    let mut chunk_ids_by_file: HashMap<String, Vec<(i64, String)>> = HashMap::new(); // path → [(id, text)]

    for (file, content) in &to_reindex {
        let rel = vault_relative(vault, file);

        let (fm, body) = frontmatter::parse(content);

        let db = db.lock().unwrap();

        // Frontmatter
        for (k, v) in &fm.fields {
            db.insert_frontmatter(&rel, k, v)?;
        }
        // Tags
        for tag in &fm.tags {
            db.insert_tag(&rel, tag)?;
        }
        // Inline #tags (simple scan)
        for tag in extract_inline_tags(&body) {
            db.insert_tag(&rel, &tag)?;
        }

        // Chunks
        let chunks = chunker::chunk(&body);
        let mut texts_for_embedding: Vec<(i64, String)> = Vec::new();
        for chunk in &chunks {
            let embed_text = if chunk.heading.is_empty() {
                chunk.content.clone()
            } else {
                format!("{}\n{}", chunk.heading, chunk.content)
            };
            let id = db.insert_chunk(&rel, &chunk.heading, &chunk.content)?;
            texts_for_embedding.push((id, embed_text));
        }
        chunk_ids_by_file.insert(rel.clone(), texts_for_embedding);

        // Wikilinks — resolve targets to vault-relative paths
        let targets = wikilinks::extract_targets(content);
        for target in targets {
            if let Some(resolved) = wikilinks::resolve(&target, &name_map) {
                let rel_target = vault_relative(vault, &resolved);
                db.insert_link(&rel, &rel_target)?;
            }
        }

        drop(db);
    }

    // Embed all new chunks (batched per file)
    tracing::info!("embedding {} file(s)...", chunk_ids_by_file.len());
    for (rel, id_texts) in &chunk_ids_by_file {
        if id_texts.is_empty() {
            continue;
        }
        let texts: Vec<&str> = id_texts.iter().map(|(_, t)| t.as_str()).collect();
        let embeddings = embedder
            .embed(&texts)
            .with_context(|| format!("embedding chunks for {}", rel))?;
        let db = db.lock().unwrap();
        for ((id, _), embedding) in id_texts.iter().zip(embeddings.iter()) {
            db.set_chunk_embedding(*id, embedding)?;
        }
    }

    // Recompute Cleora graph embeddings
    tracing::info!("computing graph embeddings...");
    crate::embeddings::cleora::compute(db)?;

    tracing::info!("indexing complete");
    Ok(())
}

/// Index a single file. Returns `true` if content changed (caller should
/// recompute Cleora after processing a batch). Takes a pre-built `name_map`
/// so the watcher can build it once per batch rather than once per file.
pub fn reindex_file(
    vault: &Path,
    file: &Path,
    db: &Arc<Mutex<Db>>,
    embedder: &Arc<Embedder>,
    name_map: &HashMap<String, PathBuf>,
) -> Result<bool> {
    let rel = vault_relative(vault, file);
    tracing::debug!("reindexing: {}", rel);

    if !file.exists() {
        db.lock().unwrap().delete_note(&rel)?;
        return Ok(true);
    }

    let content = std::fs::read_to_string(file)?;
    let sha = sha256(&content);
    let mtime = file_mtime(file).unwrap_or(0);

    {
        let db = db.lock().unwrap();
        if let Some(meta) = db.get_note_meta(&rel)? {
            if meta.sha256 == sha {
                return Ok(false); // unchanged
            }
        }
        db.upsert_note(&rel, mtime, &sha)?;
        db.delete_chunks(&rel)?;
        db.delete_tags(&rel)?;
        db.delete_frontmatter(&rel)?;
        db.delete_links(&rel)?;
    }

    let (fm, body) = frontmatter::parse(&content);
    let mut texts_for_embedding: Vec<(i64, String)> = Vec::new();

    {
        let db = db.lock().unwrap();
        for (k, v) in &fm.fields {
            db.insert_frontmatter(&rel, k, v)?;
        }
        for tag in &fm.tags {
            db.insert_tag(&rel, tag)?;
        }
        for tag in extract_inline_tags(&body) {
            db.insert_tag(&rel, &tag)?;
        }
        let chunks = chunker::chunk(&body);
        for chunk in &chunks {
            let embed_text = if chunk.heading.is_empty() {
                chunk.content.clone()
            } else {
                format!("{}\n{}", chunk.heading, chunk.content)
            };
            let id = db.insert_chunk(&rel, &chunk.heading, &chunk.content)?;
            texts_for_embedding.push((id, embed_text));
        }
        for target in wikilinks::extract_targets(&content) {
            if let Some(resolved) = wikilinks::resolve(&target, name_map) {
                db.insert_link(&rel, &vault_relative(vault, &resolved))?;
            }
        }
    }

    if !texts_for_embedding.is_empty() {
        let texts: Vec<&str> = texts_for_embedding
            .iter()
            .map(|(_, t)| t.as_str())
            .collect();
        let embeddings = embedder.embed(&texts)?;
        let db = db.lock().unwrap();
        for ((id, _), emb) in texts_for_embedding.iter().zip(embeddings.iter()) {
            db.set_chunk_embedding(*id, emb)?;
        }
    }

    Ok(true)
}

// ── helpers ──────────────────────────────────────────────────────────────────

pub fn collect_md_files(vault: &Path) -> Vec<PathBuf> {
    WalkBuilder::new(vault)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".obsidian" | ".git" | ".herbalist.db")
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        })
        .collect()
}

/// Make `file` relative to `vault`, returned as a forward-slash string.
pub fn vault_relative(vault: &Path, file: &Path) -> String {
    file.strip_prefix(vault)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn sha256(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    hex::encode(h.finalize())
}

fn file_mtime(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn extract_inline_tags(body: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        // Track fenced code blocks (``` or ~~~) so we don't pick up #include etc.
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for word in line.split_whitespace() {
            if word.starts_with('#') && word.len() > 1 {
                let tag = word
                    .trim_start_matches('#')
                    .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
                if !tag.is_empty()
                    && tag
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    tags.push(tag.to_owned());
                }
            }
        }
    }
    tags
}
