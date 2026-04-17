pub mod cleora;

use anyhow::{bail, Context, Result};
use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use std::path::Path;

/// Wrapper around a fastembed TextEmbedding model.
pub struct Embedder {
    inner: TextEmbedding,
    #[allow(dead_code)] // stored at index time; callers may inspect it
    pub dimension: usize,
}

impl Embedder {
    /// Load a model from fastembed's curated registry.
    /// Downloads and caches the model on first use (~90–550 MB depending on model).
    /// Cache location: $FASTEMBED_CACHE_DIR, or the platform cache dir:
    ///   macOS:   ~/Library/Caches/herbalist-mcp
    ///   Linux:   ~/.cache/herbalist-mcp
    ///   Windows: %LOCALAPPDATA%\herbalist-mcp
    pub fn from_registry(model: EmbeddingModel) -> Result<Self> {
        let dimension = model_dimension(&model);
        let cache_dir = model_cache_dir();
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;
        let inner = TextEmbedding::try_new(InitOptions::new(model).with_cache_dir(cache_dir))?;
        Ok(Self { inner, dimension })
    }

    /// Load an ONNX embedding model from a local directory.
    /// The directory must contain fastembed's expected files:
    ///   model.onnx, tokenizer.json, config.json, special_tokens_map.json, tokenizer_config.json
    pub fn from_path(path: &Path) -> Result<Self> {
        if !path.is_dir() {
            bail!(
                "--model-path must be a directory containing model.onnx + tokenizer files (got: {})",
                path.display()
            );
        }
        let read = |name: &str| -> Result<Vec<u8>> {
            std::fs::read(path.join(name))
                .with_context(|| format!("reading {} from {}", name, path.display()))
        };
        let onnx_file = read("model.onnx")?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let user_model = UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files);
        let inner = TextEmbedding::try_new_from_user_defined(
            user_model,
            InitOptionsUserDefined::default(),
        )?;
        // Probe real output dimension; fastembed doesn't expose it before inference.
        let probe = inner.embed(vec![""], None)?;
        let dimension = probe.first().map(|v| v.len()).unwrap_or(384);
        Ok(Self { inner, dimension })
    }

    /// Embed a batch of text strings. Returns one vector per input.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed(texts.to_vec(), None)
    }
}

// ── cosine similarity ─────────────────────────────────────────────────────────

/// Cosine similarity between two f32 vectors (dot product of L2-normalized vecs).
/// Returns 0.0 if either vector is zero.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "embedding dimension mismatch");
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < f32::EPSILON || norm_b < f32::EPSILON {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// ── model helpers ─────────────────────────────────────────────────────────────

/// Best-effort dimension lookup for known models.
fn model_dimension(model: &EmbeddingModel) -> usize {
    match model {
        EmbeddingModel::AllMiniLML6V2 | EmbeddingModel::AllMiniLML6V2Q => 384,
        EmbeddingModel::BGESmallENV15 | EmbeddingModel::BGESmallENV15Q => 384,
        EmbeddingModel::BGEBaseENV15 | EmbeddingModel::BGEBaseENV15Q => 768,
        EmbeddingModel::NomicEmbedTextV1
        | EmbeddingModel::NomicEmbedTextV15
        | EmbeddingModel::NomicEmbedTextV15Q => 768,
        _ => 384,
    }
}

/// Parse the model name from CLI --model flag into an EmbeddingModel enum.
pub fn model_from_name(name: &str) -> Result<EmbeddingModel> {
    match name.to_lowercase().as_str() {
        "bge-small-en-v1.5" | "bge-small" => Ok(EmbeddingModel::BGESmallENV15),
        "bge-base-en-v1.5" | "bge-base"   => Ok(EmbeddingModel::BGEBaseENV15),
        "all-minilm-l6-v2" | "minilm"     => Ok(EmbeddingModel::AllMiniLML6V2),
        "nomic-embed-text-v1.5" | "nomic" => Ok(EmbeddingModel::NomicEmbedTextV15),
        other => bail!(
            "unknown model '{}'. Choose from: bge-small-en-v1.5, bge-base-en-v1.5, all-minilm-l6-v2, nomic-embed-text-v1.5",
            other
        ),
    }
}

/// Resolve the model cache directory.
/// Respects FASTEMBED_CACHE_DIR env var; otherwise uses the platform cache dir.
pub fn model_cache_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("FASTEMBED_CACHE_DIR") {
        return std::path::PathBuf::from(dir);
    }
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("herbalist-mcp")
}
