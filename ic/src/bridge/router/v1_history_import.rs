//! One-time import of pre-Engine-V2 conversation history into an engine
//! conversation surface (WI#442 F7).
//!
//! The router's v1 dual-write is forward-only: Engine V2 messages land in the
//! relational database so the gateway history API keeps working, but rows the
//! tenant accumulated *before* switching to Engine V2 never reach the v2
//! conversation surface. A new engine thread is seeded from that surface
//! (`build_history_from_entries`), so without an import the model starts each
//! switched tenant's first thread with no prior context.
//!
//! The import runs once per v1 conversation, gated on a marker in that
//! conversation's metadata. It reads the v1 rows created before the engine
//! surface existed — the surface's `created_at` is the cutoff — so messages the
//! dual-write has already stored for v2 turns can never be re-imported, even if
//! the marker write fails and a later message retries.

use tracing::{debug, warn};

use lunarwing_engine::{ConversationEntry, ConversationId};
use uuid::Uuid;

use super::EngineState;

/// Marker key in the v1 conversation metadata once history has been imported.
pub(crate) const IMPORTED_MARKER: &str = "engine_v2_v1_history_imported";

/// Import pre-switch v1 history into the engine conversation if it has not
/// been imported yet.
///
/// Best-effort: any database or store failure is logged and the message path
/// continues, because a missing import degrades context but must never block
/// the user's turn. Returns the number of entries added when the import runs.
pub(crate) async fn import_v1_history_if_needed(
    state: &EngineState,
    conversation_id: ConversationId,
    v1_conversation_id: Uuid,
    user_id: &str,
) -> usize {
    let Some(db) = state.db.as_ref() else {
        return 0;
    };

    match db.get_conversation_metadata(v1_conversation_id).await {
        Ok(Some(metadata)) if metadata.get(IMPORTED_MARKER).is_some() => return 0,
        Ok(_) => {}
        Err(error) => {
            debug!(%error, conversation_id = %v1_conversation_id, "engine v2: v1 history marker lookup failed; skipping import");
            return 0;
        }
    }

    // The surface's creation time is the cutoff: every v1 row at or after it is
    // a forward dual-write from a v2 turn, not pre-switch history.
    let Some(surface) = state
        .conversation_manager
        .get_conversation(conversation_id)
        .await
    else {
        return 0;
    };
    let cutoff = surface.created_at;

    let rows = match db.list_conversation_messages(v1_conversation_id).await {
        Ok(rows) => rows,
        Err(error) => {
            debug!(%error, conversation_id = %v1_conversation_id, "engine v2: v1 history read failed; skipping import");
            return 0;
        }
    };

    let entries = importable_entries(rows, cutoff);

    let imported = if entries.is_empty() {
        0
    } else {
        match state
            .conversation_manager
            .import_v1_history(conversation_id, user_id, entries)
            .await
        {
            Ok(count) => count,
            Err(error) => {
                warn!(%error, conversation_id = %conversation_id, "engine v2: v1 history import failed");
                return 0;
            }
        }
    };

    // Mark after a successful import (or when there was nothing to import) so
    // re-runs skip the list query. A failed marker write only costs one extra
    // scan next turn: the cutoff keeps it duplicate-free.
    if let Err(error) = db
        .update_conversation_metadata_field(
            v1_conversation_id,
            IMPORTED_MARKER,
            &serde_json::json!(true),
        )
        .await
    {
        debug!(%error, conversation_id = %v1_conversation_id, "engine v2: failed to record v1 history import marker");
    }

    debug!(
        conversation_id = %conversation_id,
        imported,
        "engine v2: imported pre-switch v1 history"
    );
    imported
}

/// Map v1 rows to importable conversation entries.
///
/// Only `user` and `assistant` rows older than `cutoff` are conversation
/// context. `tool_calls`, `interrupted`, and any other v1 bookkeeping role are
/// skipped: the engine does not replay them, and importing them would put
/// non-conversational text into the seeded history. Rows at or after the cutoff
/// are the forward dual-write's own v2 turns.
fn importable_entries(
    rows: Vec<crate::history::ConversationMessage>,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Vec<ConversationEntry> {
    rows.into_iter()
        .filter(|row| row.created_at < cutoff)
        .filter_map(|row| match row.role.as_str() {
            "user" => Some(ConversationEntry::imported_user(
                &row.content,
                row.created_at,
            )),
            "assistant" => Some(ConversationEntry::imported_agent(
                &row.content,
                row.created_at,
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn row(
        role: &str,
        content: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> crate::history::ConversationMessage {
        crate::history::ConversationMessage {
            id: Uuid::new_v4(),
            role: role.to_string(),
            content: content.to_string(),
            created_at,
        }
    }

    #[test]
    fn only_user_and_assistant_rows_before_the_cutoff_are_imported() {
        let now = Utc::now();
        let cutoff = now;
        let rows = vec![
            row("user", "old question", now - Duration::minutes(10)),
            row("assistant", "old answer", now - Duration::minutes(9)),
            row("tool_calls", "[]", now - Duration::minutes(8)),
            row("interrupted", "", now - Duration::minutes(7)),
            // Forward dual-write from a v2 turn: at/after the surface creation.
            row("user", "v2 question", now),
            row("assistant", "v2 answer", now + Duration::seconds(1)),
        ];

        let entries = importable_entries(rows, cutoff);

        let contents: Vec<&str> = entries.iter().map(|e| e.content.as_str()).collect();
        assert_eq!(contents, ["old question", "old answer"]);
        assert!(matches!(
            entries[0].sender,
            lunarwing_engine::EntrySender::User
        ));
        assert!(matches!(
            entries[1].sender,
            lunarwing_engine::EntrySender::ImportedAgent
        ));
        assert_eq!(entries[0].metadata["imported_from"], "v1");
    }

    #[test]
    fn an_empty_history_yields_no_entries() {
        assert!(importable_entries(Vec::new(), Utc::now()).is_empty());
    }
}
