# herbalist-mcp

A semantic search and graph navigation MCP server for [Obsidian](https://obsidian.md/) vaults, written in Rust. Pre-indexes a folder of Markdown files with neural embeddings and a wikilink graph, then exposes 6 MCP tools to AI agents over stdio.

## Features

- **Hybrid search**: FTS5 BM25 keyword matching + neural cosine similarity, merged with a boost when a chunk appears in both result sets
- **In-process embeddings**: [fastembed](https://github.com/Qdrant/fastembed-rs) (BGE-Small-EN-v1.5 by default) — no Ollama, no separate server, no HTTP calls at query time
- **Graph navigation**: `[[wikilink]]` graph stored in SQLite; BFS traversal, backlink resolution, structural graph embeddings (Cleora) for `related_notes`
- **Obsidian-aware parsing**: YAML frontmatter, inline `#tags`, `[[wikilink]]` extraction and path resolution, H1/H2 heading-based content chunking
- **Incremental indexing**: SHA256 content hashing — only changed files are re-embedded on restart
- **File watching**: `notify`-based watcher re-indexes modified files in the background while the server is running
- **Self-contained index**: SQLite database at `<vault>/.herbalist.db` — no external state, easy to reset

## Installation

```bash
git clone https://github.com/golbinski/herbalist-mcp
cd herbalist-mcp
cargo build --release
```

The binary is at `target/release/herbalist-mcp`. Copy it anywhere on your `PATH`.

Or download a pre-built binary from [Releases](https://github.com/golbinski/herbalist-mcp/releases). Verify the SHA256 checksum and GitHub build provenance attestation before running.

### First run: embedding model download

On the first `index` or `serve`, herbalist-mcp downloads the embedding model (~130 MB) from [HuggingFace](https://huggingface.co/) via fastembed. The model is cached to `~/.cache/fastembed/` and reused on all subsequent runs. An internet connection is only required once.

```
[INFO] Loading embedding model from registry (first run — downloading ~130 MB)...
[INFO] Model cached to ~/.cache/fastembed/
```

To use a locally-provided model instead (air-gapped environments):

```bash
herbalist-mcp index --vault ~/notes --model-path ~/models/bge-small/
```

The `--model-path` directory must contain: `model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json`.

## CLI Usage

```bash
# Index a vault (blocks until complete, then exits)
herbalist-mcp index --vault ~/notes

# Index with a specific embedding model
herbalist-mcp index --vault ~/notes --model bge-base-en-v1.5

# Index using a local model (no download)
herbalist-mcp index --vault ~/notes --model-path ~/models/bge-small/

# Ad-hoc search for testing
herbalist-mcp search --vault ~/notes --query "stoic philosophy"
herbalist-mcp search --vault ~/notes --query "kubernetes networking" --top-k 20

# Start MCP server (auto-indexes on startup, watches for changes)
herbalist-mcp serve --vault ~/notes
```

All output goes to stdout as JSON. Logs go to stderr.

Run without arguments (or with `serve`) to start the MCP server over stdio.

### Available models

| Name | Size | Dimensions | Notes |
|------|------|------------|-------|
| `bge-small-en-v1.5` | ~130 MB | 384 | Default — good balance of speed and quality |
| `bge-base-en-v1.5` | ~440 MB | 768 | Higher quality, slower indexing |
| `all-minilm-l6-v2` | ~90 MB | 384 | Faster, slightly lower quality |
| `nomic-embed-text-v1.5` | ~550 MB | 768 | Highest quality |

The model choice is persisted to the vault index — subsequent runs use the same model automatically.

## MCP Configuration

Add to your MCP client config (e.g. Claude Code `~/.claude.json` or Claude Desktop `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "herbalist": {
      "command": "/path/to/herbalist-mcp",
      "args": ["serve", "--vault", "/path/to/your/vault"],
      "env": {
        "HERBALIST_LOG": "herbalist_mcp=info"
      }
    }
  }
}
```

Or using the environment variable shorthand:

```json
{
  "mcpServers": {
    "herbalist": {
      "command": "/path/to/herbalist-mcp",
      "args": ["serve"],
      "env": {
        "HERBALIST_VAULT": "/path/to/your/vault",
        "HERBALIST_LOG": "herbalist_mcp=info"
      }
    }
  }
}
```

### Environment variables

| Variable | Description |
|----------|-------------|
| `HERBALIST_VAULT` | Vault path — equivalent to `--vault` flag |
| `HERBALIST_LOG` | Log level filter (output goes to stderr). Default: `herbalist_mcp=info` |

## MCP Tools

### `search_notes`

Semantic + keyword search over the vault. Embeds the query with the same model used during indexing, ranks note sections by cosine similarity, and blends in FTS5 BM25 results with a small boost for chunks that appear in both.

```
search_notes(query: "stoic philosophy and resilience", top_k: 10)
```

Returns: `[{ path, section, snippet, score }]`

### `get_note`

Read a note's full content with its graph context — frontmatter, tags, outbound wikilinks, and backlinks from other notes.

```
get_note(path: "Philosophy/Marcus Aurelius.md")
```

Returns: `{ path, content, frontmatter, tags, outlinks, backlinks }`

### `related_notes`

Find notes structurally similar to a given note based on the wikilink graph. Uses Cleora embeddings — notes that share similar link neighborhoods rank higher.

```
related_notes(path: "Philosophy/Marcus Aurelius.md", top_k: 10)
```

Returns: `[{ path, score }]`

### `list_tags`

List all tags present in the vault (from YAML frontmatter `tags:` fields and inline `#tag` syntax).

```
list_tags()
```

Returns: `{ tags: ["philosophy", "rust", "mcp", ...] }`

### `notes_by_tag`

Return all notes that have a given tag.

```
notes_by_tag(tag: "philosophy")
```

Returns: `{ paths: ["Philosophy/Marcus Aurelius.md", ...] }`

### `graph_neighbors`

Traverse the `[[wikilink]]` graph from a note up to `depth` hops in both directions (outlinks and backlinks).

```
graph_neighbors(path: "Philosophy/Marcus Aurelius.md", depth: 2)
```

Returns: `{ neighbors: [{ path, link_type, depth }] }` — `link_type` is `"outlink"` or `"backlink"`.

## Architecture

```
src/
  main.rs                — CLI entry point (clap), serve/index/search dispatch, file watcher
  db/
    mod.rs               — SQLite schema, FTS5 virtual table, all CRUD
  indexer/
    mod.rs               — file walk (ignore-aware), incremental pipeline, single-file reindex
    chunker.rs           — split markdown body at H1/H2 headings
    frontmatter.rs       — YAML frontmatter parser (serde_yaml)
    wikilinks.rs         — [[wikilink]] extraction, vault name→path resolver
  embeddings/
    mod.rs               — fastembed wrapper, --model-path support, cosine similarity
    cleora.rs            — graph embeddings: degree init → adjacency propagation → L2 normalize
  mcp/
    mod.rs               — ServerHandler impl (rmcp), tool registration, call dispatch
    tools.rs             — 6 tool handler functions
```

### Indexing pipeline

1. Walk vault with `ignore::WalkBuilder` — respects `.gitignore`, skips `.obsidian/`
2. Compare each file's SHA256 against the stored value — skip if unchanged
3. For changed files: delete existing chunks/tags/links (cascade), re-parse
4. Parse: extract YAML frontmatter → tags; split body into heading chunks; extract wikilinks
5. Resolve wikilinks to vault-relative paths using a vault-wide name→path map
6. Embed all new chunks in batch via fastembed (BGE-Small or configured model)
7. Recompute Cleora graph embeddings from the updated wikilink adjacency

All operations run on a single SQLite connection with WAL mode. The file watcher debounces changes by 500ms before triggering a single-file reindex.

### Embeddings

**Neural (chunk-level)**: fastembed runs ONNX-format models in-process via ONNX Runtime. The default model (BGE-Small-EN-v1.5) produces 384-dimensional vectors. Embeddings are stored as raw `f32` little-endian blobs in the `chunks.embedding` column.

**Structural (note-level)**: Cleora algorithm over the wikilink graph. Each note is initialized with a deterministic unit vector seeded by its path hash. Three iterations of adjacency-weighted mean pooling with L2 normalization produce the final `note_embeddings` vectors (128 dimensions). Notes with similar link neighborhoods end up close in embedding space.

### SQLite schema

| Table | Description |
|-------|-------------|
| `notes` | Indexed files: `path`, `mtime`, `sha256` |
| `chunks` | Heading sections: `note_path`, `heading`, `content`, `embedding BLOB` |
| `chunks_fts` | FTS5 virtual table over `chunks.content` and `chunks.heading` |
| `links` | Wikilink graph: `source_path → target_path` |
| `tags` | Per-note tags from frontmatter and inline `#tag` |
| `frontmatter` | YAML frontmatter key/value pairs |
| `note_embeddings` | Cleora per-note structural embeddings |
| `config` | Persisted settings (model name, etc.) |

## Development

```bash
cargo build           # debug build
cargo test            # 6 unit tests (chunker, frontmatter, wikilinks)
cargo build --release
```

To reset the index:

```bash
rm ~/notes/.herbalist.db
herbalist-mcp index --vault ~/notes
```
