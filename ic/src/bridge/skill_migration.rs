//! V1 → V2 skill migration.
//!
//! Converts v1 `LoadedSkill` instances (from filesystem SKILL.md files) into
//! v2 `MemoryDoc` with `DocType::Skill` and structured `V2SkillMetadata`.
//! The migration is idempotent: skills with unchanged content_hash are skipped.
//!
//! **Remove after v1 migration is complete.** Once all users are on ENGINE_V2
//! and SKILL.md files are authored directly as v2 MemoryDocs (or via the
//! skill-extraction mission), this one-time migration code is unnecessary.
//! The `migrate_v1_skills` / `migrate_v1_skill_list` functions and the call
//! site in `bridge/router.rs:init_engine()` can all be deleted.

use std::sync::Arc;

use lunarwing_engine::traits::store::Store;
use lunarwing_engine::types::error::EngineError;
use lunarwing_engine::types::memory::{DocType, MemoryDoc};
use lunarwing_engine::types::project::ProjectId;
use lunarwing_engine::types::shared_owner_id;

use lunarwing_skills::SkillRegistry;
use lunarwing_skills::types::{LoadedSkill, SkillSource, SkillTrust};
use lunarwing_skills::v2::{V2SkillMetadata, V2SkillSource};

/// Migrate v1 skills to v2 MemoryDocs.
///
/// Reads all skills from the v1 `SkillRegistry`, converts each to a `MemoryDoc`
/// with `DocType::Skill` and `V2SkillMetadata`, and saves to the Store.
///
/// Returns the number of skills migrated or updated.
pub async fn migrate_v1_skills(
    v1_registry: &SkillRegistry,
    store: &Arc<dyn Store>,
    project_id: ProjectId,
) -> Result<usize, EngineError> {
    migrate_v1_skill_list(v1_registry.skills(), store, project_id).await
}

/// Migrate a snapshot of v1 skills to v2 MemoryDocs.
///
/// Takes a pre-cloned slice of skills (to avoid holding a lock across await).
pub async fn migrate_v1_skill_list(
    v1_skills: &[LoadedSkill],
    store: &Arc<dyn Store>,
    project_id: ProjectId,
) -> Result<usize, EngineError> {
    if v1_skills.is_empty() {
        return Ok(0);
    }

    // Load existing skill docs to check for duplicates by content_hash
    let existing_docs = store.list_shared_memory_docs(project_id).await?;
    let existing_hashes: std::collections::HashSet<String> = existing_docs
        .iter()
        .filter(|d| d.doc_type == DocType::Skill)
        .filter_map(|d| {
            serde_json::from_value::<V2SkillMetadata>(d.metadata.clone())
                .ok()
                .map(|m| m.content_hash)
        })
        .filter(|h| !h.is_empty())
        .collect();
    let existing_registry_installed_names: std::collections::HashSet<String> = existing_docs
        .iter()
        .filter(|doc| doc.doc_type == DocType::Skill)
        .filter_map(|doc| serde_json::from_value::<V2SkillMetadata>(doc.metadata.clone()).ok())
        .filter(|meta| meta.trust == SkillTrust::Installed && meta.registry_slug.is_some())
        .map(|meta| meta.name)
        .collect();

    let mut migrated = 0;

    for skill in v1_skills {
        // Engine V2 registry updates replace the durable MemoryDoc, while the
        // legacy filesystem copy may still contain the pre-update package.
        // Preserve the updated registry doc instead of re-importing stale
        // content as a duplicate on restart.
        if skill.trust == SkillTrust::Installed
            && existing_registry_installed_names.contains(skill.name())
        {
            tracing::debug!(
                skill = %skill.name(),
                "skipping v1 registry skill migration: durable registry doc already exists"
            );
            continue;
        }
        // Skip if content hasn't changed (idempotent)
        if existing_hashes.contains(&skill.content_hash) {
            tracing::debug!(
                skill = %skill.name(),
                "skipping v1 skill migration: content unchanged"
            );
            continue;
        }

        let doc = v1_skill_to_memory_doc(skill, project_id);
        store.save_memory_doc(&doc).await?;
        migrated += 1;

        tracing::debug!(
            skill = %skill.name(),
            doc_id = %doc.id.0,
            "migrated v1 skill to v2 MemoryDoc"
        );
    }

    if migrated > 0 {
        tracing::debug!("migrated {migrated} v1 skill(s) to v2 engine");
    }

    Ok(migrated)
}

