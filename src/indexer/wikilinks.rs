use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static WIKILINK_RE: OnceLock<Regex> = OnceLock::new();

fn wikilink_re() -> &'static Regex {
    WIKILINK_RE.get_or_init(|| Regex::new(r"\[\[([^\]|#]+)(?:[|#][^\]]*)?]]").unwrap())
}

/// Extract raw wikilink targets from markdown content.
/// `[[Note Name]]` → `"Note Name"`
/// `[[Note Name|Display]]` → `"Note Name"`
/// `[[Note Name#Heading]]` → `"Note Name"`
pub fn extract_targets(content: &str) -> Vec<String> {
    wikilink_re()
        .captures_iter(content)
        .map(|cap| cap[1].trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Build a name → path map from all vault markdown files.
/// Key is the stem (filename without `.md`), lowercased for case-insensitive lookup.
/// If two files have the same stem, last writer wins (vault should avoid duplicates).
pub fn build_name_map(md_files: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for path in md_files {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            map.insert(stem.to_lowercase(), path.clone());
        }
    }
    map
}

/// Resolve a wikilink target string to an actual file path using the name map.
/// Returns `None` if the target cannot be resolved.
pub fn resolve(target: &str, name_map: &HashMap<String, PathBuf>) -> Option<PathBuf> {
    // Strip any leading path components (Obsidian allows [[Folder/Note]])
    let stem = Path::new(target)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(target);
    name_map.get(&stem.to_lowercase()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract() {
        let md = "See [[Alpha]] and [[Beta|display]] and [[Gamma#section]].";
        let targets = extract_targets(md);
        assert_eq!(targets, vec!["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn test_resolve() {
        let paths = vec![
            PathBuf::from("/vault/Alpha.md"),
            PathBuf::from("/vault/sub/Beta.md"),
        ];
        let map = build_name_map(&paths);
        assert_eq!(
            resolve("Alpha", &map),
            Some(PathBuf::from("/vault/Alpha.md"))
        );
        assert_eq!(
            resolve("beta", &map),
            Some(PathBuf::from("/vault/sub/Beta.md"))
        );
        assert_eq!(resolve("Missing", &map), None);
    }
}
