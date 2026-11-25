//! Structural classification of Wasmtime execution traps.
//!
//! Wasmtime attaches a [`wasmtime::WasmBacktrace`] as an anyhow *context* on
//! every trap, and anyhow's non-alternate `Display` prints only the outermost
//! context. Reading `e.to_string()` therefore yields
//! `error while executing at wasm backtrace: ...` and hides the actual trap
//! reason line (`wasm trap: <reason>`) — every trap in this codebase used to
//! report as a reason-less backtrace (WI#891).
//!
//! The helpers here read the root cause, prefer a structural
//! [`wasmtime::Trap`] downcast over string matching, and fold a recorded
//! memory-growth denial into a memory-limit explanation.

/// The underlying Wasmtime trap, when the error chain carries one.
///
/// `wasmtime::Error` is an `anyhow::Error` alias, and anyhow's downcast walks
/// the whole context chain, so this finds the trap even though the backtrace
/// context is what `Display` shows.
pub fn trap_kind(error: &anyhow::Error) -> Option<wasmtime::Trap> {
    error.downcast_ref::<wasmtime::Trap>().copied()
}

/// The innermost cause text — the line that actually names the failure.
///
/// Falls back to the full display when the chain has a single layer.
pub fn root_cause_text(error: &anyhow::Error) -> String {
    let root = error.root_cause().to_string();
    if root.is_empty() {
        error.to_string()
    } else {
        root
    }
}

/// Whether the error chain reports fuel exhaustion.
///
/// Matches Wasmtime 36's wording (`all fuel consumed by WebAssembly`) as well
/// as the older `out of fuel` phrasing. The trap enum is authoritative when
/// present, so the string match is only a fallback for wrapped errors.
pub fn is_fuel_exhausted(error: &anyhow::Error) -> bool {
    if trap_kind(error) == Some(wasmtime::Trap::OutOfFuel) {
        return true;
    }
    let text = format!("{error:#}");
    text.contains("all fuel consumed by WebAssembly") || text.contains("out of fuel")
}

/// A one-line, reason-bearing description of a trap for agent-facing errors.
///
/// `denied_growth` is the byte size of a refused `memory.grow`, when the
/// resource limiter recorded one. An allocation failure that traps as
/// `unreachable` is reported as a memory limit instead of an unexplained
/// abort, because the guest's allocator aborts once growth is refused.
pub fn describe_trap(error: &anyhow::Error, memory_limit: u64, denied_growth: Option<u64>) -> String {
    let root = root_cause_text(error);
    if let Some(desired) = denied_growth {
        return format!(
            "{root} (WASM memory limit reached: guest requested {desired} bytes, \
             limit is {memory_limit} bytes)"
        );
    }
    root
}

#[cfg(test)]
mod tests {
    use super::{describe_trap, is_fuel_exhausted, root_cause_text, trap_kind};

    /// A trap with a backtrace context must report the trap reason, not the
    /// backtrace header (WI#891: `e.to_string()` showed only the context).
    #[test]
    fn root_cause_text_skips_the_backtrace_context() {
        let error = anyhow::Error::from(wasmtime::Trap::MemoryOutOfBounds)
            .context("error while executing at wasm backtrace:\n    0: 0x1234");
        assert_eq!(
            error.to_string(),
            "error while executing at wasm backtrace:\n    0: 0x1234"
        );
        assert!(
            root_cause_text(&error).contains("out of bounds memory access"),
            "root cause must name the trap, got: {}",
            root_cause_text(&error)
        );
        assert!(!root_cause_text(&error).contains("wasm backtrace"));
    }

    /// The structural downcast must find the trap through the context chain.
    #[test]
    fn trap_kind_survives_context_layers() {
        let error = anyhow::Error::from(wasmtime::Trap::UnreachableCodeReached)
            .context("error while executing at wasm backtrace:");
        assert_eq!(trap_kind(&error), Some(wasmtime::Trap::UnreachableCodeReached));

        let plain = anyhow::anyhow!("GitLab HTTP request failed");
        assert_eq!(trap_kind(&plain), None);
    }

    /// Wasmtime 36 words fuel exhaustion `all fuel consumed by WebAssembly`;
    /// the old `out of fuel` sniff could never match a real trap.
    #[test]
    fn fuel_exhaustion_matches_current_and_legacy_wording() {
        let current = anyhow::Error::from(wasmtime::Trap::OutOfFuel)
            .context("error while executing at wasm backtrace:");
        assert!(is_fuel_exhausted(&current));

        let legacy = anyhow::anyhow!("wasm trap: out of fuel");
        assert!(is_fuel_exhausted(&legacy));

        let other = anyhow::Error::from(wasmtime::Trap::UnreachableCodeReached);
        assert!(!is_fuel_exhausted(&other));
    }

    /// A refused `memory.grow` that traps as `unreachable` must be reported as
    /// a memory limit, since the guest allocator aborts on the refusal.
    #[test]
    fn denied_growth_is_reported_as_a_memory_limit() {
        let error = anyhow::Error::from(wasmtime::Trap::UnreachableCodeReached)
            .context("error while executing at wasm backtrace:");
        let described = describe_trap(&error, 10 * 1024 * 1024, Some(12 * 1024 * 1024));
        assert!(described.contains("memory limit reached"), "{described}");
        assert!(described.contains("12582912"), "{described}");
        assert!(described.contains("10485760"), "{described}");
        assert!(!described.contains("wasm backtrace"), "{described}");
    }

    /// Without a recorded denial the trap reason stands on its own.
    #[test]
    fn undescribed_trap_keeps_its_reason() {
        let error = anyhow::Error::from(wasmtime::Trap::IntegerDivisionByZero);
        let described = describe_trap(&error, 10 * 1024 * 1024, None);
        assert!(described.contains("integer divide by zero"), "{described}");
        assert!(!described.contains("memory limit"), "{described}");
    }
}
