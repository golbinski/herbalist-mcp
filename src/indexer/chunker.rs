/// A single section of a markdown document.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The heading text (empty string for content before the first heading).
    pub heading: String,
    /// The body content of the section (heading line excluded).
    pub content: String,
}

/// Split a markdown document body into chunks at H1/H2 boundaries.
/// The preamble (content before the first heading) becomes a chunk with an empty heading.
/// Very short chunks (< 10 chars of content) are merged into the previous chunk.
pub fn chunk(body: &str) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current_heading = String::new();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body.lines() {
        if let Some(heading) = parse_heading(line) {
            // Flush accumulated content
            let content = current_lines.join("\n").trim().to_owned();
            if !content.is_empty() || chunks.is_empty() {
                chunks.push(Chunk {
                    heading: current_heading.clone(),
                    content,
                });
            }
            current_heading = heading;
            current_lines.clear();
        } else {
            current_lines.push(line);
        }
    }

    // Flush final section
    let content = current_lines.join("\n").trim().to_owned();
    if !content.is_empty() || chunks.is_empty() {
        chunks.push(Chunk {
            heading: current_heading,
            content,
        });
    }

    // Merge tiny chunks (< 10 chars) upward
    merge_tiny(chunks)
}

fn parse_heading(line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("## ") {
        Some(rest.trim().to_owned())
    } else {
        line.strip_prefix("# ").map(|rest| rest.trim().to_owned())
    }
}

fn merge_tiny(mut chunks: Vec<Chunk>) -> Vec<Chunk> {
    if chunks.len() <= 1 {
        return chunks;
    }
    let mut i = 1;
    while i < chunks.len() {
        if chunks[i].content.len() < 10 {
            let tiny = chunks.remove(i);
            let prev = &mut chunks[i - 1];
            if !tiny.content.is_empty() {
                if !prev.content.is_empty() {
                    prev.content.push('\n');
                }
                prev.content.push_str(&tiny.content);
            }
        } else {
            i += 1;
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_basic() {
        let body = "Intro text.\n# Section One\nContent one.\n## Subsection\nContent two.";
        let chunks = chunk(body);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].heading, "");
        assert!(chunks[0].content.contains("Intro"));
        assert_eq!(chunks[1].heading, "Section One");
        assert_eq!(chunks[2].heading, "Subsection");
    }

    #[test]
    fn test_no_headings() {
        let body = "Just plain content.";
        let chunks = chunk(body);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "");
        assert!(chunks[0].content.contains("plain"));
    }
}
