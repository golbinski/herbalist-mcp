use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawVersioning {
    resource_key: Option<String>,
    revision_key: Option<String>,
    revision_of_weight: Option<f32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawVaultConfig {
    #[serde(default)]
    versioning: Option<RawVersioning>,
}

/// Fully resolved versioning config for a scope.
#[derive(Debug, Clone)]
pub struct VersioningConfig {
    /// Frontmatter key whose value identifies the logical document.
    pub resource_key: String,
    /// Frontmatter key holding epoch timestamp (integer) for ordering.
    pub revision_key: String,
    /// Cleora edge weight for revision_of edges (default 0.5).
    pub revision_of_weight: f32,
}

/// Effective config tied to a vault subdirectory scope.
#[derive(Debug, Clone)]
pub struct ScopedConfig {
    /// Vault-relative directory (empty string = vault root).
    pub scope: String,
    pub versioning: VersioningConfig,
}

/// All config information loaded from a vault's `.herbalist.yaml` files.
pub struct LoadedConfigs {
    /// Every vault-relative directory that contains a `.herbalist.yaml`,
    /// sorted shallowest first. These are the namespace boundaries.
    pub all_scopes: Vec<String>,
    /// Scopes where the merged config resolves to a complete VersioningConfig,
    /// sorted shallowest first.
    pub versioned: Vec<ScopedConfig>,
}

impl LoadedConfigs {
    pub fn empty() -> Self {
        Self {
            all_scopes: vec![],
            versioned: vec![],
        }
    }
}

impl RawVersioning {
    fn overlay(base: Self, top: Self) -> Self {
        Self {
            resource_key: top.resource_key.or(base.resource_key),
            revision_key: top.revision_key.or(base.revision_key),
            revision_of_weight: top.revision_of_weight.or(base.revision_of_weight),
        }
    }

    fn resolve(self) -> Option<VersioningConfig> {
        Some(VersioningConfig {
            resource_key: self.resource_key?,
            revision_key: self.revision_key?,
            revision_of_weight: self.revision_of_weight.unwrap_or(0.5),
        })
    }
}

/// Walk the vault and load all `.herbalist.yaml` files into a `LoadedConfigs`.
pub fn load(vault: &Path) -> Result<LoadedConfigs> {
    use ignore::WalkBuilder;

    let mut raw_by_dir: BTreeMap<String, RawVersioning> = BTreeMap::new();

    let walker = WalkBuilder::new(vault)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".obsidian" | ".git")
        })
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_name() != ".herbalist.yaml" {
            continue;
        }
        let dir = entry.path().parent().unwrap_or(vault);
        let rel = dir_relative(vault, dir);
        match std::fs::read_to_string(entry.path()) {
            Ok(text) => {
                let raw: RawVaultConfig = serde_yaml::from_str(&text).unwrap_or_default();
                raw_by_dir.insert(rel, raw.versioning.unwrap_or_default());
            }
            Err(e) => tracing::warn!("cannot read {}: {e}", entry.path().display()),
        }
    }

    let mut all_scopes: Vec<String> = raw_by_dir.keys().cloned().collect();
    all_scopes.sort_by_key(|s| {
        if s.is_empty() {
            0
        } else {
            s.matches('/').count() + 1
        }
    });

    let mut versioned: Vec<ScopedConfig> = Vec::new();
    for scope in &all_scopes {
        let merged = merged_versioning(scope, &raw_by_dir);
        if let Some(v) = merged.resolve() {
            versioned.push(ScopedConfig {
                scope: scope.clone(),
                versioning: v,
            });
        }
    }

    Ok(LoadedConfigs {
        all_scopes,
        versioned,
    })
}

