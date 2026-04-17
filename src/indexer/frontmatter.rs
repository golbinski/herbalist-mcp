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
    // Skip the opening --- line (handles both \n and \r\n)
    let after_open = match content.find('\n') {
        Some(i) => i + 1,
        None => return ("", content),
    };
    let rest = &content[after_open..];
    // Scan line-by-line using byte positions so \r\n is handled correctly.
    let mut pos = 0;
    while pos < rest.len() {
        let line_end = rest[pos..].find('\n').map_or(rest.len() - pos, |i| i + 1);
        // Strip line terminator(s) before comparing
        let line = rest[pos..pos + line_end].trim_end_matches(['\r', '\n']);
        if line == "---" || line == "..." {
            return (&rest[..pos], &rest[pos + line_end..]);
        }
        pos += line_end;
    }
    // No closing delimiter — treat entire content as having no frontmatter
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
