//! Serialized Engine V2 test environment guard.
//!
//! Provides a shared RAII guard that enables Engine V2 for the current
//! process and restores the original env_drop. The accompanying
//! `#[cfg(test)]` module verifies save/restore round-trip behavior via
//! a process-local mutex so concurrent test threads cannot alias the
//! environment.

use std::ffi::OsString;

use lunarwing::bridge::reset_engine_state;

/// Restores `ENGINE_V2` and `ENGINE_V2_CHANNELS` on drop. The support
/// module intentionally keeps serialization (and the async reset used
/// by callers) in the dedicated integration binaries; this guard only
/// manages environment state.
pub struct EngineV2EnvGuard {
    engine_v2: Option<OsString>,
    channels: Option<OsString>,
}

impl EngineV2EnvGuard {
    /// Enable Engine V2 with optional channel scoping, saving the
    /// current env values for later restoration.
    ///
    /// # Safety
    ///
    /// Process environment mutation is `unsafe` under Rust 2024. Each
    /// consuming integration binary is serialized — exactly one test
    /// runs at a time within a binary — and the guard restores both
    /// variables before the agent stops, so no concurrent reader
    /// observes a half-initialized state.
    pub fn enable(channels: Option<&str>) -> Self {
        let prev = Self {
            engine_v2: std::env::var_os("ENGINE_V2"),
            channels: std::env::var_os("ENGINE_V2_CHANNELS"),
        };

        // SAFETY: [Category 13 — Library/unsafe contract]
        // Each consuming integration binary is serialized (exactly one
        // test runs at a time), and no other code reads ENGINE_V2 or
        // ENGINE_V2_CHANNELS concurrently during this call. The
        // constructor's saved sentinels are restored in `Drop` before
        // the test completes, so no observer can witness a
        // half-initialized state.
        unsafe {
            std::env::set_var("ENGINE_V2", "true");
            match channels {
                Some(value) => std::env::set_var("ENGINE_V2_CHANNELS", value),
                None => std::env::remove_var("ENGINE_V2_CHANNELS"),
            }
        }

        prev
    }

    /// Update the active `ENGINE_V2_CHANNELS` value in place without
    /// touching `ENGINE_V2`.
    pub fn set_channels(&self, channels: Option<&str>) {
        // SAFETY: [Category 13 — Library/unsafe contract]
        // Same serialization invariant as `enable`: the single test
        // thread own the environment, and this mutation is
        // instantaneous.
        unsafe {
            match channels {
                Some(value) => std::env::set_var("ENGINE_V2_CHANNELS", value),
                None => std::env::remove_var("ENGINE_V2_CHANNELS"),
            }
        }
    }

    /// Perform the async reset that callers require, then release the
    /// guard so `Drop` restores env state. Splitting the reset from
    /// `Drop` avoids blocking the runtime inside a destructor.
    pub async fn cleanup(self) {
        reset_engine_state().await;
        drop(self);
    }
}

impl Drop for EngineV2EnvGuard {
    fn drop(&mut self) {
        // SAFETY: [Category 13 — Library/unsafe contract]
        // The dedicated test has stopped its background agent before
        // this guard is dropped, so no concurrent environment reader
        // remains. Restoring sentinels matches the enable contract.
        unsafe {
            restore_env("ENGINE_V2", self.engine_v2.take());
            restore_env("ENGINE_V2_CHANNELS", self.channels.take());
        }
    }
}

unsafe fn restore_env(key: &str, value: Option<OsString>) {
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::EngineV2EnvGuard;

    /// Process-local serialization so environment mutation cannot
    /// overlap across test threads within a single binary.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn save_and_restore_roundtrip() {
        let _guard = ENV_LOCK.lock().expect("mutex poisoned");

        let saved_engine_v2 = std::env::var_os("ENGINE_V2");
        let saved_channels = std::env::var_os("ENGINE_V2_CHANNELS");

        let env = EngineV2EnvGuard::enable(Some("xmpp"));
        assert_eq!(std::env::var("ENGINE_V2").unwrap(), "true");
        assert_eq!(std::env::var("ENGINE_V2_CHANNELS").unwrap(), "xmpp");

        env.cleanup().await;

        assert_eq!(std::env::var_os("ENGINE_V2"), saved_engine_v2);
        assert_eq!(std::env::var_os("ENGINE_V2_CHANNELS"), saved_channels);
    }
}
