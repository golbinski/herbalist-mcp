# herbalist-mcp

A semantic search and graph navigation MCP server for [Obsidian](https://obsidian.md/) vaults, written in Rust. Pre-indexes a folder of Markdown files with neural embeddings and a wikilink graph, then exposes 6 MCP tools to AI agents over stdio.

## Features

- **Hybrid search**: FTS5 BM25 keyword matching + neural cosine similarity, merged with a boost when a chunk appears in both result sets
- **In-process embeddings**: [fastembed](https://github.com/Qdrant/fastembed-rs) (ONNX Runtime) — no Ollama, no separate server, no HTTP calls at query time
- **Graph navigation**: `[[wikilink]]` graph stored in SQLite; BFS traversal, backlink resolution, structural graph embeddings (Cleora) for `related_notes`
- **Obsidian-aware parsing**: YAML frontmatter, inline `#tags`, `[[wikilink]]` extraction and path resolution, H1/H2 heading-based content chunking
- **Incremental indexing**: SHA256 content hashing — only changed files are re-embedded on restart
- **File watching**: `notify`-based watcher re-indexes modified files in the background while the server is running
- **Self-contained index**: SQLite database at `<vault>/.herbalist.db` — no external state, easy to reset

## Installation

### macOS / Linux

```sh
curl -fsSL https://raw.githubusercontent.com/golbinski/herbalist-mcp/main/install.sh | sh
```

The script detects your OS and architecture, downloads the right binary from [Releases](https://github.com/golbinski/herbalist-mcp/releases), verifies its SHA256 checksum, installs it to `~/.local/bin`, and — if you provide a vault path — indexes it and writes MCP entries into Claude Code (`~/.claude.json`) and VS Code (`Code/User/mcp.json`) if those are present.

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/golbinski/herbalist-mcp/main/install.ps1 | iex
```

Same steps as above: downloads `herbalist-mcp-windows-x86_64.exe`, verifies SHA256, installs to `%LOCALAPPDATA%\herbalist-mcp`, adds it to your user PATH, and configures Claude Code and VS Code if found.

### Manual install

Download a pre-built binary from [Releases](https://github.com/golbinski/herbalist-mcp/releases):

| Platform | File |
|----------|------|
| macOS (Apple Silicon) | `herbalist-mcp-macos-aarch64` |
| macOS (Intel) | `herbalist-mcp-macos-x86_64` |
| Linux x86_64 | `herbalist-mcp-linux-x86_64` |
| Windows x86_64 | `herbalist-mcp-windows-x86_64.exe` |

Each release includes a `.sha256` checksum file and a GitHub build provenance attestation. Verify both before running.

### Build from source

```bash
git clone https://github.com/golbinski/herbalist-mcp
cd herbalist-mcp
cargo build --release
# binary at target/release/herbalist-mcp
```

## Quick start with the sample vault

The repo includes a sample Obsidian vault (`sample-vault/`) covering medicinal herbs — useful for testing without needing your own notes:

```bash
herbalist-mcp index --vault ./sample-vault
herbalist-mcp search --vault ./sample-vault --query "anxiety and sleep"
herbalist-mcp search --vault ./sample-vault --query "joint pain inflammation"
```

## CLI Usage

### `index` — set up and index a vault

Run this before `serve`. On the first run it prompts you to choose an embedding model:

```
$ herbalist-mcp index --vault ~/notes

No embedding model configured for this vault.
Choose a model to download (one-time setup):

  [1] bge-small-en-v1.5        ~130 MB  Fast, good quality (recommended)
  [2] all-minilm-l6-v2          ~90 MB  Faster, lower quality
  [3] bge-base-en-v1.5         ~440 MB  Better quality, slower indexing
  [4] nomic-embed-text-v1.5    ~550 MB  Best quality

  Or re-run with --model-path <dir> to use a local ONNX model.

Choice [1-4]:
```

The choice is saved to the vault's `.herbalist.db` and reused automatically on all subsequent runs. To change it later, pass `--model <name>` explicitly.

```bash
# First-time index (prompts for model)
herbalist-mcp index --vault ~/notes

# Re-index with a different model (updates saved choice)
herbalist-mcp index --vault ~/notes --model bge-base-en-v1.5

# Use a local ONNX model (no download, no prompt)
herbalist-mcp index --vault ~/notes --model-path ~/models/bge-small/
```

### `serve` — start the MCP server

Reads the model configured by `index`. Fails with a clear error if the vault has not been indexed yet.

```bash
herbalist-mcp serve --vault ~/notes
```

The server auto-indexes on startup (incremental — skips unchanged files) and watches for file changes in the background.

### `search` — ad-hoc search for testing

```bash
herbalist-mcp search --vault ~/notes --query "stoic philosophy"
herbalist-mcp search --vault ~/notes --query "kubernetes networking" --top-k 20
```

Outputs JSON to stdout.

### Available models

| Name | Size | Dimensions | Notes |
|------|------|------------|-------|
| `bge-small-en-v1.5` | ~130 MB | 384 | Recommended — good balance of speed and quality |
| `all-minilm-l6-v2` | ~90 MB | 384 | Fastest, slightly lower quality |
| `bge-base-en-v1.5` | ~440 MB | 768 | Better quality, slower indexing |
| `nomic-embed-text-v1.5` | ~550 MB | 768 | Best quality |

### Model cache location

Models are downloaded once and cached at:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Caches/herbalist-mcp/` |
| Linux | `~/.cache/herbalist-mcp/` |
| Windows | `%LOCALAPPDATA%\herbalist-mcp\` |

Override with the `FASTEMBED_CACHE_DIR` environment variable.

To use a pre-downloaded model and skip the download entirely, pass `--model-path <dir>` pointing to a directory containing `model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, and `tokenizer_config.json`.

## MCP Configuration

The vault must be indexed before the MCP server can start. Run `herbalist-mcp index --vault <path>` once first, then add to your MCP client config:

### Claude Desktop / Claude Code

**Claude Desktop** (`claude_desktop_config.json`) and **Claude Code** (`~/.claude.json`):

```json
{
  "mcpServers": {
    "herbalist": {
      "command": "/path/to/herbalist-mcp",
      "args": ["serve", "--vault", "/path/to/your/vault"],
      "env": {
        "HERBALIST_LOG": "herbalist_mcp=warn"
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
        "HERBALIST_LOG": "herbalist_mcp=warn"
      }
    }
  }
}
```

### VS Code (GitHub Copilot)

MCP support requires VS Code 1.99 or later with the GitHub Copilot extension.

**Workspace-scoped** — create `.vscode/mcp.json` in your project:

```json
{
  "servers": {
    "herbalist": {
      "type": "stdio",
      "command": "/path/to/herbalist-mcp",
      "args": ["serve", "--vault", "/path/to/your/vault"],
      "env": {
        "HERBALIST_LOG": "herbalist_mcp=warn"
      }
    }
  }
}
```

**User-scoped** — create `mcp.json` in the VS Code user config directory:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/Code/User/mcp.json` |
| Linux | `~/.config/Code/User/mcp.json` |
| Windows | `%APPDATA%\Code\User\mcp.json` |

```json
{
  "servers": {
    "herbalist": {
      "type": "stdio",
      "command": "/path/to/herbalist-mcp",
      "args": ["serve", "--vault", "/path/to/your/vault"],
      "env": {
        "HERBALIST_LOG": "herbalist_mcp=warn"
      }
    }
  }
}
```

After saving, enable the server via the MCP: Enable/Disable Servers command or the Copilot chat toolbar.

### Environment variables

| Variable | Description |
|----------|-------------|
| `HERBALIST_VAULT` | Vault path — equivalent to `--vault` flag |
| `HERBALIST_LOG` | Log level filter (output goes to stderr). Default: `herbalist_mcp=info` |
| `FASTEMBED_CACHE_DIR` | Override the model download cache directory |

## MCP Tools

### `search_notes`

Semantic + keyword search over the vault. Embeds the query with the same model used during indexing, ranks note sections by cosine similarity, and blends in FTS5 BM25 results with a small boost for chunks that appear in both.

```
search_notes(query: "herbs for anxiety and sleep", top_k: 10)
```

Returns: `[{ path, section, snippet, score }]`

### `get_note`

Read a note's full content with its graph context — frontmatter, tags, outbound wikilinks, and backlinks from other notes.

```
get_note(path: "Herbs/Chamomile.md")
```

Returns: `{ path, content, frontmatter, tags, outlinks, backlinks }`

### `related_notes`

Find notes structurally similar to a given note based on the wikilink graph. Uses Cleora embeddings — notes that share similar link neighborhoods rank higher.

```
related_notes(path: "Herbs/Chamomile.md", top_k: 10)
```

Returns: `[{ path, score }]`

### `list_tags`

List all tags present in the vault (from YAML frontmatter `tags:` fields and inline `#tag` syntax).

```
list_tags()
```

Returns: `{ tags: ["anti-inflammatory", "nervine", "sleep", ...] }`

### `notes_by_tag`

Return all notes that have a given tag.

```
notes_by_tag(tag: "anti-inflammatory")
```

Returns: `{ paths: ["Herbs/Turmeric.md", "Herbs/Ginger.md", ...] }`

### `graph_neighbors`

Traverse the `[[wikilink]]` graph from a note up to `depth` hops in both directions (outlinks and backlinks).

```
graph_neighbors(path: "Herbs/Chamomile.md", depth: 2)
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
6. Embed all new chunks in batch via fastembed (configured model)
7. Recompute Cleora graph embeddings from the updated wikilink adjacency

All operations run on a single SQLite connection with WAL mode. The file watcher debounces changes by 500ms before triggering a single-file reindex.

### Embeddings

**Neural (chunk-level)**: fastembed runs ONNX-format models in-process via ONNX Runtime. Embeddings are stored as raw `f32` little-endian blobs in the `chunks.embedding` column.

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
| `config` | Persisted settings (model name, embedding dimension) |

## Development

```bash
cargo build           # debug build
cargo test            # 6 unit tests (chunker, frontmatter, wikilinks)
cargo build --release
```

To reset the index (also clears the model choice — you will be prompted again):

```bash
rm ~/notes/.herbalist.db
herbalist-mcp index --vault ~/notes
```
