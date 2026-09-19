//! Context retrieval engine.
//!
//! Builds context for thread steps by retrieving relevant memory docs
//! from the project. Uses keyword matching against doc title + content,
//! with priority scoring by doc type (Lessons and Specs rank higher
//! than Summaries for context injection).

use std::collections::HashSet;
use std::sync::Arc;

use crate::executor::orchestrator::{
    FAILURE_TRACKER_TITLE, ORCHESTRATOR_META_TAG, ORCHESTRATOR_TAG, ORCHESTRATOR_TITLE,
};
use crate::executor::prompt::{PREAMBLE_OVERLAY_TITLE, PROMPT_OVERLAY_TAG};
use crate::runtime::mission::{FIX_PATTERN_DB_TAG, FIX_PATTERN_DB_TITLE};
use crate::traits::store::Store;
use crate::types::error::EngineError;
use crate::types::memory::{DocType, MemoryDoc};
use crate::types::project::ProjectId;

/// Retrieves relevant memory docs for a thread's context.
pub struct RetrievalEngine {
    store: Arc<dyn Store>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetrievalPurpose {
    Conversation,
    Mission,
}

impl RetrievalEngine {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Retrieve relevant conversational memory within a project.
    ///
    /// Automatic recall excludes engine-maintenance docs and skills, requires
    /// whole-token overlap, and returns empty for queries without relevant
    /// matches. An empty result is not evidence that no stored memory exists;
    /// explicit memory search remains available for deliberate investigation.
    pub async fn retrieve_context(
        &self,
        project_id: ProjectId,
        user_id: &str,
        query: &str,
        max_docs: usize,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        self.retrieve_for(
            project_id,
            user_id,
            query,
            max_docs,
            RetrievalPurpose::Conversation,
        )
        .await
    }

    /// Mission planning/maintenance can inspect internal docs and fall back to
    /// type priority + recency when the goal's vocabulary has no overlap.
    /// This policy is selected by the host, never by conversational model input.
    pub(crate) async fn retrieve_mission_context(
        &self,
        project_id: ProjectId,
        user_id: &str,
        query: &str,
        max_docs: usize,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        self.retrieve_for(
            project_id,
            user_id,
            query,
            max_docs,
            RetrievalPurpose::Mission,
        )
        .await
    }

    async fn retrieve_for(
        &self,
        project_id: ProjectId,
        user_id: &str,
        query: &str,
        max_docs: usize,
        purpose: RetrievalPurpose,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        let keywords = extract_keywords(query);
        if max_docs == 0 || (keywords.is_empty() && purpose == RetrievalPurpose::Conversation) {
            return Ok(Vec::new());
        }

        // Include both user-owned and shared system docs for context retrieval.
        let all_docs: Vec<MemoryDoc> = self
            .store
            .list_memory_docs_with_shared(project_id, user_id)
            .await?
            .into_iter()
            // Skills use deterministic activation in the orchestrator. Generic
            // retrieval must not bypass that selector and inject them as knowledge.
            .filter(|doc| doc.doc_type != DocType::Skill)
            .filter(|doc| purpose == RetrievalPurpose::Mission || !is_engine_maintenance(doc))
            .collect();
        if all_docs.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f64, MemoryDoc)> = all_docs
            .iter()
            .filter_map(|doc| {
                let keyword_score = keyword_match_score(doc, &keywords);
                // Zero-relevance docs must not ride the type-priority bonus:
                // the bonus ranks matching docs among themselves, it does not
                // turn unrelated docs into matches. (Filtering on the combined
                // score was a no-op — every doc type weighs above zero.)
                if keyword_score <= 0.0 {
                    return None;
                }
                Some((keyword_score + doc_type_weight(doc.doc_type), doc.clone()))
            })
            .collect();

