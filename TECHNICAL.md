# Technical Background

This document explains the technologies and algorithms inside herbalist-mcp. It is written for software developers who are not necessarily familiar with machine learning or information retrieval. The goal is to give you enough understanding to evaluate whether this tool is appropriate for your use case and to feel confident about what is running on your machine.

---

## Contents

1. [What are text embeddings?](#what-are-text-embeddings)
2. [The embedding models](#the-embedding-models)
3. [ONNX Runtime and fastembed](#onnx-runtime-and-fastembed)
4. [Hybrid search: BM25 + cosine similarity](#hybrid-search-bm25--cosine-similarity)
5. [Cleora: graph embeddings from wikilinks](#cleora-graph-embeddings-from-wikilinks)
6. [SQLite and FTS5](#sqlite-and-fts5)
7. [Supply chain and security](#supply-chain-and-security)

---

## What are text embeddings?

An embedding is a way of turning a piece of text into a list of numbers — a vector — such that texts with similar *meaning* end up with similar vectors.

For example, the sentences "herbs that help with sleep" and "plants used for insomnia" are phrased differently but mean roughly the same thing. A good embedding model will place their vectors close together in space (high cosine similarity), while "tax filing deadlines" would end up far away.

This is what makes semantic search possible. Traditional keyword search requires the query to share words with the document. Embedding-based search matches on meaning even when the wording differs.

Concretely, each model produces a vector of fixed length — 384 or 768 floating-point numbers depending on the model. Vectors are stored in the database and searched at query time by computing the cosine similarity between the query vector and every stored chunk vector.

**Cosine similarity** measures the angle between two vectors. Two identical vectors have similarity 1.0; two completely unrelated vectors are near 0.0; two vectors pointing in opposite directions would be -1.0 (rare in practice for text). Results are ranked by this score.

---

## The embedding models

herbalist-mcp offers four models from fastembed's curated registry. All four are *sentence transformers* — neural networks fine-tuned to produce meaningful embeddings for natural language.

### BGE Small EN v1.5 (recommended, ~130 MB)

**Who made it:** Beijing Academy of Artificial Intelligence (BAAI), published on Hugging Face.

**What it is:** "BGE" stands for BAAI General Embedding. The small variant has 33 million parameters. It was trained on a large corpus of English text pairs (question/answer, similar sentences, etc.) using contrastive learning — pairs of similar texts were pulled together in vector space while dissimilar pairs were pushed apart.

**Dimensions:** 384. Fast to embed, good quality. The recommended default.

### All-MiniLM-L6-v2 (~90 MB)

**Who made it:** Microsoft, released under Apache 2.0 via the `sentence-transformers` project.

**What it is:** A distilled model — a smaller network trained to approximate the behaviour of a much larger teacher model. "L6" means 6 transformer layers. Originally derived from MiniLM, then fine-tuned on over 1 billion sentence pairs. Very fast, slightly lower retrieval quality than BGE Small.

**Dimensions:** 384.

### BGE Base EN v1.5 (~440 MB)

**Who made it:** BAAI, same family as BGE Small.

**What it is:** The base variant with 110 million parameters — noticeably better retrieval quality than the small model, but around 3× slower to embed and 3× larger on disk.

**Dimensions:** 768. Worth considering for larger vaults where retrieval accuracy matters more than indexing speed.

### Nomic Embed Text v1.5 (~550 MB)

**Who made it:** Nomic AI, released under Apache 2.0 with full training code and data published.

**What it is:** A fully open-source embedding model (code, weights, and training pipeline are all public — unlike most models in this space). Uses a long-context architecture that handles documents up to 8192 tokens. Highest retrieval quality of the four options; slowest to run.

**Dimensions:** 768.

### Why the models are safe to run locally

These are *embedding* models, not generative LLMs. They do not produce text, do not have internet access, and do not call external services at runtime. They read text in and produce a vector of numbers out — nothing else. The ONNX weights are static files; they do not change or learn from your data. Your notes never leave your machine.

---

## ONNX Runtime and fastembed

### ONNX Runtime

ONNX (Open Neural Network Exchange) is an open standard for representing machine learning models. It is maintained by the Linux Foundation and supported by Microsoft, Meta, AMD, Intel, NVIDIA, and others. An ONNX file is a portable, self-contained description of a neural network's computation graph.

ONNX Runtime is the reference inference engine for ONNX models — a C++ library that loads an ONNX file and executes it efficiently on CPU (or GPU, though herbalist-mcp uses CPU only). It is the same runtime used in production by Microsoft Office, Windows, Azure Cognitive Services, and many other products. It is not experimental software.

The runtime is statically linked into the herbalist-mcp binary — there is no separate process, no server, and no system dependency to install.

### fastembed

[fastembed](https://github.com/Qdrant/fastembed-rs) is a Rust library maintained by Qdrant (the vector database company) that wraps ONNX Runtime with a high-level API for text embedding. It handles:

- Downloading and caching model weights from Hugging Face on first use
- Tokenising input text (splitting text into the sub-word tokens the model understands)
- Running the ONNX graph
- Returning the resulting embedding vectors

fastembed pins the SHA256 checksum of every model it ships. When it downloads a model, it verifies the download matches the expected hash before using it — the same principle as a package manager's lockfile.

---

## Hybrid search: BM25 + cosine similarity

herbalist-mcp combines two fundamentally different retrieval methods and merges their results.

### BM25 (keyword search)

BM25 (Best Match 25) is a ranking function from 1994 that has remained the standard for keyword-based document retrieval for three decades. It scores documents by how often the query terms appear in them, with two corrections:

1. **Term frequency saturation:** a word appearing 10 times in a document is not 10× more relevant than one appearing once — BM25 applies a diminishing return.
2. **Document length normalisation:** a short document where the query term appears twice is more relevant than a long document where it appears twice buried in thousands of words.

SQLite's FTS5 extension implements BM25 natively. It operates over a full-text index of all chunk content and headings.

BM25 is very good at exact and near-exact matches. It fails when the query and document use different words for the same concept.

### Cosine similarity (semantic search)

As described above: embed the query with the same model used during indexing, then rank all stored chunk embeddings by cosine similarity to the query vector.

This is good at meaning-based matches and synonyms. It can be confused by short or ambiguous queries.

### How they are merged

After retrieving the top results from each method independently, the scores are normalised to the same scale and combined. Chunks that appear in *both* result sets receive a small bonus — the intuition is that if both a keyword match and a semantic match agree on a result, that agreement is meaningful signal.

The final ranked list is deduplicated by note and trimmed to `top_k`.

---

## Cleora: graph embeddings from wikilinks

`related_notes` works differently from `search_notes`. Instead of matching on text content, it finds notes that have similar *positions in the link graph* — notes that link to and are linked from similar sets of other notes.

### What Cleora is

Cleora is a graph embedding algorithm published by Synerise in 2021 ([paper](https://arxiv.org/abs/2102.02302)). It is simple, fast, and deterministic — no training, no gradient descent, no randomness. It works purely through linear algebra over the graph's adjacency structure.

### How it works in herbalist-mcp

1. **Initialise.** Each note gets a starting vector of length 128, computed deterministically from the hash of its file path. These initial vectors are random-looking but stable — the same note always gets the same starting vector regardless of when it was added.

2. **Propagate.** For each note, replace its vector with the mean of its neighbours' current vectors (both notes it links to and notes that link to it), then L2-normalise the result (scale the vector to unit length). Repeat this three times.

3. **Store.** The final vectors are written to the `note_embeddings` table.

After propagation, notes that share similar link neighbourhoods end up with similar vectors. If Chamomile links to and is linked from many of the same notes as Lavender, their embeddings will be close in vector space. This captures structural similarity that text content alone cannot — two notes might use completely different words yet be deeply interrelated in the graph.

Cleora is recomputed from scratch after every index run. At the vault sizes herbalist-mcp targets (hundreds to low thousands of notes), this takes milliseconds.

### Why not use the text embeddings for `related_notes`?

You could — and it would give you semantically similar notes. But structural similarity (shared link neighbourhood) and semantic similarity (similar text) capture different things. A category note ("Nervines") might be text-dissimilar to "Chamomile" but structurally very close because every nervine herb links through it. Cleora finds that relationship; text embeddings would not.

---

## SQLite and FTS5

The entire index — notes metadata, chunk content, embeddings, link graph, tags, config — lives in a single SQLite file at `<vault>/.herbalist.db`.

SQLite runs in-process (no server), is [the most widely deployed database engine in the world](https://www.sqlite.org/mostdeployed.html), and is included in a bundled form in the binary (via `rusqlite`'s bundled feature) so there is no system SQLite dependency.

**WAL mode** (Write-Ahead Logging) is enabled. This allows reads and writes to proceed concurrently — the file watcher can write an updated note while the MCP server is reading results for a query.

**FTS5** is SQLite's full-text search extension. It maintains an inverted index over chunk content and headings, and implements BM25 natively via the `rank` column. The `chunks_fts` virtual table is a content table backed by `chunks` — the content is stored once, not duplicated.

**Embeddings** are stored as raw `f32` little-endian byte blobs. A 384-dimension embedding takes 384 × 4 = 1536 bytes. At 50 chunks per note and 500 notes, that is around 37 MB of embedding data — well within SQLite's comfortable operating range.

---

## Supply chain and security

### Model downloads

On first run of `herbalist-mcp index`, the chosen model is downloaded from Hugging Face via fastembed. fastembed pins the expected SHA256 hash of each model file and verifies it after download. A tampered or corrupted download will be rejected.

Models are cached at the platform cache directory (`~/Library/Caches/herbalist-mcp/` on macOS, `~/.cache/herbalist-mcp/` on Linux) and reused on subsequent runs. To skip the download entirely and use a model you have obtained and verified through your own process, pass `--model-path <dir>`.

### Binary distribution

Release binaries are built by GitHub Actions on official GitHub-hosted runners. Each binary has:

- A **SHA256 checksum** published alongside it
- A **GitHub build provenance attestation** (SLSA level 2) — a signed record that links the binary to the exact commit and workflow run that produced it, verifiable with `gh attestation verify`
- A **VirusTotal scan** result linked from the release

### Rust crate dependencies

All Rust dependencies are pinned in `Cargo.lock`. The CI pipeline runs `cargo deny check` on every push, which verifies:

- No known CVEs in the dependency tree (checked against the RustSec advisory database)
- All dependency licenses are in the approved list
- All git dependencies come from explicitly allowlisted repositories

The only git dependency is `rmcp` (the MCP SDK), pulled from `github.com/modelcontextprotocol/rust-sdk` — the official repository maintained by Anthropic.
