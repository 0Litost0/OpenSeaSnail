//! daemon 进程资源与 HTTP 状态的组合边界（M1.2）。
//!
//! `DaemonResources` 只由进程组合根持有，负责启动期 reconcile、orphan reap 和
//! 默认 runtime warmup；HTTP router 只接收其持有的唯一 `HttpState`。

use crate::account::AccountError;
use crate::api::{AppState, HttpState};

#[derive(Default)]
struct WarmupOwner(std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>);
impl Drop for WarmupOwner {
    fn drop(&mut self) {
        if let Some(task) = self.0.get_mut().unwrap_or_else(|p| p.into_inner()).take() {
            task.abort();
        }
    }
}

/// daemon 进程级资源 owner。HTTP handler 不应直接持有该类型。
#[derive(Clone)]
pub struct DaemonResources {
    app: AppState,
    state: HttpState,
    warmup_status: tokio::sync::watch::Sender<Option<bool>>,
    warmup: std::sync::Arc<WarmupOwner>,
}

impl DaemonResources {
    pub fn new(state: AppState) -> Self {
        Self {
            app: state.clone(),
            state: HttpState::new(state),
            warmup: Default::default(),
            warmup_status: tokio::sync::watch::channel(None).0,
        }
    }

    /// 返回唯一 HTTP 状态快照；其中只含应用服务和 transport-local 状态。
    pub fn http_state(&self) -> HttpState {
        self.state.clone()
    }

    /// 启动期数据 reconcile。锁定账户时保留既有可观测 warning 语义。
    pub fn reconcile_storage(&self) -> Result<(), AccountError> {
        self.app.reconcile_storage_for_lifecycle()
    }

    /// 回收上次异常退出留下的 sidecar，并返回现有统计结果。
    pub async fn reap_orphans(&self) -> seasnail_runtime::ReapReport {
        self.app.reap_runtime_orphans_for_lifecycle().await
    }

    /// 生命周期直接调用应用服务预热默认模型；不经过 HTTP adapter。
    pub async fn activate_default_model(&self) -> Result<(), crate::application::ApplicationError> {
        self.state.model_service().activate_default().await
    }

    /// 在 HTTP 就绪后异步预热默认模型；失败仅记录 warning，不阻塞 API 启动。
    pub fn spawn_default_warmup(&self) {
        let service = self.state.model_service().clone();
        let status = self.warmup_status.clone();
        let mut warmup = self.warmup.0.lock().unwrap_or_else(|p| p.into_inner());
        if warmup.is_some() {
            return;
        }
        *warmup = Some(tokio::spawn(async move {
            if let Err(err) = service.activate_default().await {
                status.send_replace(Some(false));
                tracing::warn!(?err, "默认本地模型预热失败");
            } else {
                status.send_replace(Some(true));
                tracing::info!("默认 SenseVoice Sherpa ONNX 模型已就绪");
            }
        }));
    }

    pub fn warmup_status(&self) -> tokio::sync::watch::Receiver<Option<bool>> {
        self.warmup_status.subscribe()
    }

    pub fn request_shutdown(&self) {
        if let Some(warmup) = self
            .warmup
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            warmup.abort();
        }
        self.state.transcription_service().close_for_shutdown();
    }

    pub async fn shutdown(&self) -> std::io::Result<()> {
        self.request_shutdown();
        let warmup = self
            .warmup
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(warmup) = warmup {
            let _ = warmup.await;
        }
        self.state.transcription_service().shutdown().await;
        self.app.stop_runtime_for_lifecycle().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Auth, Crypto};
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use std::sync::Arc;

    struct PendingRuntime(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    #[async_trait::async_trait]
    impl seasnail_runtime::ModelRuntime for PendingRuntime {
        fn id(&self) -> &str {
            "sensevoice-small-sherpa-int8"
        }
        fn runtime_kind(&self) -> seasnail_runtime::RuntimeKind {
            seasnail_runtime::RuntimeKind::SherpaOnnx
        }
        fn capabilities(&self) -> seasnail_runtime::Capabilities {
            seasnail_runtime::Capabilities::sensevoice_gguf()
        }
        async fn start(&self, _: u16) -> std::io::Result<()> {
            struct Guard(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            let _guard = Guard(self.1.clone());
            self.0.store(true, std::sync::atomic::Ordering::Release);
            std::future::pending().await
        }
        async fn stop(&self) -> std::io::Result<()> {
            Ok(())
        }
        async fn health(&self) -> bool {
            false
        }
        async fn transcribe(
            &self,
            _: seasnail_runtime::contract::TranscribeReq,
        ) -> Result<seasnail_runtime::contract::OpenAiSegments, seasnail_runtime::RuntimeError>
        {
            Err(seasnail_runtime::RuntimeError::NotStarted)
        }
    }
    #[tokio::test]
    async fn warmup_is_cancelled_when_last_lifecycle_owner_drops() {
        let dir = tempfile::tempdir().unwrap();
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runtime = Arc::new(PendingRuntime(started.clone(), dropped.clone()));
        let crypto = Arc::new(
            Crypto::new(
                dir.path().into(),
                Arc::new(MemoryKeychain::new()),
                Argon2Params::default(),
            )
            .unwrap(),
        );
        let state = AppState::new(Arc::new(Auth::new(crypto)), dir.path().into())
            .with_runtime_factory(crate::api::RuntimeFactory {
                installed: Arc::new(|_| true),
                build: Arc::new(move |_| Ok(runtime.clone())),
            });
        let resources = DaemonResources::new(state);
        resources.spawn_default_warmup();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !started.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        drop(resources);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !dropped.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn resources_provide_a_cloneable_http_state_without_moving_lifecycle_owner() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(
            Crypto::new(
                dir.path().to_path_buf(),
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        let state = AppState::new(Arc::new(Auth::new(crypto)), dir.path().to_path_buf());
        let resources = DaemonResources::new(state);
        let first = resources.http_state();
        let second = resources.http_state();
        assert!(Arc::ptr_eq(&first.services().auth, &second.services().auth));
        assert_eq!(
            first.auth_service().is_initialized(),
            second.auth_service().is_initialized()
        );
    }
}
