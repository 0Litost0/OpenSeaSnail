//! Runtime reservation（ST-M4.2）。
//!
//! reservation 的固定顺序是先取得 operation gate，再读取 registry，并把 runtime
//! handle 与 model id 冻结进不可复制的值。未消费或任务异常退出时，lease 由 RAII 释放。

use crate::{
    ModelRuntime, RuntimeLease, RuntimeOperation, RuntimeOperationGate, RuntimeOperationOccupied,
    SidecarRegistry,
};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeReservationError {
    #[error("runtime operation is busy: {0}")]
    Busy(#[from] RuntimeOperationOccupied),
    #[error("no active runtime")]
    Unavailable,
    #[error("runtime restart failed: {0}")]
    Restart(#[source] std::io::Error),
}

/// 一次性、不可 Clone 的转写 runtime reservation。
pub struct ReservedTranscription {
    runtime: Arc<dyn ModelRuntime>,
    model_id: String,
    lease: RuntimeLease,
}

impl ReservedTranscription {
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// 只允许 runtime facade 消费 reservation。不得向 daemon/application 暴露
    /// runtime handle 或 lease，否则调用方可以绕过一次性 facade 重复执行 backend。
    pub(crate) fn into_parts(self) -> (Arc<dyn ModelRuntime>, String, RuntimeLease) {
        (self.runtime, self.model_id, self.lease)
    }
}

/// gate-first 预留 active runtime。
pub async fn reserve_transcription(
    gate: &Arc<RuntimeOperationGate>,
    registry: &SidecarRegistry,
    session_id: &str,
) -> Result<ReservedTranscription, RuntimeReservationError> {
    let lease = gate.acquire(RuntimeOperation::Transcription {
        session_id: session_id.into(),
    })?;
    let Some(runtime) = registry.active().await else {
        return Err(RuntimeReservationError::Unavailable);
    };
    let model_id = runtime.id().to_owned();
    Ok(ReservedTranscription {
        runtime,
        model_id,
        lease,
    })
}

/// Retry 专用 reservation：在同一个 gate lease 内检查并恢复当前 runtime，随后冻结
/// 同一实例。它不选择或切换模型，且 restart 失败时由 reservation drop 释放 gate。
pub async fn reserve_retry_transcription(
    gate: &Arc<RuntimeOperationGate>,
    registry: &SidecarRegistry,
    session_id: &str,
) -> Result<ReservedTranscription, RuntimeReservationError> {
    let lease = gate.acquire(RuntimeOperation::Transcription {
        session_id: session_id.into(),
    })?;
    let Some(runtime) = registry.active().await else {
        return Err(RuntimeReservationError::Unavailable);
    };
    if !runtime.health().await {
        // Real drivers take their process handle before awaiting stop. Clear first so any
        // stop failure cannot leave registry advertising a stopped/unknown runtime.
        registry.clear().await;
        runtime
            .stop()
            .await
            .map_err(RuntimeReservationError::Restart)?;
        if let Err(error) = runtime.start(0).await {
            return Err(RuntimeReservationError::Restart(error));
        }
        registry.register(Arc::clone(&runtime)).await;
    }
    let model_id = runtime.id().to_owned();
    Ok(ReservedTranscription {
        runtime,
        model_id,
        lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OpenAiSegments, TranscribeReq};
    use crate::{Capabilities, Diarization, MockRuntime, RuntimeError, RuntimeKind};
    use async_trait::async_trait;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct RestartRuntime {
        healthy: AtomicBool,
        fail_start: bool,
        fail_stop: bool,
    }

    #[async_trait]
    impl ModelRuntime for RestartRuntime {
        fn id(&self) -> &str {
            "restartable"
        }
        fn runtime_kind(&self) -> RuntimeKind {
            RuntimeKind::Whisper
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                diarization: Diarization::None,
                streaming: false,
                languages: vec!["en".into()],
                max_audio_seconds: 60,
                word_timestamps: false,
            }
        }
        async fn start(&self, _: u16) -> io::Result<()> {
            if self.fail_start {
                return Err(io::Error::other("injected start failure"));
            }
            self.healthy.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn stop(&self) -> io::Result<()> {
            if self.fail_stop {
                return Err(io::Error::other("injected stop failure"));
            }
            self.healthy.store(false, Ordering::SeqCst);
            Ok(())
        }
        async fn health(&self) -> bool {
            self.healthy.load(Ordering::SeqCst)
        }
        async fn transcribe(&self, _: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
            Err(RuntimeError::NotStarted)
        }
    }

    #[tokio::test]
    async fn no_active_runtime_releases_gate() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        assert!(matches!(
            reserve_transcription(&gate, &registry, "s1").await,
            Err(RuntimeReservationError::Unavailable)
        ));
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn reservation_freezes_runtime_and_is_released_on_drop() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        let runtime = Arc::new(MockRuntime::new(
            "mock-a",
            RuntimeKind::Whisper,
            Default::default(),
        ));
        registry.register(runtime).await;
        let reservation = reserve_transcription(&gate, &registry, "s1").await.unwrap();
        assert_eq!(reservation.model_id(), "mock-a");
        assert!(matches!(
            gate.active(),
            Some(RuntimeOperation::Transcription { .. })
        ));
        drop(reservation);
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn retry_reservation_recovers_unhealthy_current_runtime_under_gate() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        let runtime = Arc::new(RestartRuntime {
            healthy: AtomicBool::new(false),
            fail_start: false,
            fail_stop: false,
        });
        registry.register(runtime.clone()).await;
        assert!(!runtime.health().await);

        let reservation = reserve_retry_transcription(&gate, &registry, "retry")
            .await
            .unwrap();
        assert_eq!(reservation.model_id(), "restartable");
        assert!(runtime.health().await);
        drop(reservation);
        assert!(gate.active().is_none());
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn retry_recovery_start_failure_clears_registry() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        registry
            .register(Arc::new(RestartRuntime {
                healthy: AtomicBool::new(false),
                fail_start: true,
                fail_stop: false,
            }))
            .await;

        assert!(matches!(
            reserve_retry_transcription(&gate, &registry, "retry").await,
            Err(RuntimeReservationError::Restart(_))
        ));
        assert_eq!(registry.active_id().await, None);
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn retry_recovery_stop_failure_clears_registry_and_does_not_start_again() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        registry
            .register(Arc::new(RestartRuntime {
                healthy: AtomicBool::new(false),
                fail_start: false,
                fail_stop: true,
            }))
            .await;

        assert!(matches!(
            reserve_retry_transcription(&gate, &registry, "retry").await,
            Err(RuntimeReservationError::Restart(_))
        ));
        assert_eq!(registry.active_id().await, None);
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn busy_model_switch_is_not_mistaken_for_unavailable() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let _switch = gate
            .acquire(RuntimeOperation::ModelSwitch {
                model_id: "m1".into(),
            })
            .unwrap();
        let registry = SidecarRegistry::new();
        assert!(matches!(
            reserve_transcription(&gate, &registry, "s1").await,
            Err(RuntimeReservationError::Busy(_))
        ));
    }

    #[tokio::test]
    async fn failure_before_worker_spawn_releases_reservation() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = SidecarRegistry::new();
        registry
            .register(Arc::new(MockRuntime::new(
                "mock-a",
                RuntimeKind::Whisper,
                Default::default(),
            )))
            .await;
        let reservation = reserve_transcription(&gate, &registry, "s1").await.unwrap();
        let preparation: Result<(), &'static str> = Err("prepare failed");
        if preparation.is_err() {
            drop(reservation);
        }
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn aborted_worker_releases_reservation() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        registry
            .register(Arc::new(MockRuntime::new(
                "mock-a",
                RuntimeKind::Whisper,
                Default::default(),
            )))
            .await;
        let (reserved_tx, reserved_rx) = tokio::sync::oneshot::channel();
        let task_gate = Arc::clone(&gate);
        let task_registry = Arc::clone(&registry);
        let worker = tokio::spawn(async move {
            let _reservation = reserve_transcription(&task_gate, &task_registry, "s1")
                .await
                .unwrap();
            let _ = reserved_tx.send(());
            std::future::pending::<()>().await;
        });
        reserved_rx.await.unwrap();
        assert!(gate.active().is_some());
        worker.abort();
        let _ = worker.await;
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn panicking_worker_releases_reservation() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        registry
            .register(Arc::new(MockRuntime::new(
                "mock-a",
                RuntimeKind::Whisper,
                Default::default(),
            )))
            .await;
        let task_gate = Arc::clone(&gate);
        let worker = tokio::spawn(async move {
            let _reservation = reserve_transcription(&task_gate, &registry, "s1")
                .await
                .unwrap();
            panic!("worker panic after reservation");
        });
        assert!(worker.await.unwrap_err().is_panic());
        assert!(gate.active().is_none());
    }
}
