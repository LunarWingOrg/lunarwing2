//! Admission and lifetime tracking for non-cancellable native inference (WI#264).
//!
//! Native ONNX inference cannot be safely interrupted once a blocking task has
//! entered it, so a caller timeout or disconnect never tries to cancel it.
//! Instead each component admits at most one call before acquiring shared
//! capacity, and both permits move into the blocking worker. An abandoned
//! worker can therefore retain only its component mutex and one shared
//! permit: new work for that component fails fast instead of queueing behind
//! it, while other components keep using the remaining shared capacity.
//!
//! A panic while the component mutex is held, or a pre-poisoned model lock,
//! marks the component permanently failed instead of reporting transient
//! busy: native model state cannot be assumed valid after an unwind, so
//! recovery is a process restart that rebuilds the engine (WI#264 review P2).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::engine::EngineError;

/// Voice component sharing the sidecar's inference capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VoiceComponent {
    Stt,
    Tts,
    Vad,
}

impl VoiceComponent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Tts => "tts",
            Self::Vad => "vad",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Stt => 0,
            Self::Tts => 1,
            Self::Vad => 2,
        }
    }
}

const COMPONENT_COUNT: usize = 3;

/// Per-component inference state, for `/ready` and `/metrics`.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ComponentInferenceStatus {
    /// A call is currently admitted and holding the component's slot.
    pub active: bool,
    /// Abandoned calls whose native inference has not returned yet.
    pub abandoned_in_flight: usize,
    /// Total calls abandoned since process start.
    pub abandoned_total: u64,
    /// A panic or poisoned model lock left the component unusable; recovery
    /// requires a sidecar restart, not a busy retry.
    pub failed: bool,
}

/// Inference state for all voice components.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct InferenceStatus {
    pub stt: ComponentInferenceStatus,
    pub tts: ComponentInferenceStatus,
    pub vad: ComponentInferenceStatus,
}

impl InferenceStatus {
    /// True when any component is failed or still draining an abandoned
    /// call; `/ready` must stay 503 in both cases. Ordinary busy work alone
    /// does not degrade readiness.
    pub fn is_degraded(&self) -> bool {
        [self.stt, self.tts, self.vad]
            .iter()
            .any(|status| status.failed || status.abandoned_in_flight > 0)
    }
}

/// Abandonment and failure bookkeeping for one component.
#[derive(Debug, Default)]
struct ComponentSlots {
    /// Admission slot; one admitted call per component at a time.
    admission: Arc<Semaphore>,
    abandoned_total: AtomicU64,
    abandoned_in_flight: AtomicUsize,
    failed: AtomicBool,
}

impl ComponentSlots {
    fn status(&self) -> ComponentInferenceStatus {
        ComponentInferenceStatus {
            active: self.admission.available_permits() == 0,
            abandoned_in_flight: self.abandoned_in_flight.load(Ordering::Relaxed),
            abandoned_total: self.abandoned_total.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Acquire),
        }
    }

    fn check_failed(&self) -> Result<(), EngineError> {
        if self.failed.load(Ordering::Acquire) {
            Err(EngineError::State)
        } else {
            Ok(())
        }
    }

    fn mark_failed(&self) {
        self.failed.store(true, Ordering::Release);
    }
}

/// Shared inference runner used by every production component and by the
/// model-free controlled-inference tests (WI#264).
#[derive(Debug)]
pub struct InferenceRuntime {
    capacity: Arc<Semaphore>,
    components: [Arc<ComponentSlots>; COMPONENT_COUNT],
}

