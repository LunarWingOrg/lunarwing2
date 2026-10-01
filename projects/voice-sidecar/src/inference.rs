//! Admission and lifetime tracking for non-cancellable native inference (WI#264).
//!
//! One call per component is admitted BEFORE acquiring shared capacity. A caller
//! timeout cannot release a running worker's permits or enqueue more work behind
//! its model lock. Other components can use the remaining shared capacity.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::engine::EngineError;

#[derive(Clone, Copy, Debug)]
pub enum VoiceComponent {
    Stt,
    Tts,
    Vad,
}

impl VoiceComponent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Tts => "tts",
            Self::Vad => "vad",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Stt => 0,
            Self::Tts => 1,
            Self::Vad => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ComponentInferenceStatus {
    pub active: bool,
    pub abandoned_in_flight: usize,
    pub abandoned_total: u64,
    /// A panic/poisoned model requires a process restart, not a busy retry.
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct InferenceStatus {
    pub stt: ComponentInferenceStatus,
    pub tts: ComponentInferenceStatus,
    pub vad: ComponentInferenceStatus,
}

impl InferenceStatus {
    pub fn is_degraded(&self) -> bool {
        [self.stt, self.tts, self.vad]
            .iter()
            .any(|status| status.failed || status.abandoned_in_flight > 0)
    }
}

struct Component {
    admission: Arc<Semaphore>,
    abandoned_in_flight: AtomicUsize,
    abandoned_total: AtomicU64,
    failed: AtomicBool,
}

impl Default for Component {
    fn default() -> Self {
        Self {
            admission: Arc::new(Semaphore::new(1)),
            abandoned_in_flight: AtomicUsize::new(0),
            abandoned_total: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        }
    }
}

impl Component {
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
}

pub struct InferenceRuntime {
    capacity: Arc<Semaphore>,
    components: [Arc<Component>; 3],
}

impl InferenceRuntime {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: Arc::new(Semaphore::new(capacity)),
            components: std::array::from_fn(|_| Arc::new(Component::default())),
        }
    }

    pub fn status(&self) -> InferenceStatus {
        InferenceStatus {
            stt: self.components[0].status(),
            tts: self.components[1].status(),
            vad: self.components[2].status(),
        }
    }

    /// The same admission, model locking, and settlement path is used by all
    /// production components and by model-free controlled-inference tests.
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
        let component = Arc::clone(&self.components[component.index()]);
        component.check_failed()?;
        let admission = Arc::clone(&component.admission)
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        // A previous worker may have failed between the first check and admission.
        component.check_failed()?;
        let capacity = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| EngineError::Busy)?;
        let phase = Arc::new(Mutex::new(CallPhase::Active));
        let caller = CallerGuard {
            component: Arc::clone(&component),
            phase: Arc::clone(&phase),
            name,
        };
        let worker = WorkerGuard {
            component: Arc::clone(&component),
            phase,
            name,
            _capacity: capacity,
            _admission: admission,
        };
        let result = tokio::task::spawn_blocking(move || {
            // Model guard drops before the worker settles and releases permits,
            // including during unwinding. Never recover a poisoned native model.
            let _worker = worker;
            let mut model = match model.try_lock() {
                Ok(model) => model,
                Err(TryLockError::WouldBlock) => return Err(EngineError::Busy),
                Err(TryLockError::Poisoned(_)) => {
                    component.failed.store(true, Ordering::Release);
                    tracing::error!(
                        component = name,
                        "voice model is poisoned; sidecar restart required"
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

#[derive(PartialEq)]
enum CallPhase {
    Active,
    CallerLeft,
    WorkerReturned,
}

// Only bookkeeping locks are recoverable; native model locks are not. The
// per-call lock covers BOTH transitions and counters, preserving !1255's fix:
// worker settlement must never decrement before caller abandonment increments.
fn lock_phase(phase: &Mutex<CallPhase>) -> MutexGuard<'_, CallPhase> {
    phase.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct CallerGuard {
    component: Arc<Component>,
    phase: Arc<Mutex<CallPhase>>,
    name: &'static str,
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
            tracing::warn!(
                component = self.name,
                "voice caller left while native inference is still draining"
            );
        }
    }
}

struct WorkerGuard {
    component: Arc<Component>,
    phase: Arc<Mutex<CallPhase>>,
    name: &'static str,
    _capacity: OwnedSemaphorePermit,
    _admission: OwnedSemaphorePermit,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.component.failed.store(true, Ordering::Release);
            tracing::error!(
                component = self.name,
                "voice inference panicked; component requires a sidecar restart"
            );
        }
        let mut phase = lock_phase(&self.phase);
        if *phase == CallPhase::CallerLeft {
            self.component
                .abandoned_in_flight
                .fetch_sub(1, Ordering::Relaxed);
        }
        *phase = CallPhase::WorkerReturned;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;

    #[test]
    fn concurrent_settlement_never_underflows_or_leaks_permits() {
        let runtime = InferenceRuntime::new(2);
        let component = &runtime.components[VoiceComponent::Tts.index()];
        for _ in 0..64 {
            let phase = Arc::new(Mutex::new(CallPhase::Active));
            let caller = CallerGuard {
                component: Arc::clone(component),
                phase: Arc::clone(&phase),
                name: "tts",
            };
            let worker = WorkerGuard {
                component: Arc::clone(component),
                phase,
                name: "tts",
                _capacity: Arc::clone(&runtime.capacity).try_acquire_owned().unwrap(),
                _admission: Arc::clone(&component.admission)
                    .try_acquire_owned()
                    .unwrap(),
            };
            let barrier = Arc::new(Barrier::new(2));
            let caller_barrier = Arc::clone(&barrier);
            let caller = std::thread::spawn(move || {
                caller_barrier.wait();
                drop(caller);
            });
            let worker = std::thread::spawn(move || {
                barrier.wait();
                drop(worker);
            });
            // A wrapped decrement can be hidden by a later increment returning
            // the final value to zero. Observe during settlement as well.
            while !caller.is_finished() || !worker.is_finished() {
                assert!(runtime.status().tts.abandoned_in_flight <= 1);
                std::thread::yield_now();
            }
            caller.join().unwrap();
            worker.join().unwrap();
            assert_eq!(runtime.status().tts.abandoned_in_flight, 0);
            assert!(!runtime.status().tts.active);
            assert_eq!(runtime.capacity.available_permits(), 2);
        }
        assert!(runtime.status().tts.abandoned_total <= 64);
    }

    #[tokio::test]
    async fn contention_is_temporary_but_poison_requires_restart() {
        let runtime = InferenceRuntime::new(2);
        let model = Arc::new(Mutex::new(()));
        let held_model = Arc::clone(&model);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = held_model.lock().unwrap();
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started_rx.await.unwrap();
        assert!(matches!(
            runtime
                .run(VoiceComponent::Tts, Arc::clone(&model), |_| Ok(()))
                .await,
            Err(EngineError::Busy)
        ));
        assert!(!runtime.status().is_degraded());
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        runtime
            .run(VoiceComponent::Tts, Arc::clone(&model), |_| Ok(()))
            .await
            .unwrap();

        let poisoned_model = Arc::clone(&model);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned_model.lock().unwrap();
                panic!("controlled model-lock poison");
            })
            .join()
            .is_err()
        );
        for _ in 0..3 {
            assert!(matches!(
                runtime
                    .run::<_, (), _>(VoiceComponent::Tts, Arc::clone(&model), |_| panic!(
                        "poisoned model reused"
                    ))
                    .await,
                Err(EngineError::State)
            ));
        }
        assert!(runtime.status().tts.failed);
        assert!(runtime.status().is_degraded());
        assert_eq!(runtime.status().tts.abandoned_in_flight, 0);
        assert_eq!(runtime.capacity.available_permits(), 2);
    }
}
