//! Bounded SillyTavern-style lorebook import and prompt activation.
//!
//! Lorebooks live in primary workspace scope under `lorebooks/*.json`. This
//! implementation intentionally supports a documented SillyTavern import
//! subset rather than claiming complete World Info parity.

pub mod model;

pub use model::{LorebookEntry, LorebookError, SelectiveLogic, parse_lorebook};
