use crate::config::{find_namespace, versioning_for_namespace, LoadedConfigs};
use crate::db::Db;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Compute and persist note roles based on the vault's versioning config.
///
/// Algorithm:
/// 1. Reset all notes to 'primary'.
/// 2. Delete all synthetic revision_of edges.
/// 3. For each namespace that has a complete VersioningConfig, find all notes
///    carrying both resource_key and revision_key frontmatter, group by
///    resource_id, and mark every non-head note as 'archival' with a
///    revision_of edge pointing at the head.
pub fn compute(db: &Arc<Mutex<Db>>, configs: &LoadedConfigs) -> Result<()> {
    if configs.versioned.is_empty() {
        return Ok(());
    }

    let db = db.lock().unwrap();

    db.reset_all_roles()?;
    db.delete_all_revision_links()?;

    let all_paths = db.all_note_paths()?;

    // Group note paths by namespace.
    let mut by_namespace: HashMap<String, Vec<String>> = HashMap::new();
    for path in &all_paths {
        if let Some(ns) = find_namespace(path, &configs.all_scopes) {
            by_namespace
                .entry(ns.to_owned())
                .or_default()
                .push(path.clone());
        }
        // Notes with no namespace stay 'primary' — nothing to do.
    }

    for namespace in by_namespace.keys() {
        let scoped = match versioning_for_namespace(namespace, &configs.versioned) {
            Some(sc) => sc,
            None => continue,
        };

        let resource_key = &scoped.versioning.resource_key;
        let revision_key = &scoped.versioning.revision_key;
        let weight = scoped.versioning.revision_of_weight;

        // Fetch all notes that have both keys (globally — filter by namespace below).
        let all_versioned = db.notes_with_versioning_keys(resource_key, revision_key)?;

        // Keep only notes whose namespace matches this scope.
        let in_scope: Vec<(String, String, i64)> = all_versioned
            .into_iter()
            .filter(|(path, _, _)| {
                find_namespace(path, &configs.all_scopes)
                    .map(|ns| ns == namespace.as_str())
                    .unwrap_or(false)
            })
            .collect();

        // Group by resource_id.
        let mut by_resource: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for (path, resource_id, revision) in in_scope {
            by_resource
                .entry(resource_id)
                .or_default()
                .push((path, revision));
        }

        for (_, mut group) in by_resource {
            if group.len() <= 1 {
                // Single version — already 'primary'.
                continue;
            }

            // Sort descending by revision; highest epoch = head.
            group.sort_by(|a, b| b.1.cmp(&a.1));
            let (head_path, _) = &group[0];

            for (path, _) in &group[1..] {
                db.set_note_role(path, "archival")?;
                db.insert_revision_link(path, head_path, weight)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LoadedConfigs, ScopedConfig, VersioningConfig};
    use crate::db::Db;
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

    fn make_db() -> Arc<Mutex<Db>> {
        let tmp = NamedTempFile::new().unwrap();
        Arc::new(Mutex::new(Db::open(tmp.path()).unwrap()))
    }

    fn simple_configs(resource_key: &str, revision_key: &str) -> LoadedConfigs {
        LoadedConfigs {
            all_scopes: vec!["".to_owned()],
            versioned: vec![ScopedConfig {
                scope: "".to_owned(),
                versioning: VersioningConfig {
                    resource_key: resource_key.to_owned(),
                    revision_key: revision_key.to_owned(),
                    revision_of_weight: 0.5,
                },
            }],
        }
    }

    fn seed_note(db: &Arc<Mutex<Db>>, path: &str, resource_id: &str, revision: i64) {
        let db = db.lock().unwrap();
        db.upsert_note(path, 0, path).unwrap();
        db.insert_frontmatter(path, "resource-id", resource_id)
            .unwrap();
        db.insert_frontmatter(path, "revision", &revision.to_string())
            .unwrap();
    }

    #[test]
    fn single_version_stays_primary() {
        let db = make_db();
        seed_note(&db, "essay.md", "my-essay", 1000);
        let configs = simple_configs("resource-id", "revision");
        compute(&db, &configs).unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .get_note_role("essay.md")
                .unwrap()
                .as_deref(),
            Some("primary")
        );
    }

    #[test]
    fn higher_revision_is_head() {
        let db = make_db();
        seed_note(&db, "essay-v1.md", "my-essay", 1000);
        seed_note(&db, "essay-v2.md", "my-essay", 2000);
        let configs = simple_configs("resource-id", "revision");
        compute(&db, &configs).unwrap();
        let db_guard = db.lock().unwrap();
        assert_eq!(
            db_guard.get_note_role("essay-v2.md").unwrap().as_deref(),
            Some("primary")
        );
        assert_eq!(
            db_guard.get_note_role("essay-v1.md").unwrap().as_deref(),
            Some("archival")
        );
    }

    #[test]
    fn archival_note_gets_revision_edge() {
        let db = make_db();
        seed_note(&db, "essay-v1.md", "my-essay", 1000);
        seed_note(&db, "essay-v2.md", "my-essay", 2000);
        let configs = simple_configs("resource-id", "revision");
        compute(&db, &configs).unwrap();

        let links = db.lock().unwrap().all_links().unwrap();
        let revision_edge = links
            .iter()
            .find(|(src, tgt, _)| src == "essay-v1.md" && tgt == "essay-v2.md");
        assert!(
            revision_edge.is_some(),
            "expected revision_of edge from v1 to v2"
        );
    }

    #[test]
    fn wikilink_preserved_alongside_role() {
        let db = make_db();
        seed_note(&db, "essay-v1.md", "my-essay", 1000);
        seed_note(&db, "essay-v2.md", "my-essay", 2000);

        // Add a body wikilink from v1 to some other note.
        {
            let db_guard = db.lock().unwrap();
            db_guard.upsert_note("other.md", 0, "other").unwrap();
            db_guard.insert_link("essay-v1.md", "other.md").unwrap();
        }

        let configs = simple_configs("resource-id", "revision");
        compute(&db, &configs).unwrap();

        let links = db.lock().unwrap().all_links().unwrap();
        let wikilink = links
            .iter()
            .find(|(src, tgt, _)| src == "essay-v1.md" && tgt == "other.md");
        assert!(
            wikilink.is_some(),
            "wikilink from archival note must be preserved"
        );
    }

    #[test]
    fn recompute_resets_stale_archival() {
        let db = make_db();
        seed_note(&db, "essay-v1.md", "my-essay", 1000);
        seed_note(&db, "essay-v2.md", "my-essay", 2000);
        let configs = simple_configs("resource-id", "revision");
        compute(&db, &configs).unwrap();

        // Remove the resource frontmatter from essay-v1 (simulates reindex)
        {
            let db_guard = db.lock().unwrap();
            db_guard.delete_frontmatter("essay-v1.md").unwrap();
        }
        compute(&db, &configs).unwrap();

        assert_eq!(
            db.lock()
                .unwrap()
                .get_note_role("essay-v1.md")
                .unwrap()
                .as_deref(),
            Some("primary"),
            "note without resource frontmatter should revert to primary"
        );
    }
}