/// Merge raw versioning configs from vault root down to `scope`.
fn merged_versioning(scope: &str, raw_by_dir: &BTreeMap<String, RawVersioning>) -> RawVersioning {
    let mut acc = RawVersioning::default();

    // Root config first
    if let Some(v) = raw_by_dir.get("") {
        acc = RawVersioning::overlay(acc, v.clone());
    }

    if scope.is_empty() {
        return acc;
    }

    // Walk each path component from root toward scope
    let mut prefix = String::new();
    for part in scope.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        if let Some(v) = raw_by_dir.get(&prefix) {
            acc = RawVersioning::overlay(acc, v.clone());
        }
    }

    acc
}

/// The namespace for a note is the deepest ancestor directory that owns a
/// `.herbalist.yaml`.  Returns `None` if no config file exists anywhere above
/// the note.
///
/// `all_scopes` must be sorted shallowest first.
pub fn find_namespace<'a>(note_path: &str, all_scopes: &'a [String]) -> Option<&'a str> {
    all_scopes
        .iter()
        .rev() // deepest first
        .find(|scope| scope_contains(scope, note_path))
        .map(|s| s.as_str())
}

/// Find the effective `ScopedConfig` for a given namespace.
/// Returns `None` if no versioning config covers this namespace.
///
/// `versioned` must be sorted shallowest first.
pub fn versioning_for_namespace<'a>(
    namespace: &str,
    versioned: &'a [ScopedConfig],
) -> Option<&'a ScopedConfig> {
    // The namespace IS a scope boundary; find the deepest ScopedConfig that
    // is an ancestor of (or equal to) the namespace.
    versioned
        .iter()
        .rev()
        .find(|sc| sc.scope == namespace || scope_contains(&sc.scope, namespace))
}

/// True when `scope` is an ancestor of (or equal to) `path`.
fn scope_contains(scope: &str, path: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    path == scope || path.starts_with(&format!("{}/", scope))
}

fn dir_relative(vault: &Path, dir: &Path) -> String {
    dir.strip_prefix(vault)
        .unwrap_or(dir)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_contains_root() {
        assert!(scope_contains("", "any/path.md"));
        assert!(scope_contains("", "root.md"));
    }

    #[test]
    fn scope_contains_subdir() {
        assert!(scope_contains("papers", "papers/essay.md"));
        assert!(scope_contains("papers", "papers/deep/essay.md"));
        assert!(!scope_contains("papers", "papers-old/essay.md"));
        assert!(!scope_contains("papers", "other/essay.md"));
    }

    #[test]
    fn find_namespace_deepest_wins() {
        let scopes = vec!["".to_owned(), "papers".to_owned(), "papers/2024".to_owned()];
        assert_eq!(
            find_namespace("papers/2024/essay.md", &scopes),
            Some("papers/2024")
        );
        assert_eq!(find_namespace("papers/essay.md", &scopes), Some("papers"));
        assert_eq!(find_namespace("notes/daily.md", &scopes), Some(""));
        assert_eq!(find_namespace("notes/daily.md", &[]), None);
    }

    #[test]
    fn raw_versioning_merge_overlay_wins() {
        let base = RawVersioning {
            resource_key: Some("id".to_owned()),
            revision_key: Some("rev".to_owned()),
            revision_of_weight: None,
        };
        let top = RawVersioning {
            resource_key: Some("doc-id".to_owned()),
            revision_key: None,
            revision_of_weight: Some(0.3),
        };
        let merged = RawVersioning::overlay(base, top);
        assert_eq!(merged.resource_key.as_deref(), Some("doc-id"));
        assert_eq!(merged.revision_key.as_deref(), Some("rev"));
        let resolved = merged.resolve().unwrap();
        assert!((resolved.revision_of_weight - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn raw_versioning_default_weight() {
        let raw = RawVersioning {
            resource_key: Some("id".to_owned()),
            revision_key: Some("rev".to_owned()),
            revision_of_weight: None,
        };
        let resolved = raw.resolve().unwrap();
        assert!((resolved.revision_of_weight - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn raw_versioning_resolve_requires_both_keys() {
        let raw = RawVersioning {
            resource_key: Some("id".to_owned()),
            ..Default::default()
        };
        assert!(raw.resolve().is_none());
    }
}