/// Convert a single v1 `LoadedSkill` to a v2 `MemoryDoc`.
fn v1_skill_to_memory_doc(skill: &LoadedSkill, project_id: ProjectId) -> MemoryDoc {
    let v2_source = match &skill.source {
        SkillSource::Workspace(_) | SkillSource::User(_) | SkillSource::Installed(_) => {
            V2SkillSource::Migrated
        }
        SkillSource::Bundled(_) => V2SkillSource::Migrated,
    };

    let mut meta = V2SkillMetadata {
        name: skill.manifest.name.clone(),
        description: skill.manifest.description.clone(),
        activation: skill.manifest.activation.clone(),
        source: v2_source,
        trust: skill.trust,
        content_hash: skill.content_hash.clone(),
        // SAFETY: `"{}"` deserializes to all-default V2SkillMetadata (every
        // field is `#[serde(default)]`); infallible.
        ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
    };

    // B-3: stamp registry provenance from a `.registry.json` sidecar next to
    // the skill's SKILL.md, if present (written at install time for catalog
    // pulls). Missing sidecar = locally-authored → provenance stays None.
    if let Some(provenance) = read_registry_sidecar(&skill.source) {
        meta.registry_url = Some(provenance.registry_url);
        meta.registry_publisher = provenance.publisher;
        meta.registry_slug = provenance.slug;
        meta.registry_version = provenance.version;
        meta.pulled_at = Some(provenance.pulled_at);
        meta.registry_content_hash = Some(provenance.content_hash);
    }

    let mut doc = MemoryDoc::new(
        project_id,
        shared_owner_id(),
        DocType::Skill,
        format!("skill:{}", skill.manifest.name),
        &skill.prompt_content,
    );
    doc.metadata = serde_json::to_value(&meta).unwrap_or_default();
    doc.tags = vec!["migrated_from_v1".to_string()];
    doc
}

/// B-3 registry provenance sidecar (written at install time for catalog pulls).
#[derive(Debug, serde::Deserialize)]
struct RegistrySidecar {
    registry_url: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    version: Option<String>,
    pulled_at: chrono::DateTime<chrono::Utc>,
    content_hash: String,
}