        if scored.is_empty() && purpose == RetrievalPurpose::Mission {
            // Preserve broad mission recall without injecting unrelated records
            // into ordinary conversations.
            let mut fallback = all_docs;
            fallback.sort_by(|a, b| {
                let weight = doc_type_weight(b.doc_type)
                    .partial_cmp(&doc_type_weight(a.doc_type))
                    .unwrap_or(std::cmp::Ordering::Equal);
                weight.then(b.updated_at.cmp(&a.updated_at))
            });
            fallback.truncate(max_docs);
            return Ok(fallback);
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(max_docs);
        Ok(scored.into_iter().map(|(_, doc)| doc).collect())
    }
}

/// Reserved maintenance identities and tags also cover legacy untagged docs
/// and renamed maintenance docs. Shared ownership alone does not make a normal
/// knowledge document internal.
pub(crate) fn is_engine_maintenance(doc: &MemoryDoc) -> bool {
    doc.is_engine_control()
        || matches!(
            doc.title.as_str(),
            FIX_PATTERN_DB_TITLE
                | PREAMBLE_OVERLAY_TITLE
                | ORCHESTRATOR_TITLE
                | FAILURE_TRACKER_TITLE
        )
        || doc.tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                FIX_PATTERN_DB_TAG | PROMPT_OVERLAY_TAG | ORCHESTRATOR_TAG | ORCHESTRATOR_META_TAG
            )
        })
}

