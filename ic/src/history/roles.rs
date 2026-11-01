//! Durable conversation-role census (WI#518).
//!
//! `conversation_messages.role` is a free-text column shared by several
//! writers: the classic agent loop, the Engine V2 bridge dual-write, and the
//! turn-recovery train. Each reader used to carry its own copy of "which roles
//! are conversation", so a new bookkeeping role had to be remembered at every
//! site or it leaked into a user-visible surface. This module is the one place
//! that names and classifies those roles.
//!
//! Engine `EventKind::MessageAdded` events carry the separate wire namespace
//! (`MessageRole::wire_name()`), classified by [`classify_engine_role`].
//! Durable rows are always written lowercase, so the two namespaces never
//! collide.

/// Human-authored conversation text.
pub const USER_ROLE: &str = "user";
/// Assistant-authored conversation text.
pub const ASSISTANT_ROLE: &str = "assistant";
/// Tool-call transcript captured as JSON.
pub const TOOL_CALLS_ROLE: &str = "tool_calls";
/// Marker row written when a turn is stopped by `/interrupt`.
///
/// The classic loop (`agent/thread_ops.rs`) and the Engine V2 bridge
/// (`bridge/router.rs`) append one row with this role after the interrupted
/// turn's user message. Without it, an interrupted turn has its user message
/// persisted but no assistant response, and is rendered as `Failed` —
/// operator-misleading, since no error occurred.
pub const INTERRUPTED_ROLE: &str = "interrupted";
/// Marker row written by the turn-recovery train (WI#478).
pub const TURN_RECOVERY_ROLE: &str = "turn_recovery";

/// Roles that carry conversation text and are replayed into LLM context.
pub const CONVERSATION_ROLES: [&str; 2] = [USER_ROLE, ASSISTANT_ROLE];

/// Roles whose rows are reconstructed into tool messages by the v1 hydration
/// path; their content is JSON, never conversation text.
pub const TOOL_CALL_ROLES: [&str; 1] = [TOOL_CALLS_ROLE];

/// Display-only marker roles: they drive UI state and are never replayed.
pub const MARKER_ROLES: [&str; 2] = [INTERRUPTED_ROLE, TURN_RECOVERY_ROLE];

/// Every machine-written row: tool transcripts plus display markers.
pub const BOOKKEEPING_ROLES: [&str; 3] = [TOOL_CALLS_ROLE, INTERRUPTED_ROLE, TURN_RECOVERY_ROLE];

/// Every durable role currently written to `conversation_messages`.
///
/// Add new roles here *and* to [`classify_conversation_role`]; the census
/// tests fail until both agree.
pub const ALL_ROLES: [&str; 5] = [
    USER_ROLE,
    ASSISTANT_ROLE,
    TOOL_CALLS_ROLE,
    INTERRUPTED_ROLE,
    TURN_RECOVERY_ROLE,
];

/// Classification of a durable `conversation_messages.role` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRole {
    /// Human-authored text.
    User,
    /// Assistant-authored text.
    Assistant,
    /// Tool-call transcript reconstructed into tool messages.
    ToolCalls,
    /// Display-only marker; never replayed and never used as content.
    Marker,
    /// Not in the census: an older writer's role, or one added without
    /// registering it. Readers treat it exactly like a marker.
    Unknown,
}

/// Classify a durable `conversation_messages.role` value.
///
/// Matching is byte-exact: rows are written by this daemon in lowercase, and a
/// case-folding match would silently start replaying legacy rows that the
/// existing filters skip.
pub fn classify_conversation_role(role: &str) -> ConversationRole {
    match role {
        USER_ROLE => ConversationRole::User,
        ASSISTANT_ROLE => ConversationRole::Assistant,
        TOOL_CALLS_ROLE => ConversationRole::ToolCalls,
        INTERRUPTED_ROLE | TURN_RECOVERY_ROLE => ConversationRole::Marker,
        _ => ConversationRole::Unknown,
    }
}

/// Whether `role` is a machine-written row rather than conversation text.
pub fn is_bookkeeping_role(role: &str) -> bool {
    BOOKKEEPING_ROLES.contains(&role)
}

