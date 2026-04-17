use crate::db::{blob_to_floats, Db};
use crate::embeddings::{cosine_similarity, Embedder};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct ToolContext {
    pub vault: PathBuf,
    pub db: Arc<Mutex<Db>>,
    pub embedder: Arc<Embedder>,
}

// ── search_notes ──────────────────────────────────────────────────────────────

pub fn search_notes(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let query = params["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'query' parameter"))?;
    let top_k = params["top_k"].as_u64().unwrap_or(10) as usize;

    // Semantic search: embed query, cosine over all chunk embeddings
    let query_vec = ctx
        .embedder
        .embed(&[query])?
        .into_iter()
        .next()
        .unwrap_or_default();

    // Acquire lock once for both reads (consistent snapshot, one lock/unlock)
    let (chunks, fts_results) = {
        let db = ctx.db.lock().unwrap();
        (db.all_embedded_chunks()?, db.fts_search(query, top_k * 2)?)
    };

    let mut scored: Vec<(f32, &crate::db::EmbeddedChunk)> = chunks
        .iter()
        .map(|c| {
            let emb = blob_to_floats(&c.embedding);
            let score = cosine_similarity(&query_vec, &emb);
            (score, c)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let fts_set: HashMap<i64, &crate::db::FtsResult> =
        fts_results.iter().map(|r| (r.chunk_id, r)).collect();

    // Merge: semantic top-k, boosted if also in FTS results
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut results = Vec::new();

    for (score, chunk) in scored.iter().take(top_k * 2) {
        let mut final_score = *score;
        if fts_set.contains_key(&chunk.id) {
            final_score = (final_score + 0.2).min(1.0); // FTS boost
        }
        if seen_paths.insert(chunk.note_path.clone()) {
            let snippet = chunk.content.chars().take(200).collect::<String>();
            results.push(json!({
                "path": chunk.note_path,
                "section": chunk.heading,
                "snippet": snippet,
                "score": (final_score * 1000.0) as i64,
            }));
        }
        if results.len() >= top_k {
            break;
        }
    }

    // Fill remaining slots from FTS results not yet in results
    for fts in fts_results.iter() {
        if results.len() >= top_k {
            break;
        }
        if seen_paths.insert(fts.note_path.clone()) {
            results.push(json!({
                "path": fts.note_path,
                "section": fts.heading,
                "snippet": fts.snippet,
                "score": 500, // FTS-only baseline
            }));
        }
    }

    Ok(json!({ "results": results }))
}

// ── get_note ─────────────────────────────────────────────────────────────────

pub fn get_note(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let path = params["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'path' parameter"))?;

    // Reject absolute paths and any path containing '..' components.
    {
        let p = std::path::Path::new(path);
        if p.is_absolute() || p.components().any(|c| c == std::path::Component::ParentDir) {
            anyhow::bail!("invalid note path: {}", path);
        }
    }

    let full_path = ctx.vault.join(path);
    let content = std::fs::read_to_string(&full_path)
        .unwrap_or_else(|_| "(file not found on disk)".to_owned());

    let db = ctx.db.lock().unwrap();
    let frontmatter = db.get_frontmatter(path)?;
    let note_tags: Vec<String> = db.tags_for_note(path)?;
    let outlinks = db.outlinks(path)?;
    let backlinks = db.backlinks(path)?;

    let fm_obj: serde_json::Map<String, Value> = frontmatter
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();

    Ok(json!({
        "path": path,
        "content": content,
        "frontmatter": fm_obj,
        "tags": note_tags,
        "outlinks": outlinks,
        "backlinks": backlinks,
    }))
}

// ── related_notes ─────────────────────────────────────────────────────────────

pub fn related_notes(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let path = params["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'path' parameter"))?;
    let top_k = params["top_k"].as_u64().unwrap_or(10) as usize;

    let db = ctx.db.lock().unwrap();
    let anchor_blob = match db.get_note_embedding(path)? {
        Some(b) => b,
        None => return Ok(json!({ "results": [] })),
    };
    let anchor = blob_to_floats(&anchor_blob);

    let all = db.all_note_embeddings()?;
    let mut scored: Vec<(f32, String)> = all
        .iter()
        .filter(|(p, _)| p != path)
        .map(|(p, b)| {
            let v = blob_to_floats(b);
            (cosine_similarity(&anchor, &v), p.clone())
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let results: Vec<Value> = scored
        .into_iter()
        .take(top_k)
        .map(|(score, path)| json!({ "path": path, "score": (score * 1000.0) as i64 }))
        .collect();

    Ok(json!({ "results": results }))
}

// ── list_tags ─────────────────────────────────────────────────────────────────

pub fn list_tags(ctx: &ToolContext, _params: &Value) -> Result<Value> {
    let tags = ctx.db.lock().unwrap().all_tags()?;
    Ok(json!({ "tags": tags }))
}

// ── notes_by_tag ──────────────────────────────────────────────────────────────

pub fn notes_by_tag(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let tag = params["tag"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'tag' parameter"))?;
    let paths = ctx.db.lock().unwrap().notes_by_tag(tag)?;
    Ok(json!({ "paths": paths }))
}

// ── graph_neighbors ───────────────────────────────────────────────────────────

pub fn graph_neighbors(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let path = params["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'path' parameter"))?;
    let depth = params["depth"].as_u64().unwrap_or(1) as usize;
    let depth = depth.min(5); // safety cap

    let db = ctx.db.lock().unwrap();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut results: Vec<Value> = Vec::new();

    visited.insert(path.to_owned());
    queue.push_back((path.to_owned(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        let outlinks = db.outlinks(&current)?;
        for target in outlinks {
            if visited.insert(target.clone()) {
                results.push(json!({ "path": target, "link_type": "outlink", "depth": d + 1 }));
                queue.push_back((target, d + 1));
            }
        }
        let backlinks = db.backlinks(&current)?;
        for source in backlinks {
            if visited.insert(source.clone()) {
                results.push(json!({ "path": source, "link_type": "backlink", "depth": d + 1 }));
                queue.push_back((source, d + 1));
            }
        }
    }

    Ok(json!({ "neighbors": results }))
}