impl InferenceRuntime {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: Arc::new(Semaphore::new(capacity)),
            components: std::array::from_fn(|_| Arc::new(ComponentSlots::default())),
        }
    }

    pub fn status(&self) -> InferenceStatus {
        InferenceStatus {
            stt: self.components[0].status(),
            tts: self.components[1].status(),
            vad: self.components[2].status(),
        }
    }

    /// Runs one inference call under component admission and shared capacity.
    ///
    /// Admission (one slot per component) is acquired *before* shared
    /// capacity, so an abandoned call retains at most one shared permit and
    /// callers for the same component are rejected fast — even before the
    /// abandoned request's own timeout elapses — instead of queueing behind
    /// its model mutex.
    pub async fn run<M, T, F>(
        &self,
        component: VoiceComponent,
        model: Arc<Mutex<M>>,
        infer: F,
    ) -> Result<T, EngineError>
    where
        M: Send + 'static,
        T: Send + 'static,
        F: FnOnce(&mut M) -> Result<T, EngineError> + Send + 'static,
    {
        let name = component.as_str();
        let slots = Arc::clone(&self.components[component.index()]);
        slots.check_failed()?;
        let admission = Arc::clone(&slots.admission)
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        // A worker may have failed between the first check and admission.
        slots.check_failed()?;
        let capacity = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        let phase = Arc::new(Mutex::new(CallPhase::Active));
        let caller = CallerGuard {
            component: Arc::clone(&slots),
            phase: Arc::clone(&phase),
        };
        let worker = WorkerGuard {
            component: Arc::clone(&slots),
            name,
            phase,
            _capacity: capacity,
            _admission: admission,
        };
        let result = tokio::task::spawn_blocking(move || {
            // The model guard drops before the worker settles and releases
            // its permits, including during unwinding. A poisoned native
            // model is never recovered in place.
            let _worker = worker;
            let mut model = match model.try_lock() {
                Ok(model) => model,
                Err(TryLockError::WouldBlock) => return Err(EngineError::Busy),
                Err(TryLockError::Poisoned(_)) => {
                    slots.mark_failed();
                    tracing::error!(
                        component = name,
                        "voice model mutex is poisoned; sidecar restart required"
                    );
                    return Err(EngineError::State);
                }
            };
            infer(&mut model)
        })
        .await
        .map_err(|_| EngineError::State)?;
        drop(caller);
        result
    }
}

/// Phase of one tracked call, advanced exactly once per side.
///
/// Both transitions and their counter updates go through the per-call mutex
/// shared by the guards, so a phase change and its accounting are one atomic
/// step: the worker can never observe `CallerLeft` and decrement the
/// in-flight counter before the caller's increment is visible. A
/// settled-flag swap alone cannot guarantee that ordering — publishing the
/// flag and incrementing the counter separately let a descheduled caller
/// cause the worker to decrement a still-zero counter and wrap it to
/// `usize::MAX` (MR !1255 review finding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CallPhase {
    #[default]
    Active,
    CallerLeft,
    WorkerReturned,
}

/// Locks a bookkeeping mutex, recovering from poisoning.
///
/// Only advisory accounting locks are recovered here; a poisoned *model*
/// mutex marks the component failed instead (see [`InferenceRuntime::run`]).
fn lock_phase(phase: &Mutex<CallPhase>) -> MutexGuard<'_, CallPhase> {
    phase.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Half of the abandonment detector owned by the caller's async future.
///
/// When the caller's future is dropped while the call is still
/// [`CallPhase::Active`] (worker still running), the drop records the call
/// as abandoned. On normal completion the future's drop happens after the
/// join already observed `WorkerReturned`, so nothing is recorded.
struct CallerGuard {
    component: Arc<ComponentSlots>,
    phase: Arc<Mutex<CallPhase>>,
}

impl Drop for CallerGuard {
    fn drop(&mut self) {
        let mut phase = lock_phase(&self.phase);
        if *phase == CallPhase::Active {
            self.component
                .abandoned_total
                .fetch_add(1, Ordering::Relaxed);
            self.component
                .abandoned_in_flight
                .fetch_add(1, Ordering::Relaxed);
            *phase = CallPhase::CallerLeft;
        }
    }
}

/// Half of the abandonment detector owned by the blocking closure.
///
/// Holds both the component admission slot and the shared capacity permit,
/// so they are released exactly when the blocking closure settles — a caller
/// timeout cannot release a running worker's permits. Because settlement
/// happens in `Drop`, a panic inside the closure still settles the
/// accounting, but it also marks the component permanently failed: the
/// unwind may have left native state invalid (WI#264 review P2).
struct WorkerGuard {
    component: Arc<ComponentSlots>,
    name: &'static str,
    phase: Arc<Mutex<CallPhase>>,
    _capacity: OwnedSemaphorePermit,
    _admission: OwnedSemaphorePermit,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                component = self.name,
                "voice inference panicked; component requires a sidecar restart"
            );
            self.component.mark_failed();
        }
        let mut phase = lock_phase(&self.phase);
        let previous = *phase;
        *phase = CallPhase::WorkerReturned;
        if previous == CallPhase::CallerLeft {
            self.component
                .abandoned_in_flight
                .fetch_sub(1, Ordering::Relaxed);
        }
    }
}