/// Wire role on an engine `EventKind::MessageAdded`, derived from
/// `MessageRole::wire_name()` so the census cannot drift from the engine's own
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineRole {
    /// A human turn, or a legacy (pre-WI#227) interpreter step output.
    User,
    /// Assistant text.
    Assistant,
    /// Sealed interpreter step record (WI#227).
    StepRecord,
    /// `System`, `ActionResult`, or a spelling this census does not know.
    Other,
}

const WIRE_USER: &str = lunarwing_engine::MessageRole::User.wire_name();
const WIRE_ASSISTANT: &str = lunarwing_engine::MessageRole::Assistant.wire_name();
const WIRE_STEP_RECORD: &str = lunarwing_engine::MessageRole::SealedStepRecord.wire_name();

/// Classify an engine `EventKind::MessageAdded` wire role.
pub fn classify_engine_role(role: &str) -> EngineRole {
    match role {
        WIRE_USER => EngineRole::User,
        WIRE_ASSISTANT => EngineRole::Assistant,
        WIRE_STEP_RECORD => EngineRole::StepRecord,
        _ => EngineRole::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_census_role_classifies_without_falling_through() {
        for role in ALL_ROLES {
            assert!(
                !matches!(classify_conversation_role(role), ConversationRole::Unknown),
                "role {role:?} is in the census but classify_conversation_role does not know it"
            );
        }
        assert_eq!(
            ALL_ROLES.len(),
            CONVERSATION_ROLES.len() + BOOKKEEPING_ROLES.len()
        );
    }

    #[test]
    fn conversation_and_bookkeeping_partition_the_census() {
        for role in ALL_ROLES {
            let conversation = CONVERSATION_ROLES.contains(&role);
            let bookkeeping = BOOKKEEPING_ROLES.contains(&role);
            assert!(
                conversation ^ bookkeeping,
                "role {role:?} must be exactly one of conversation or bookkeeping"
            );
            assert_eq!(is_bookkeeping_role(role), bookkeeping);
        }
    }

    #[test]
    fn tool_calls_and_markers_partition_bookkeeping_roles() {
        for role in BOOKKEEPING_ROLES {
            let tool_call = TOOL_CALL_ROLES.contains(&role);
            let marker = MARKER_ROLES.contains(&role);
            assert!(
                tool_call ^ marker,
                "bookkeeping role {role:?} must be exactly one of tool transcript or marker"
            );
        }
    }

    #[test]
    fn conversation_roles_classify_to_their_own_variant() {
        assert_eq!(
            classify_conversation_role(USER_ROLE),
            ConversationRole::User
        );
        assert_eq!(
            classify_conversation_role(ASSISTANT_ROLE),
            ConversationRole::Assistant
        );
        assert_eq!(
            classify_conversation_role(TOOL_CALLS_ROLE),
            ConversationRole::ToolCalls
        );
        for role in MARKER_ROLES {
            assert_eq!(classify_conversation_role(role), ConversationRole::Marker);
        }
    }

    #[test]
    fn unknown_roles_are_not_conversation() {
        for role in [
            "",
            "system",
            "User",
            "assistant ",
            "tool_call",
            "future_role",
        ] {
            assert_eq!(
                classify_conversation_role(role),
                ConversationRole::Unknown,
                "role {role:?} must stay out of the conversation census"
            );
            assert!(!is_bookkeeping_role(role));
        }
    }

    #[test]
    fn engine_wire_roles_match_the_engine_vocabulary() {
        assert_eq!(classify_engine_role("User"), EngineRole::User);
        assert_eq!(classify_engine_role("Assistant"), EngineRole::Assistant);
        assert_eq!(classify_engine_role("StepRecord"), EngineRole::StepRecord);
        for role in ["System", "ActionResult", "TurnRecovery", "", "user"] {
            assert_eq!(
                classify_engine_role(role),
                EngineRole::Other,
                "wire role {role:?} must not be surfaced as a step record"
            );
        }
    }
}
