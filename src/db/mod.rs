use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(vault_root: &Path) -> Result<Self> {
        let db_path = vault_root.join(".herbalist.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening db at {}", db_path.display()))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;

             CREATE TABLE IF NOT EXISTS notes (
                 path   TEXT PRIMARY KEY,
                 mtime  INTEGER NOT NULL,
                 sha256 TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS frontmatter (
                 note_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
                 key       TEXT NOT NULL,
                 value     TEXT NOT NULL,
                 PRIMARY KEY (note_path, key)
             );

             CREATE TABLE IF NOT EXISTS tags (
                 note_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
                 tag       TEXT NOT NULL,
                 PRIMARY KEY (note_path, tag)
             );
             CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

             CREATE TABLE IF NOT EXISTS chunks (
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 note_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
                 heading   TEXT NOT NULL,
                 content   TEXT NOT NULL,
                 embedding BLOB
             );
             CREATE INDEX IF NOT EXISTS idx_chunks_note ON chunks(note_path);

             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                 content,
                 heading,
                 note_path UNINDEXED,
                 chunk_id  UNINDEXED
             );

             CREATE TABLE IF NOT EXISTS links (
                 source_path TEXT NOT NULL,
                 target_path TEXT NOT NULL,
                 PRIMARY KEY (source_path, target_path)
             );
             CREATE INDEX IF NOT EXISTS idx_links_source ON links(source_path);
             CREATE INDEX IF NOT EXISTS idx_links_target ON links(target_path);

             CREATE TABLE IF NOT EXISTS note_embeddings (
                 note_path TEXT PRIMARY KEY REFERENCES notes(path) ON DELETE CASCADE,
                 embedding BLOB NOT NULL
             );

             CREATE TABLE IF NOT EXISTS config (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        Ok(())
    }

    // ── notes ────────────────────────────────────────────────────────────────

    pub fn get_note_meta(&self, path: &str) -> Result<Option<NoteMeta>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, mtime, sha256 FROM notes WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(NoteMeta {
                path: row.get(0)?,
                mtime: row.get(1)?,
                sha256: row.get(2)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn upsert_note(&self, path: &str, mtime: i64, sha256: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO notes(path, mtime, sha256) VALUES(?1,?2,?3)
             ON CONFLICT(path) DO UPDATE SET mtime=excluded.mtime, sha256=excluded.sha256",
            params![path, mtime, sha256],
        )?;
        Ok(())
    }

    pub fn delete_note(&self, path: &str) -> Result<()> {
        // Cascades to chunks, tags, frontmatter, note_embeddings, links (source side)
        self.conn
            .execute("DELETE FROM notes WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM links WHERE source_path=?1", params![path])?;
        Ok(())
    }

    pub fn all_note_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM notes")?;
        let paths = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(paths)
    }

    // ── frontmatter ──────────────────────────────────────────────────────────

    pub fn delete_frontmatter(&self, note_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM frontmatter WHERE note_path=?1",
            params![note_path],
        )?;
        Ok(())
    }

    pub fn insert_frontmatter(&self, note_path: &str, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO frontmatter(note_path,key,value) VALUES(?1,?2,?3)",
            params![note_path, key, value],
        )?;
        Ok(())
    }

    pub fn get_frontmatter(&self, note_path: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key,value FROM frontmatter WHERE note_path=?1")?;
        let rows = stmt
            .query_map(params![note_path], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ── tags ─────────────────────────────────────────────────────────────────

    pub fn delete_tags(&self, note_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tags WHERE note_path=?1", params![note_path])?;
        Ok(())
    }

    pub fn insert_tag(&self, note_path: &str, tag: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO tags(note_path,tag) VALUES(?1,?2)",
            params![note_path, tag],
        )?;
        Ok(())
    }

    pub fn all_tags(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT tag FROM tags ORDER BY tag")?;
        let tags = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(tags)
    }

    pub fn notes_by_tag(&self, tag: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT note_path FROM tags WHERE tag=?1 ORDER BY note_path")?;
        let paths = stmt
            .query_map(params![tag], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(paths)
    }

    /// All tags for a specific note.
    pub fn notes_by_tag_for_note(&self, note_path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE note_path=?1 ORDER BY tag")?;
        let tags = stmt
            .query_map(params![note_path], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(tags)
    }

    // ── chunks ───────────────────────────────────────────────────────────────

    pub fn delete_chunks(&self, note_path: &str) -> Result<()> {
        // Remove from FTS first
        self.conn.execute(
            "DELETE FROM chunks_fts WHERE note_path=?1",
            params![note_path],
        )?;
        self.conn
            .execute("DELETE FROM chunks WHERE note_path=?1", params![note_path])?;
        Ok(())
    }

    pub fn insert_chunk(&self, note_path: &str, heading: &str, content: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO chunks(note_path,heading,content) VALUES(?1,?2,?3)",
            params![note_path, heading, content],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "INSERT INTO chunks_fts(rowid, content, heading, note_path, chunk_id) VALUES(?1,?2,?3,?4,?5)",
            params![id, content, heading, note_path, id],
        )?;
        Ok(id)
    }

    pub fn set_chunk_embedding(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        let blob = floats_to_blob(embedding);
        self.conn.execute(
            "UPDATE chunks SET embedding=?1 WHERE id=?2",
            params![blob, chunk_id],
        )?;
        Ok(())
    }

    /// Returns all chunks that have embeddings.
    pub fn all_embedded_chunks(&self) -> Result<Vec<EmbeddedChunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, note_path, heading, content, embedding FROM chunks WHERE embedding IS NOT NULL",
        )?;
        let chunks = stmt
            .query_map([], |r| {
                let blob: Vec<u8> = r.get(4)?;
                Ok(EmbeddedChunk {
                    id: r.get(0)?,
                    note_path: r.get(1)?,
                    heading: r.get(2)?,
                    content: r.get(3)?,
                    embedding: blob,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(chunks)
    }

    // ── FTS search ───────────────────────────────────────────────────────────

    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT chunk_id, note_path, heading, snippet(chunks_fts, 0, '[', ']', '...', 32), rank
             FROM chunks_fts
             WHERE chunks_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query, limit as i64], |r| {
                Ok(FtsResult {
                    chunk_id: r.get(0)?,
                    note_path: r.get(1)?,
                    heading: r.get(2)?,
                    snippet: r.get(3)?,
                    rank: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ── links ────────────────────────────────────────────────────────────────

    pub fn delete_links(&self, source_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM links WHERE source_path=?1",
            params![source_path],
        )?;
        Ok(())
    }

    pub fn insert_link(&self, source_path: &str, target_path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO links(source_path,target_path) VALUES(?1,?2)",
            params![source_path, target_path],
        )?;
        Ok(())
    }

    pub fn outlinks(&self, path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT target_path FROM links WHERE source_path=?1 ORDER BY target_path")?;
        let paths = stmt
            .query_map(params![path], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(paths)
    }

    pub fn backlinks(&self, path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_path FROM links WHERE target_path=?1 ORDER BY source_path")?;
        let paths = stmt
            .query_map(params![path], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(paths)
    }

    pub fn all_links(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_path, target_path FROM links")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ── note_embeddings (Cleora) ─────────────────────────────────────────────

    pub fn upsert_note_embedding(&self, note_path: &str, embedding: &[f32]) -> Result<()> {
        let blob = floats_to_blob(embedding);
        self.conn.execute(
            "INSERT INTO note_embeddings(note_path,embedding) VALUES(?1,?2)
             ON CONFLICT(note_path) DO UPDATE SET embedding=excluded.embedding",
            params![note_path, blob],
        )?;
        Ok(())
    }

    pub fn all_note_embeddings(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT note_path, embedding FROM note_embeddings")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_note_embedding(&self, note_path: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM note_embeddings WHERE note_path=?1")?;
        let mut rows = stmt.query(params![note_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    // ── config ───────────────────────────────────────────────────────────────

    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM config WHERE key=?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO config(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

// ── row types ────────────────────────────────────────────────────────────────

pub struct NoteMeta {
    #[allow(dead_code)]
    pub path: String,
    #[allow(dead_code)]
    pub mtime: i64,
    pub sha256: String,
}

pub struct EmbeddedChunk {
    pub id: i64,
    pub note_path: String,
    pub heading: String,
    pub content: String,
    pub embedding: Vec<u8>, // raw f32 LE bytes
}

pub struct FtsResult {
    pub chunk_id: i64,
    pub note_path: String,
    pub heading: String,
    pub snippet: String,
    #[allow(dead_code)] // populated by FTS5; available for future ranking use
    pub rank: f64,
}

// ── blob helpers ─────────────────────────────────────────────────────────────

pub fn floats_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_floats(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
