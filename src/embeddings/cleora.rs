//! Cleora-style graph embeddings derived from the wikilink structure.
//!
//! Algorithm (simplified for sparse vault graphs):
//! 1. Assign each note an initial unit random vector seeded by its path hash.
//! 2. Propagate: for each note, average its outlink + inlink neighbors' vectors
//!    plus its own, then L2-normalize.
//! 3. Repeat for N_ITERS iterations.
//! 4. Store results in `note_embeddings` table.

use crate::db::{blob_to_floats, Db};
use crate::embeddings::l2_normalize;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const DIM: usize = 128;
const N_ITERS: usize = 3;

pub fn compute(db: &Arc<Mutex<Db>>) -> Result<()> {
    let db = db.lock().unwrap();
    let note_paths = db.all_note_paths()?;
    if note_paths.is_empty() {
        return Ok(());
    }

    // Build index: path → position
    let idx: HashMap<&str, usize> = note_paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();

    let n = note_paths.len();

    // Load existing note embeddings to warm-start if available, else init from hash
    let existing = db.all_note_embeddings()?;
    let existing_map: HashMap<&str, Vec<f32>> = existing
        .iter()
        .map(|(p, b)| (p.as_str(), blob_to_floats(b)))
        .filter(|(_, v)| v.len() == DIM)
        .collect();

    let mut vecs: Vec<Vec<f32>> = note_paths
        .iter()
        .map(|p| {
            existing_map
                .get(p.as_str())
                .cloned()
                .unwrap_or_else(|| init_vector(p))
        })
        .collect();

    // Build adjacency lists (both directions)
    let links = db.all_links()?;
    let mut out_neighbors: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_neighbors: Vec<Vec<usize>> = vec![vec![]; n];
    for (src, tgt) in &links {
        if let (Some(&si), Some(&ti)) = (idx.get(src.as_str()), idx.get(tgt.as_str())) {
            out_neighbors[si].push(ti);
            in_neighbors[ti].push(si);
        }
    }

    // Propagation
    for _ in 0..N_ITERS {
        let prev = vecs.clone();
        for i in 0..n {
            let neighbors: Vec<usize> = out_neighbors[i]
                .iter()
                .chain(in_neighbors[i].iter())
                .copied()
                .collect();

            if neighbors.is_empty() {
                // Isolated node — keep its own vector
                continue;
            }

            let mut agg = prev[i].clone();
            for &j in &neighbors {
                for (a, b) in agg.iter_mut().zip(prev[j].iter()) {
                    *a += b;
                }
            }
            // Average
            let count = (neighbors.len() + 1) as f32;
            for x in agg.iter_mut() {
                *x /= count;
            }
            l2_normalize(&mut agg);
            vecs[i] = agg;
        }
    }

    // Remove embeddings for notes that have become isolated (no links in or out)
    // — their hash-derived vectors carry no graph signal.
    db.conn.execute(
        "DELETE FROM note_embeddings
         WHERE note_path NOT IN (
             SELECT source_path FROM links
             UNION
             SELECT target_path FROM links
         )",
        [],
    )?;

    // Persist only notes with at least one link
    for (i, (path, vec)) in note_paths.iter().zip(vecs.iter()).enumerate() {
        if out_neighbors[i].is_empty() && in_neighbors[i].is_empty() {
            continue;
        }
        db.upsert_note_embedding(path, vec)?;
    }

    Ok(())
}

/// Deterministic unit-sphere initialization based on path hash.
fn init_vector(path: &str) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut v = vec![0f32; DIM];
    for (i, x) in v.iter_mut().enumerate() {
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        (i as u64).hash(&mut h);
        let bits = h.finish();
        // Map hash to [-1, 1]
        *x = (bits as i64 as f32) / (i64::MAX as f32);
    }
    l2_normalize(&mut v);
    v
}
