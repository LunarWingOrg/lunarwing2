use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use lunarwing_common::repl_wire::{ServerFrame, StatusFrame, encode_line};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::ReplLimits;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrameClass {
    Control,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Delivery {
    Enqueued,
    ShedStatus,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum OfferError {
    #[error("failed to encode REPL response: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("slow consumer disconnected")]
    SlowConsumer,
}

pub(super) struct QueuedFrame {
    bytes: Vec<u8>,
    class: FrameClass,
}

#[derive(Clone)]
pub(super) struct Outbound {
    tx: mpsc::Sender<QueuedFrame>,
    queued_bytes: Arc<AtomicUsize>,
    dropped_status: Arc<AtomicU64>,
    limits: ReplLimits,
    kill: CancellationToken,
}

impl Outbound {
    pub(super) fn channel(
        limits: ReplLimits,
        kill: CancellationToken,
    ) -> (Self, mpsc::Receiver<QueuedFrame>) {
        let (tx, rx) = mpsc::channel(limits.outbound_queue_frames);
        (
            Self {
                tx,
                queued_bytes: Arc::new(AtomicUsize::new(0)),
                dropped_status: Arc::new(AtomicU64::new(0)),
                limits,
                kill,
            },
            rx,
        )
    }

    pub(super) fn offer(
        &self,
        frame: &ServerFrame,
        class: FrameClass,
    ) -> Result<Delivery, OfferError> {
        let bytes = encode_line(frame)?;
        self.offer_bytes(bytes, class)
    }

    fn offer_bytes(
        &self,
        bytes: Vec<u8>,
        class: FrameClass,
    ) -> Result<Delivery, OfferError> {
        if self.kill.is_cancelled() {
            return Err(OfferError::SlowConsumer);
        }
        if class == FrameClass::Status
            && (self.queued_bytes.load(Ordering::Acquire) > self.limits.status_shed_bytes
                || self.tx.capacity() == 0)
        {
            self.dropped_status.fetch_add(1, Ordering::AcqRel);
            return Ok(Delivery::ShedStatus);
        }

        let len = bytes.len();
        let reserved = self.queued_bytes.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| {
                current
                    .checked_add(len)
                    .filter(|total| *total <= self.limits.outbound_queue_bytes)
            },
        );
        if reserved.is_err() {
            return self.overflow(class);
        }

        let queued = QueuedFrame { bytes, class };
        if let Err(error) = self.tx.try_send(queued) {
            self.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            return self.overflow(class);
        }
        Ok(Delivery::Enqueued)
    }

    fn overflow(&self, class: FrameClass) -> Result<Delivery, OfferError> {
        if class == FrameClass::Status {
            self.dropped_status.fetch_add(1, Ordering::AcqRel);
            return Ok(Delivery::ShedStatus);
        }
        self.kill.cancel();
        Err(OfferError::SlowConsumer)
    }
}

pub(super) async fn writer_task(
    mut write: OwnedWriteHalf,
    mut rx: mpsc::Receiver<QueuedFrame>,
    queued_bytes: Arc<AtomicUsize>,
    dropped_status: Arc<AtomicU64>,
    limits: ReplLimits,
    kill: CancellationToken,
) {
    loop {
        let queued = tokio::select! {
            () = kill.cancelled() => break,
            queued = rx.recv() => queued,
        };
        let Some(queued) = queued else {
            break;
        };
        queued_bytes.fetch_sub(queued.bytes.len(), Ordering::AcqRel);

        if queued.class == FrameClass::Status {
            let missed = dropped_status.swap(0, Ordering::AcqRel);
            if missed > 0
                && !write_frame(
                    &mut write,
                    &ServerFrame::Status(StatusFrame {
                        msg_id: None,
                        event: serde_json::json!({"type": "stream_gap", "missed_events": missed}),
                    }),
                    limits,
                )
                .await
            {
                break;
            }
        }
        if !write_bytes(&mut write, &queued.bytes, limits).await {
            break;
        }
    }
    kill.cancel();
    debug!("unix socket REPL writer stopped");
}

async fn write_frame(write: &mut OwnedWriteHalf, frame: &ServerFrame, limits: ReplLimits) -> bool {
    match encode_line(frame) {
        Ok(bytes) => write_bytes(write, &bytes, limits).await,
        Err(error) => {
            debug!(%error, "unix socket REPL: outbound serialization failed");
            false
        }
    }
}

async fn write_bytes(write: &mut OwnedWriteHalf, bytes: &[u8], limits: ReplLimits) -> bool {
    match tokio::time::timeout(limits.write_timeout, write.write_all(bytes)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            debug!(%error, "unix socket REPL: write failed");
            false
        }
        Err(_) => {
            debug!("unix socket REPL: write timed out");
            false
        }
    }
}