/// Read a `.registry.json` sidecar from the skill's source directory. Returns
/// `None` if the sidecar is absent or unreadable (best-effort — a missing
/// sidecar just means the skill is locally-authored).
fn read_registry_sidecar(source: &SkillSource) -> Option<RegistrySidecar> {
    let path = match source {
        SkillSource::Workspace(p) | SkillSource::User(p) | SkillSource::Installed(p) => p,
        SkillSource::Bundled(_) => return None, // bundled skills ship with the app
    };
    // Skill sources record the skill's own directory (e.g.
    // <skills_root>/<name>), which is where the install-time writer places
    // the sidecar. Join directly; fall back to the parent only for legacy
    // file-shaped sources (a path ending in SKILL.md).
    let dir = if path.is_file() { path.parent()? } else { path };
    let sidecar = dir.join(".registry.json");
    let contents = std::fs::read_to_string(&sidecar).ok()?;
    serde_json::from_str(&contents).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunarwing_skills::types::{ActivationCriteria, SkillManifest, SkillTrust};
    use std::path::PathBuf;

    fn make_v1_skill(name: &str, content: &str) -> LoadedSkill {
        LoadedSkill {
            manifest: SkillManifest {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: format!("{name} skill"),
                activation: ActivationCriteria {
                    keywords: vec!["test".to_string()],
                    ..Default::default()
                },
                credentials: vec![],
                metadata: None,
            },
            prompt_content: content.to_string(),
            trust: SkillTrust::Trusted,
            source: SkillSource::User(PathBuf::from("/tmp/test")),
            content_hash: lunarwing_skills::compute_hash(content),
            compiled_patterns: vec![],
            lowercased_keywords: vec!["test".to_string()],
            lowercased_exclude_keywords: vec![],
            lowercased_tags: vec![],
        }
    }

    #[test]
    fn test_v1_skill_converts_to_memory_doc() {
        let skill = make_v1_skill("test-skill", "Test prompt content");
        let project_id = ProjectId::new();
        let doc = v1_skill_to_memory_doc(&skill, project_id);

        assert_eq!(doc.doc_type, DocType::Skill);
        assert_eq!(doc.title, "skill:test-skill");
        assert_eq!(doc.content, "Test prompt content");
        assert_eq!(doc.project_id, project_id);
        assert!(doc.tags.contains(&"migrated_from_v1".to_string()));

        let meta: V2SkillMetadata = serde_json::from_value(doc.metadata).unwrap();
        assert_eq!(meta.name, "test-skill");
        assert_eq!(meta.version, 1);
        assert_eq!(meta.source, V2SkillSource::Migrated);
        assert_eq!(meta.trust, SkillTrust::Trusted);
        assert!(meta.code_snippets.is_empty());
        assert!(!meta.content_hash.is_empty());
    }

    /// Regression: the install-time writer puts `.registry.json` inside the
    /// skill's own directory (`<skills_root>/<name>/.registry.json`), but the
    /// reader used `parent()` — one level too high, so provenance was never
    /// read. Source paths record the skill directory (registry.rs
    /// `make_source(path)` / `prepare_install_to_disk`), so the reader must
    /// join on the dir directly.
    #[test]
    fn test_registry_sidecar_read_from_skill_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Layout mirroring skills_install_handler: skills_root/<name>/ with
        // SKILL.md and the sidecar written next to it.
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# my-skill").unwrap();
        std::fs::write(
            skill_dir.join(".registry.json"),
            serde_json::json!({
                "registry_url": "https://registry.example.com",
                "slug": "acme/my-skill",
                "publisher": "acme",
                "version": "1.2.3",
                "content_hash": "abc123",
                "pulled_at": "2026-09-16T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();

        let provenance =
            read_registry_sidecar(&SkillSource::User(skill_dir.clone())).expect("sidecar read");
        assert_eq!(provenance.registry_url, "https://registry.example.com");
        assert_eq!(provenance.slug.as_deref(), Some("acme/my-skill"));
        assert_eq!(provenance.publisher.as_deref(), Some("acme"));
        assert_eq!(provenance.version.as_deref(), Some("1.2.3"));
        assert_eq!(provenance.content_hash, "abc123");

        // Provenance must reach the migrated doc's metadata (the end-to-end
        // contract the mismatch silently broke).
        let mut skill = make_v1_skill("my-skill", "Test prompt content");
        skill.source = SkillSource::User(skill_dir);
        let doc = v1_skill_to_memory_doc(&skill, ProjectId::new());
        let meta: V2SkillMetadata = serde_json::from_value(doc.metadata).unwrap();
        assert_eq!(
            meta.registry_url.as_deref(),
            Some("https://registry.example.com")
        );
        assert_eq!(meta.registry_slug.as_deref(), Some("acme/my-skill"));
        assert_eq!(meta.registry_publisher.as_deref(), Some("acme"));
        assert_eq!(meta.registry_version.as_deref(), Some("1.2.3"));
        assert!(meta.pulled_at.is_some());
        assert_eq!(meta.registry_content_hash.as_deref(), Some("abc123"));
    }

    /// A sidecar placed one level too high (the old buggy reader's target)
    /// must NOT be picked up, and a missing sidecar stays None.
    #[test]
    fn test_registry_sidecar_missing_or_misplaced() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# my-skill").unwrap();
        // Sidecar at the skills ROOT (old reader's wrong target): ignored.
        std::fs::write(
            tmp.path().join(".registry.json"),
            serde_json::json!({
                "registry_url": "https://registry.example.com",
                "content_hash": "abc123",
                "pulled_at": "2026-09-16T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();

        assert!(read_registry_sidecar(&SkillSource::User(skill_dir.clone())).is_none());

        // No sidecar anywhere: None (locally-authored skill).
        assert!(read_registry_sidecar(&SkillSource::User(skill_dir)).is_none());
    }

    /// Legacy file-shaped source (a path ending in SKILL.md) falls back to the
    /// parent directory, preserving pre-fix behavior for any such paths.
    #[test]
    fn test_registry_sidecar_legacy_file_shaped_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md, "# my-skill").unwrap();
        std::fs::write(
            skill_dir.join(".registry.json"),
            serde_json::json!({
                "registry_url": "https://registry.example.com",
                "content_hash": "abc123",
                "pulled_at": "2026-09-16T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();

        let provenance = read_registry_sidecar(&SkillSource::User(skill_md)).expect("sidecar read");
        assert_eq!(provenance.registry_url, "https://registry.example.com");
    }
}
