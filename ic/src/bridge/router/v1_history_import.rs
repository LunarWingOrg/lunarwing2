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
//! Three properties keep the import safe:
//!
//! - **Ownership gate** — the v1 conversation must belong to the message's
//!   user before anything is read from or written to it. A caller-supplied
//!   `thread_id` is not proof of ownership, so a forged one cannot copy
//!   another user's history into the caller's engine context. This mirrors the
//!   check the v1 hydrate path enforces (`agent/thread_ops.rs`).
//! - **New-surface gate** — the import only runs when this message created the
//!   engine surface. A surface that already existed belongs to a tenant that
//!   was on Engine V2 before this landed, and prepending weeks-old v1 rows to
//!   it would add context the user never saw in that thread and undo `/clear`
//!   (which empties entries but keeps `created_at`).
//! - **Creation-time cutoff** — only rows older than the surface's
//!   `created_at` are read, so the forward dual-write's own v2 rows can never
//!   be re-imported, even if the marker write fails and a later message
//!   retries.
//!
//! The cutoff compares two clocks: Postgres takes a row's `created_at` from the
//! database's `NOW()` default while the surface's comes from the daemon
//! (libSQL uses the daemon clock for both). Skew between them shifts the
//! boundary by the skew amount; the marker keeps that from ever re-importing
//! the same rows, and the new-surface gate means the boundary normally sits at
//! "now" with only genuinely pre-switch rows on the far side.

use tracing::{debug, warn};

use lunarwing_engine::{ConversationEntry, ConversationId};
use uuid::Uuid;

use super::EngineState;

/// Marker key in the v1 conversation metadata once history has been imported.
pub(crate) const IMPORTED_MARKER: &str = "engine_v2_v1_history_imported";

/// Import pre-switch v1 history into the engine conversation if it has not
/// been imported yet.
///
/// `surface_created` reports whether this message created the engine surface;
/// the import only runs for a newly created one (see the module docs).
///
/// Best-effort: any database or store failure is logged and the message path
/// continues, because a missing import degrades context but must never block
/// the user's turn. Returns the number of entries added when the import runs.
pub(crate) async fn import_v1_history_if_needed(
    state: &EngineState,
    conversation_id: ConversationId,
    v1_conversation_id: Uuid,
    user_id: &str,
    surface_created: bool,
) -> usize {
    let Some(db) = state.db.as_ref() else {
        return 0;
    };

    // Ownership first: nothing about another user's conversation — not even
    // the import marker — may be read or written on a forged scope.
    match db
        .conversation_belongs_to_user(v1_conversation_id, user_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            debug!(
                conversation_id = %v1_conversation_id,
                user_id,
                "engine v2: skipping v1 history import for a conversation the sender does not own"
            );
            return 0;
        }
        Err(error) => {
            debug!(%error, conversation_id = %v1_conversation_id, "engine v2: v1 history ownership check failed; skipping import");
            return 0;
        }
    }

    match db.get_conversation_metadata(v1_conversation_id).await {
        Ok(Some(metadata)) if metadata.get(IMPORTED_MARKER).is_some() => return 0,
        Ok(_) => {}
        Err(error) => {
            debug!(%error, conversation_id = %v1_conversation_id, "engine v2: v1 history marker lookup failed; skipping import");
            return 0;
        }
    }

    // A surface this message did not create belongs to a tenant already on
    // Engine V2. Record the marker so the check above short-circuits every
    // later turn, but do not backfill: that history was never part of this
    // thread's context.
    if !surface_created {
        record_marker(db.as_ref(), v1_conversation_id).await;
        debug!(
            conversation_id = %conversation_id,
            "engine v2: engine surface already existed; recorded v1 history marker without importing"
        );
        return 0;
    }

    // The surface's creation time is the cutoff: every v1 row at or after it
    // is a forward dual-write from a v2 turn, not pre-switch history.
    let Some(surface) = state
        .conversation_manager
        .get_conversation(conversation_id)
        .await
    else {
        return 0;
    };
    let cutoff = surface.created_at;

    // Bounded tail read: only the newest pre-cutoff rows can survive the
    // surface's own retention bounds, so ask the database for at most that
    // many instead of loading the whole v1 conversation first.
    let limit = lunarwing_engine::types::conversation::MAX_CONVERSATION_ENTRIES as i64;
    let rows = match db
        .list_conversation_messages_paginated(v1_conversation_id, Some(cutoff), limit)
        .await
    {
        Ok((rows, _has_more)) => rows,
        Err(error) => {
            debug!(%error, conversation_id = %v1_conversation_id, "engine v2: v1 history read failed; skipping import");
            return 0;
        }
    };

    let entries = importable_entries(rows);

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
                // Leave the marker unset so a later turn can retry; the cutoff
                // keeps a retry duplicate-free.
                warn!(%error, conversation_id = %conversation_id, "engine v2: v1 history import failed");
                return 0;
            }
        }
    };

    // Mark after a successful import (or when there was nothing to import) so
    // re-runs skip the read. A failed marker write only costs one extra scan
    // next turn: the cutoff keeps it duplicate-free.
    record_marker(db.as_ref(), v1_conversation_id).await;

    debug!(
        conversation_id = %conversation_id,
        imported,
        "engine v2: imported pre-switch v1 history"
    );
    imported
}

/// Best-effort write of the import marker onto the v1 conversation.
async fn record_marker(db: &dyn crate::db::Database, v1_conversation_id: Uuid) {
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
}

/// Map v1 rows to importable conversation entries.
///
/// Only `user` and `assistant` rows are conversation context. `tool_calls`,
/// `interrupted`, and any other v1 bookkeeping role are skipped: the engine
/// does not replay them, and importing them would put non-conversational text
/// into the seeded history. The caller has already bounded the rows to those
/// created before the surface cutoff.
fn importable_entries(rows: Vec<crate::history::ConversationMessage>) -> Vec<ConversationEntry> {
    rows.into_iter()
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
    fn only_user_and_assistant_rows_are_imported() {
        let now = Utc::now();
        let rows = vec![
            row("user", "old question", now - Duration::minutes(10)),
            row("assistant", "old answer", now - Duration::minutes(9)),
            row("tool_calls", "[]", now - Duration::minutes(8)),
            row("interrupted", "", now - Duration::minutes(7)),
        ];

        let entries = importable_entries(rows);

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
        // The v1 timestamps survive so seeding order matches the original
        // conversation rather than the import time.
        assert_eq!(entries[0].timestamp, now - Duration::minutes(10));
    }

    #[test]
    fn an_empty_history_yields_no_entries() {
        assert!(importable_entries(Vec::new()).is_empty());
    }

    #[test]
    fn unknown_roles_are_never_imported() {
        let now = Utc::now();
        let rows = vec![
            row("system", "notice", now),
            row("tool_result", "{}", now),
            row("", "", now),
        ];
        assert!(importable_entries(rows).is_empty());
    }
}
