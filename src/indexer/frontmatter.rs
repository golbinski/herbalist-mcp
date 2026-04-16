/// Parsed YAML frontmatter from a markdown file.
#[derive(Debug, Default)]
pub struct Frontmatter {
    /// Raw key/value pairs (values serialized to string).
    pub fields: Vec<(String, String)>,
    /// Tags extracted from the `tags` field (list of strings).
    pub tags: Vec<String>,
}

/// Split a markdown document into (frontmatter_text, body_text).
/// Frontmatter is the content between the opening `---` and the closing `---`
/// on its own line. Returns `("", full_content)` if no frontmatter is present.
fn split_frontmatter(content: &str) -> (&str, &str) {
    let content = content.trim_start_matches('\u{feff}'); // strip BOM
    if !content.starts_with("---") {
        return ("", content);
    }
    // Find the newline after the opening ---
    let after_open = match content.find('\n') {
        Some(i) => i + 1,
        None => return ("", content),
    };
    // Find the closing ---
    let body = &content[after_open..];
    for (i, line) in body.lines().enumerate() {
        if line.trim() == "---" || line.trim() == "..." {
            let end = after_open + body.lines().take(i).map(|l| l.len() + 1).sum::<usize>();
            return (
                &content[after_open..end],
                &body[body.lines().take(i + 1).map(|l| l.len() + 1).sum::<usize>()..],
            );
        }
    }
    // No closing delimiter found — treat as no frontmatter
    ("", content)
}

pub fn parse(content: &str) -> (Frontmatter, String) {
    let (fm_text, body) = split_frontmatter(content);

    if fm_text.is_empty() {
        return (Frontmatter::default(), body.to_owned());
    }

    let mut fm = Frontmatter::default();

    if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(fm_text)
    {
        for (k, v) in &map {
            let key = yaml_value_to_string(k);
            if key == "tags" {
                fm.tags = extract_tags(v);
            } else {
                fm.fields.push((key, yaml_value_to_string(v)));
            }
        }
    } // malformed frontmatter — skip gracefully

    // Also pull tags from the fields list for convenience
    (fm, body.to_owned())
}

fn extract_tags(v: &serde_yaml::Value) -> Vec<String> {
    match v {
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .map(yaml_value_to_string)
            .filter(|s| !s.is_empty())
            .collect(),
        serde_yaml::Value::String(s) => s
            .split(',')
            .map(|t| t.trim().trim_start_matches('#').to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => vec![],
    }
}

fn yaml_value_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\ntitle: My Note\ntags:\n  - rust\n  - mcp\n---\n# Hello\nBody.";
        let (fm, body) = parse(content);
        assert_eq!(fm.tags, vec!["rust", "mcp"]);
        assert!(fm
            .fields
            .iter()
            .any(|(k, v)| k == "title" && v == "My Note"));
        assert!(body.contains("# Hello"));
    }

    #[test]
    fn test_no_frontmatter() {
        let content = "# Just a note\nNo frontmatter here.";
        let (fm, body) = parse(content);
        assert!(fm.fields.is_empty());
        assert!(fm.tags.is_empty());
        assert!(body.contains("# Just a note"));
    }
}
