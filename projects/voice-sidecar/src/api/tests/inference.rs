//! API-level tests for inference abandonment, fencing, and failure state
//! (WI#264), using the production `InferenceRuntime` with a controlled
//! blocking model instead of a live ONNX model.

use std::sync::{Mutex, mpsc};
use std::time::Duration;

use super::*;
use crate::inference::{InferenceRuntime, InferenceStatus, VoiceComponent};

enum Completion {
    Success,
    Error,
    Panic,
}

struct ControlledModel {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    completion: mpsc::Receiver<Completion>,
}

struct ControlledEngine {
    runtime: InferenceRuntime,
    tts: Arc<Mutex<ControlledModel>>,
    stt: Arc<Mutex<()>>,
    vad: Arc<Mutex<()>>,
}

#[async_trait]
impl VoiceEngine for ControlledEngine {
    async fn transcribe(
        &self,
        _audio: AudioData,
        _language: &str,
        _model: &str,
    ) -> Result<Transcription, EngineError> {
        self.runtime
            .run(VoiceComponent::Stt, Arc::clone(&self.stt), |_| {
                Ok(Transcription {
                    text: "controlled".into(),
                    language: "en".into(),
                    confidence: None,
                    segments: vec![],
                })
            })
            .await
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice: &str,
        _speed: f32,
    ) -> Result<SynthesizedAudio, EngineError> {
        // Production runner: real component admission, model mutex, blocking
        // closure, shared capacity, cancellation, and settlement semantics.
        self.runtime
            .run(VoiceComponent::Tts, Arc::clone(&self.tts), |model| {
                let _ = model.started.send(());
                // Always bounded, even if a test fails before releasing.
                match model.completion.recv_timeout(Duration::from_secs(5)) {
                    Ok(Completion::Success) => Ok(SynthesizedAudio {
                        samples: vec![0.0; 160],
                        sample_rate: 16_000,
                    }),
                    Ok(Completion::Panic) => panic!("controlled panic with model lock held"),
                    _ => Err(EngineError::Inference),
                }
            })
            .await
    }

    async fn detect(&self, _audio: AudioData) -> Result<Vec<SpeechSegment>, EngineError> {
        self.runtime
            .run(VoiceComponent::Vad, Arc::clone(&self.vad), |_| Ok(vec![]))
            .await
    }

    fn inference_status(&self) -> InferenceStatus {
        self.runtime.status()
    }

    fn readiness(&self) -> EngineReadiness {
        MockEngine.readiness()
    }
}

struct Fixture {
    state: AppState,
    engine: Arc<ControlledEngine>,
    started: tokio::sync::mpsc::UnboundedReceiver<()>,
    completion: mpsc::Sender<Completion>,
}

impl Fixture {
    fn new(capacity: usize) -> Self {
        let (started_tx, started) = tokio::sync::mpsc::unbounded_channel();
        let (completion, completion_rx) = mpsc::channel();
        let engine = Arc::new(ControlledEngine {
            runtime: InferenceRuntime::new(capacity),
            tts: Arc::new(Mutex::new(ControlledModel {
                started: started_tx,
                completion: completion_rx,
            })),
            stt: Arc::new(Mutex::new(())),
            vad: Arc::new(Mutex::new(())),
        });
        let mut config = test_config();
        config.request_timeout_secs = 1;
        // Avoid cached STT responses bypassing the shared-permit check.
        config.cache_ttl_secs = 0;
        Self {
            state: AppState::new(Arc::new(config), engine.clone()),
            engine,
            started,
            completion,
        }
    }

    fn start_request(&self) -> tokio::task::JoinHandle<Response> {
        let state = self.state.clone();
        tokio::spawn(async move { synthesis_request(&state).await })
    }

    async fn wait_started(&mut self) {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), self.started.recv())
                .await
                .unwrap(),
            Some(())
        );
    }

    async fn wait_settled(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.engine.inference_status().tts.active {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("controlled inference did not settle");
    }

    async fn other_components(&self, expected: StatusCode) {
        for path in ["/v1/voice/transcribe", "/v1/voice/detect"] {
            let body = serde_json::json!({
                "audio": base64::engine::general_purpose::STANDARD.encode(encode_wav(&[0.0; 160], 16_000)),
                "format": "wav",
            });
            let response = api_router(self.state.clone())
                .oneshot(authorized_json_request(path, body.to_string()))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}");
        }
    }
}

async fn synthesis_request(state: &AppState) -> Response {
    api_router(state.clone())
        .oneshot(authorized_json_request(
            "/v1/voice/synthesize",
            serde_json::json!({"text": "controlled synthesis"}).to_string(),
        ))
        .await
        .unwrap()
}

