use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::writer::Outbound;

#[derive(Default)]
struct State {
    sessions: HashMap<String, Arc<Session>>,
    reservations: usize,
    delivered: HashSet<Uuid>,
}

#[derive(Default)]
pub(super) struct Registry {
    state: Mutex<State>,
}

pub(super) struct Session {
    pub(super) outbound: Outbound,
    pub(super) kill: CancellationToken,
}

pub(super) struct SlotGuard {
    registry: Arc<Registry>,
    session_id: String,
    opened: bool,
}

impl Registry {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn try_reserve(
        self: &Arc<Self>,
        max_connections: usize,
        session_id: String,
    ) -> Option<SlotGuard> {
        let mut state = self.state();
        if state.reservations >= max_connections {
            return None;
        }
        state.reservations += 1;
        drop(state);
        Some(SlotGuard {
            registry: Arc::clone(self),
            session_id,
            opened: false,
        })
    }

    pub(super) fn route(&self, session_id: &str) -> Option<Outbound> {
        self.state()
            .sessions
            .get(session_id)
            .map(|session| session.outbound.clone())
    }

    pub(super) fn mark_delivered(&self, message_id: Uuid) {
        self.state().delivered.insert(message_id);
    }

    pub(super) fn take_delivered(&self, message_id: Uuid) -> bool {
        self.state().delivered.remove(&message_id)
    }

    pub(super) fn remove(&self, session_id: &str) {
        let session = self.state().sessions.remove(session_id);
        if let Some(session) = session {
            session.kill.cancel();
            let mut state = self.state();
            state.reservations = state.reservations.saturating_sub(1);
        }
    }

    pub(super) fn close_all(&self) {
        let sessions = {
            let mut state = self.state();
            state.reservations = 0;
            state.delivered.clear();
            std::mem::take(&mut state.sessions)
        };
        for session in sessions.into_values() {
            session.kill.cancel();
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.state().sessions.len()
    }
}

impl SlotGuard {
    pub(super) fn open_session(
        mut self,
        outbound: Outbound,
        kill: CancellationToken,
    ) -> Arc<Session> {
        let session = Arc::new(Session { outbound, kill });
        self.opened = true;
        self.registry
            .state()
            .sessions
            .insert(self.session_id.clone(), Arc::clone(&session));
        session
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if !self.opened {
            let mut state = self.registry.state();
            state.reservations = state.reservations.saturating_sub(1);
        }
    }
}