/// Keep identifiers such as `web_search` and `web-fetch` intact on both sides
/// of a match; punctuation and whitespace delimit words rather than substrings.
fn word_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Extract lowercase keywords from a query, filtering out stop words.
fn extract_keywords(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "about", "it",
        "its", "this", "that", "these", "those", "i", "you", "he", "she", "we", "they", "what",
        "which", "who", "how", "when", "where", "why", "and", "or", "but", "not", "no", "if",
        "then", "so", "up", "out", "just", "hi", "hello", "hey", "thanks", "thank", "please",
        "there", "now",
    ];

    let mut seen = HashSet::new();
    word_tokens(query)
        .filter(|w| w.len() >= 2 && !STOP_WORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Score how well a doc matches the given keywords (0.0 to 1.0).
fn keyword_match_score(doc: &MemoryDoc, keywords: &[String]) -> f64 {
    if keywords.is_empty() {
        return 0.0;
    }

    let title_tokens: HashSet<_> = word_tokens(&doc.title).collect();
    let content_tokens: HashSet<_> = word_tokens(&doc.content).collect();

    let mut matched = 0usize;
    for kw in keywords {
        // Title matches are worth more
        if title_tokens.contains(kw) {
            matched += 2;
        } else if content_tokens.contains(kw) {
            matched += 1;
        }
    }

    // Normalize: max possible score is keywords.len() * 2 (all in title)
    let max_score = keywords.len() * 2;
    matched as f64 / max_score as f64
}

/// Priority weight by doc type. Higher = more useful for context injection.
fn doc_type_weight(doc_type: DocType) -> f64 {
    match doc_type {
        DocType::Spec => 0.5,    // Missing capability info is highest priority
        DocType::Skill => 0.45,  // Skills with activation metadata and code snippets
        DocType::Lesson => 0.4,  // Lessons prevent repeating mistakes
        DocType::Issue => 0.2,   // Known problems
        DocType::Summary => 0.1, // Background context
        DocType::Note => 0.05,   // Scratch notes, lowest priority
        DocType::Plan => 0.3,    // Execution plans with structured steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::project::ProjectId;

    fn make_store(docs: Vec<MemoryDoc>) -> Arc<crate::tests::InMemoryStore> {
        Arc::new(crate::tests::InMemoryStore::with_docs(docs))
    }

    #[test]
    fn extract_keywords_filters_stop_words() {
        let kws = extract_keywords("what is the latest news about Iran war");
        assert!(kws.contains(&"latest".to_string()));
        assert!(kws.contains(&"news".to_string()));
        assert!(kws.contains(&"iran".to_string()));
        assert!(kws.contains(&"war".to_string()));
        assert!(!kws.contains(&"the".to_string()));
        assert!(!kws.contains(&"is".to_string()));
    }

    #[test]
    fn extract_keywords_handles_special_chars() {
        let kws = extract_keywords("web_search web-fetch tool");
        assert!(kws.contains(&"web_search".to_string()));
        assert!(kws.contains(&"web-fetch".to_string()));
        assert!(kws.contains(&"tool".to_string()));
    }

    #[test]
    fn extract_keywords_deduplicates_and_ignores_conversational_filler() {
        assert_eq!(
            extract_keywords("Hi there, please explain MEMORY_SEARCH memory_search now"),
            vec!["explain", "memory_search"]
        );
    }

    #[test]
    fn keyword_match_requires_whole_tokens_and_preserves_identifiers() {
        let doc = MemoryDoc::new(
            ProjectId::new(),
            "test-user",
            DocType::Lesson,
            "Web_Search résumé",
            "A prompt hint about concatenation and web-fetch.",
        );
        for query in ["hi", "cat", "web", "fetch", "search"] {
            assert_eq!(keyword_match_score(&doc, &[query.into()]), 0.0, "{query}");
        }
        for query in ["web_search", "WEB-FETCH", "RÉSUMÉ"] {
            assert!(
                keyword_match_score(&doc, &extract_keywords(query)) > 0.0,
                "{query}"
            );
        }
    }

    #[test]
    fn keyword_match_title_beats_content() {
        use crate::types::project::ProjectId;

        let doc = MemoryDoc::new(
            ProjectId::new(),
            "test-user",
            DocType::Lesson,
            "Lesson about web_search errors",
            "The tool was not found during execution.",
        );

        let keywords = vec!["web_search".to_string()];
        let score = keyword_match_score(&doc, &keywords);
        // Title match = 2/2 = 1.0
        assert!((score - 1.0).abs() < f64::EPSILON);

        let keywords2 = vec!["execution".to_string()];
        let score2 = keyword_match_score(&doc, &keywords2);
        // Content-only match = 1/2 = 0.5
        assert!((score2 - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn doc_type_weight_ordering() {
        assert!(doc_type_weight(DocType::Spec) > doc_type_weight(DocType::Lesson));
        assert!(doc_type_weight(DocType::Lesson) > doc_type_weight(DocType::Issue));
        assert!(doc_type_weight(DocType::Issue) > doc_type_weight(DocType::Summary));
        assert!(doc_type_weight(DocType::Summary) > doc_type_weight(DocType::Note));
    }

    #[tokio::test]
    async fn retrieve_returns_relevant_docs_by_keyword() {
        let project = ProjectId::new();
        let store = make_store(vec![
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Lesson,
                "web_search tool alias",
                "Use web_search",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Summary,
                "weather query",
                "Fetched weather data",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Issue,
                "API timeout",
                "External API timed out",
            ),
        ]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "web_search error", 5)
            .await
            .unwrap();
        assert!(!docs.is_empty());
        // The lesson about web_search should rank first (keyword + type weight)
        assert_eq!(docs[0].doc_type, DocType::Lesson);
        assert!(docs[0].title.contains("web_search"));
    }

    #[tokio::test]
    async fn retrieve_respects_project_scoping() {
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let store = make_store(vec![
            MemoryDoc::new(
                project_a,
                "test-user",
                DocType::Lesson,
                "Lesson for project A",
                "Some lesson",
            ),
            MemoryDoc::new(
                project_b,
                "test-user",
                DocType::Lesson,
                "Lesson for project B",
                "Other lesson",
            ),
        ]);
        let engine = RetrievalEngine::new(store);

        let docs_a = engine
            .retrieve_context(project_a, "test-user", "lesson", 5)
            .await
            .unwrap();
        assert_eq!(docs_a.len(), 1);
        assert!(docs_a[0].title.contains("project A"));

        let docs_b = engine
            .retrieve_context(project_b, "test-user", "lesson", 5)
            .await
            .unwrap();
        assert_eq!(docs_b.len(), 1);
        assert!(docs_b[0].title.contains("project B"));
    }

    #[tokio::test]
    async fn retrieve_respects_max_docs_limit() {
        let project = ProjectId::new();
        let store = make_store(vec![
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Lesson,
                "Lesson 1",
                "Content 1",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Lesson,
                "Lesson 2",
                "Content 2",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Lesson,
                "Lesson 3",
                "Content 3",
            ),
        ]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "lesson", 2)
            .await
            .unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[tokio::test]
    async fn mission_retrieval_zero_keyword_overlap_returns_recent_fallback() {
        let project = ProjectId::new();
        let mut old_note = MemoryDoc::new(
            project,
            "test-user",
            DocType::Note,
            "Audit: scheduler dispatch subsystem",
            "Trust boundary map for scheduler dispatch. NEXT FOCUS: web gateway.",
        );
        old_note.updated_at = chrono::Utc::now() - chrono::Duration::hours(5);
        let mut recent_note = MemoryDoc::new(
            project,
            "test-user",
            DocType::Note,
            "Audit: web-gateway auth",
            "Entry points and trust boundaries of the gateway auth surface.",
        );
        recent_note.updated_at = chrono::Utc::now();
        let store = make_store(vec![old_note, recent_note]);
        let engine = RetrievalEngine::new(store);

        // Vocabulary completely disjoint from every stored doc.
        let docs = engine
            .retrieve_mission_context(project, "test-user", "quantum flute repair", 5)
            .await
            .unwrap();

        // Mission planning retains broad recall when vocabulary has no overlap.
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title, "Audit: web-gateway auth");
        assert_eq!(docs[1].title, "Audit: scheduler dispatch subsystem");
    }

    #[tokio::test]
    async fn mission_retrieval_fallback_prefers_higher_priority_doc_types() {
        let project = ProjectId::new();
        let mut summary = MemoryDoc::new(
            project,
            "test-user",
            DocType::Summary,
            "Thread summary",
            "Did thread things.",
        );
        summary.updated_at = chrono::Utc::now();
        let lesson = MemoryDoc::new(
            project,
            "test-user",
            DocType::Lesson,
            "Lesson about tooling",
            "Verify against source systems.",
        );
        let store = make_store(vec![summary, lesson]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_mission_context(project, "test-user", "unrelated vocabulary here", 5)
            .await
            .unwrap();

        // Lesson outranks Summary in the fallback ordering even though the
        // Summary is more recent.
        assert_eq!(docs[0].doc_type, DocType::Lesson);
    }

    #[tokio::test]
    async fn retrieve_keyword_match_excludes_unrelated_docs() {
        let project = ProjectId::new();
        let mut recent_unrelated = MemoryDoc::new(
            project,
            "test-user",
            DocType::Note,
            "Totally different topic",
            "Quantum flutes and their repair history.",
        );
        recent_unrelated.updated_at = chrono::Utc::now();
        let matching = MemoryDoc::new(
            project,
            "test-user",
            DocType::Note,
            "Audit: scheduler dispatch",
            "The scheduler dispatch boundary.",
        );
        let store = make_store(vec![recent_unrelated, matching]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "scheduler dispatch", 5)
            .await
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "Audit: scheduler dispatch");
    }

    #[tokio::test]
    async fn conversational_retrieval_does_not_fall_back_for_greetings_or_unrelated_queries() {
        let project = ProjectId::new();
        let engine = RetrievalEngine::new(make_store(vec![MemoryDoc::new(
            project,
            "test-user",
            DocType::Lesson,
            "insight:workflow:review",
            "Prefer concise status updates. Add a prompt hint about tools.",
        )]));

        for query in [
            "",
            "the and is",
            "Hi there. This is dev build #19 of 2.1.2.0 now",
            "Hello!",
            "Thanks",
            "quantum flute repair",
        ] {
            let docs = engine
                .retrieve_context(project, "test-user", query, 5)
                .await
                .unwrap();
            assert!(docs.is_empty(), "unexpected recall for {query:?}");
        }
        // A genuine partial match remains useful even with extra query terms.
        let docs = engine
            .retrieve_context(project, "test-user", "review zebra unicorn", 5)
            .await
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "insight:workflow:review");
    }

    #[tokio::test]
    async fn conversational_retrieval_excludes_maintenance_by_title_or_tag() {
        let project = ProjectId::new();
        let mut fixtures = Vec::new();
        for owner in ["test-user", crate::types::shared_owner_id()] {
            for (title, tag) in [
                (FIX_PATTERN_DB_TITLE, FIX_PATTERN_DB_TAG),
                (PREAMBLE_OVERLAY_TITLE, PROMPT_OVERLAY_TAG),
                (ORCHESTRATOR_TITLE, ORCHESTRATOR_TAG),
                (FAILURE_TRACKER_TITLE, ORCHESTRATOR_META_TAG),
            ] {
                // Both a legacy untagged record and a renamed tagged record.
                fixtures.push(MemoryDoc::new(
                    project,
                    owner,
                    DocType::Note,
                    title,
                    "repair",
                ));
                fixtures.push(
                    MemoryDoc::new(project, owner, DocType::Note, "renamed", "repair")
                        .with_tags(vec![tag.into()]),
                );
            }
            fixtures.push(MemoryDoc::new(
                project,
                owner,
                DocType::Lesson,
                "repair handbook",
                "Normal knowledge",
            ));
        }
        // Scope filtering still applies even for genuine exact matches.
        fixtures.push(MemoryDoc::new(
            project,
            "other-user",
            DocType::Lesson,
            "repair private",
            "private",
        ));
        let store = make_store(fixtures);
        let engine = RetrievalEngine::new(store.clone());
        let docs = engine
            .retrieve_context(project, "test-user", "repair", 50)
            .await
            .unwrap();
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().all(|doc| doc.title == "repair handbook"));
        assert!(docs.iter().any(|doc| doc.owner_id().is_shared()));
        assert!(docs.iter().any(|doc| doc.user_id == "test-user"));

        let mission_docs = engine
            .retrieve_mission_context(project, "test-user", "repair", 50)
            .await
            .unwrap();
        assert_eq!(mission_docs.len(), 18);
        // Filtering affects automatic recall, not the stored records.
        assert_eq!(
            store
                .list_memory_docs_with_shared(project, "test-user")
                .await
                .unwrap()
                .len(),
            18
        );
    }

    #[tokio::test]
    async fn retrieve_empty_store_returns_empty() {
        let project = ProjectId::new();
        let store = make_store(vec![]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "anything", 5)
            .await
            .unwrap();
        assert!(docs.is_empty());
    }

    #[tokio::test]
    async fn retrieve_excludes_skills_from_generic_context() {
        let project = ProjectId::new();
        let store = make_store(vec![
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Skill,
                "skill:selection-proof",
                "SKILL_CONTEXT_PROOF",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Lesson,
                "selection lesson",
                "Use deterministic selection",
            ),
        ]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "selection proof", 5)
            .await
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].doc_type, DocType::Lesson);
        assert!(!docs[0].content.contains("SKILL_CONTEXT_PROOF"));
    }

    #[tokio::test]
    async fn retrieve_spec_ranks_above_summary() {
        let project = ProjectId::new();
        let store = make_store(vec![
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Summary,
                "Summary of search",
                "searched the web",
            ),
            MemoryDoc::new(
                project,
                "test-user",
                DocType::Spec,
                "Missing search tool",
                "Use web_search for the search tool",
            ),
        ]);
        let engine = RetrievalEngine::new(store);

        let docs = engine
            .retrieve_context(project, "test-user", "search", 5)
            .await
            .unwrap();
        assert_eq!(docs.len(), 2);
        // Spec should rank first due to higher type weight
        assert_eq!(docs[0].doc_type, DocType::Spec);
    }
}