async fn probe(state: &AppState, path: &str) -> Response {
    health_router(state.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn timeout_fences_repeated_tts_preserves_shared_capacity_and_recovers() {
    let mut fixture = Fixture::new(2);
    for cycle in 1..=3 {
        let request = fixture.start_request();
        fixture.wait_started().await;
        // A second TTS call cannot consume the remaining permit, even before
        // the first request's timeout elapses.
        assert_eq!(
            synthesis_request(&fixture.state).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            probe(&fixture.state, "/ready").await.status(),
            StatusCode::OK
        );
        fixture.other_components(StatusCode::OK).await;
        let response = request.await.unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response_json(response).await["error"], "timeout");
        // Repeated timed-out requests cannot degrade the sidecar forever:
        // same-component calls fail fast and never start new inference.
        for _ in 0..6 {
            let response = tokio::time::timeout(
                Duration::from_millis(250),
                synthesis_request(&fixture.state),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response_json(response).await["error"], "unavailable");
        }
        assert!(
            fixture.started.try_recv().is_err(),
            "rejected calls must not run inference"
        );
        fixture.other_components(StatusCode::OK).await;
        // /ready stays degraded while abandoned inference drains.
        let ready = probe(&fixture.state, "/ready").await;
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(ready).await;
        assert_eq!(body["ready"], false);
        assert_eq!(body["inference"]["tts"]["abandoned_in_flight"], 1);
        assert_eq!(body["inference"]["tts"]["active"], true);
        let metrics = response_json(probe(&fixture.state, "/metrics").await).await;
        assert_eq!(metrics["inference"]["tts"]["abandoned_total"], cycle);
        assert_eq!(
            probe(&fixture.state, "/health").await.status(),
            StatusCode::OK
        );
        fixture
            .completion
            .send(if cycle == 2 {
                Completion::Error
            } else {
                Completion::Success
            })
            .unwrap();
        fixture.wait_settled().await;
        // Once the underlying call returns — success or error — the
        // component is re-admitted and shared capacity is restored.
        assert_eq!(
            probe(&fixture.state, "/ready").await.status(),
            StatusCode::OK
        );
    }
    fixture.completion.send(Completion::Success).unwrap();
    assert_eq!(
        synthesis_request(&fixture.state).await.status(),
        StatusCode::OK
    );
    let status = fixture.engine.inference_status().tts;
    assert_eq!(status.abandoned_total, 3);
    assert_eq!(status.abandoned_in_flight, 0);
    assert!(!status.failed);
}

#[tokio::test]
async fn cancelled_request_with_one_permit_is_bounded_and_recovers_after_drain() {
    let mut fixture = Fixture::new(1);
    let request = fixture.start_request();
    fixture.wait_started().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let status = fixture.engine.inference_status().tts;
    assert_eq!(status.abandoned_in_flight, 1);
    assert_eq!(status.abandoned_total, 1);
    assert_eq!(
        synthesis_request(&fixture.state).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // With only one shared permit, STT/VAD correctly fail fast too.
    fixture
        .other_components(StatusCode::SERVICE_UNAVAILABLE)
        .await;
    fixture.completion.send(Completion::Success).unwrap();
    fixture.wait_settled().await;
    fixture.other_components(StatusCode::OK).await;
    assert_eq!(
        probe(&fixture.state, "/ready").await.status(),
        StatusCode::OK
    );
}

/// Regression test for the MR !1263 review finding: a panic while the model
/// mutex is held must surface as a permanent component failure, not as
/// transient Busy with an auto-recovery claim.
#[tokio::test]
async fn panic_with_model_lock_held_settles_accounting_but_requires_restart() {
    for abandon in [false, true] {
        let mut fixture = Fixture::new(2);
        let request = fixture.start_request();
        fixture.wait_started().await;
        if abandon {
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            fixture.completion.send(Completion::Panic).unwrap();
        } else {
            fixture.completion.send(Completion::Panic).unwrap();
            let response = request.await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response_json(response).await["error"], "component_failed");
        }
        fixture.wait_settled().await;
        // The model mutex is genuinely poisoned by the unwinding panic.
        assert!(fixture.engine.tts.is_poisoned());
        let status = fixture.engine.inference_status().tts;
        assert_eq!(status.abandoned_in_flight, 0);
        assert_eq!(status.abandoned_total, u64::from(abandon));
        assert!(status.failed);
        // Later calls report permanent failure, never transient busy, and
        // the poisoned model is never reused.
        for _ in 0..3 {
            let response = synthesis_request(&fixture.state).await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response_json(response).await["error"], "component_failed");
        }
        let ready = probe(&fixture.state, "/ready").await;
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(ready).await["inference"]["tts"]["failed"],
            true
        );
        fixture.other_components(StatusCode::OK).await;
        assert!(
            fixture.started.try_recv().is_err(),
            "poisoned model must not be reused"
        );
    }

    // Recovery reconstructs the engine/model; poison is never cleared in place.
    let fresh = Fixture::new(2);
    fresh.completion.send(Completion::Success).unwrap();
    assert_eq!(
        synthesis_request(&fresh.state).await.status(),
        StatusCode::OK
    );
    assert_eq!(probe(&fresh.state, "/ready").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn returned_inference_error_is_422_and_does_not_poison_or_abandon() {
    let fixture = Fixture::new(2);
    fixture.completion.send(Completion::Error).unwrap();
    let response = synthesis_request(&fixture.state).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response_json(response).await["error"], "processing_failed");
    assert!(!fixture.engine.inference_status().is_degraded());
    fixture.completion.send(Completion::Success).unwrap();
    assert_eq!(
        synthesis_request(&fixture.state).await.status(),
        StatusCode::OK
    );
    assert_eq!(fixture.engine.inference_status().tts.abandoned_total, 0);
}
