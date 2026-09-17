//! Bounded per-call argument assembly for REPL integrity inspection. Reasoning
//! and assistant text never enter these buffers; only ToolCallDelta does.

use std::collections::BTreeMap;

use lunarwing_engine::types::repl::{
    ReplArgumentCorruption, is_repl_action, validate_repl_arguments,
};

const MAX_ARGUMENT_BYTES: usize = 1_048_576;
const MAX_CALLS: usize = 128;

#[derive(Default)]
pub(super) struct ReplArgumentGuard {
    calls: BTreeMap<usize, CallArguments>,
    bytes: usize,
    overflow: bool,
    has_repl: bool,
}

#[derive(Default)]
struct CallArguments {
    is_repl: bool,
    arguments: String,
}

impl ReplArgumentGuard {
    pub(super) fn push(&mut self, index: usize, name: Option<&str>, delta: &str) {
        self.has_repl |= name.is_some_and(is_repl_action);
        if self.calls.len() >= MAX_CALLS && !self.calls.contains_key(&index) {
            self.overflow = true;
            return;
        }
        let call = self.calls.entry(index).or_default();
        call.is_repl |= name.is_some_and(is_repl_action);
        self.bytes = self.bytes.saturating_add(delta.len());
        if self.bytes > MAX_ARGUMENT_BYTES {
            self.overflow = true;
        } else {
            call.arguments.push_str(delta);
        }
    }

    pub(super) fn validate(&self) -> Result<(), ReplArgumentCorruption> {
        if self.has_repl && self.overflow {
            return Err(ReplArgumentCorruption::InspectionLimit);
        }
        for call in self.calls.values().filter(|call| call.is_repl) {
            let arguments = serde_json::from_str(&call.arguments)
                .map_err(|_| ReplArgumentCorruption::InvalidJson)?;
            validate_repl_arguments(&arguments)?;
        }
        Ok(())
    }

    pub(super) fn has_calls(&self) -> bool {
        !self.calls.is_empty()
    }
}
