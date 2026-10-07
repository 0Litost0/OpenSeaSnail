//! Shared process composition. Dependency implementations are selected by entrypoints;
//! authentication, application services, startup order and shutdown remain identical.
use crate::{
    api::{AppState, RuntimeFactory},
    bootstrap::Bootstrap,
    daemon_resources::DaemonResources,
    init_logging, lifecycle, Auth, Crypto, SingletonLock,
};
use seasnail_crypto::{Argon2Params, KeychainStore};
use seasnail_runtime::{AudioNormalizer, RuntimeOperationGate, SidecarRegistry};
use std::{future::IntoFuture, io, path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Notify};

pub struct LaunchConfig {
    pub data_dir: PathBuf,
    pub keychain: Arc<dyn KeychainStore>,
    pub runtime_factory: Option<RuntimeFactory>,
    pub ffmpeg: PathBuf,
    pub shutdown_budget: Duration,
}

impl LaunchConfig {
    pub fn production(data_dir: PathBuf, keychain: Arc<dyn KeychainStore>) -> Self {
        Self {
            data_dir,
            keychain,
            runtime_factory: None,
            ffmpeg: std::env::var_os("FFMPEG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffmpeg".into()),
            shutdown_budget: Duration::from_secs(10),
        }
    }
}

pub struct Ready {
    pub port: u16,
    pub pid: u32,
    pub stages: Vec<&'static str>,
    pub warmup: tokio::sync::watch::Receiver<Option<bool>>,
}

struct BootstrapOwner(Bootstrap);
impl Drop for BootstrapOwner {
    fn drop(&mut self) {
        if let Err(error) = self.0.clear() {
            tracing::warn!(?error, "bootstrap cleanup failed");
        }
    }
}

pub async fn run(
    config: LaunchConfig,
    ready: impl FnOnce(Ready) -> io::Result<()>,
) -> anyhow::Result<()> {
    let _logs = init_logging(&config.data_dir.join("logs"));
    let mut stages = vec!["logging"];
    let _lock = SingletonLock::acquire(&config.data_dir)?;
    stages.push("singleton");
    // Only the process composition root sets the subprocess ownership root.
    std::env::set_var("SEASNAIL_DATA_DIR", &config.data_dir);
    let shutdown = Arc::new(Notify::new());
    lifecycle::spawn_parent_death_watcher(shutdown.clone());
    stages.push("parent_watch");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    stages.push("bind");
    let desktop_capability = crate::desktop_auth::rotate_capability(&config.data_dir)?;
    let bootstrap = BootstrapOwner(Bootstrap::new(config.data_dir.clone()));
    bootstrap.0.write(port)?;
    stages.push("bootstrap");
    let crypto = Arc::new(Crypto::new(
        config.data_dir.clone(),
        config.keychain,
        Argon2Params::default(),
    )?);
    let auth = Arc::new(Auth::new(crypto));
    let temporary = tempfile::Builder::new()
        .prefix(".daemon-tmp-")
        .tempdir_in(&config.data_dir)?;
    let mut state = AppState::with_runtime_handles(
        auth,
        Arc::new(SidecarRegistry::new_with_orphan_dir(
            config.data_dir.join("sidecars"),
        )),
        Arc::new(RuntimeOperationGate::new_for_composition_root()),
        Arc::new(AudioNormalizer::with_temp_dir(
            config.ffmpeg,
            temporary.path().to_path_buf(),
        )),
        config.data_dir,
    );
    if let Some(factory) = config.runtime_factory {
        state = state.with_runtime_factory(factory);
    }
    let resources = DaemonResources::new(state.with_desktop_capability(desktop_capability));
    stages.push("restore_and_compose");
    if let Err(error) = resources.reconcile_storage() {
        tracing::warn!(?error, "startup storage reconcile deferred");
    }
    stages.push("reconcile");
    let reap = resources.reap_orphans().await;
    tracing::info!(
        orphan_pids_killed = reap.orphan_pids_killed,
        "startup orphan reap completed"
    );
    stages.push("reap");
    resources.spawn_default_warmup();
    stages.push("warmup_scheduled");
    let mut shutdown_deadline = None;
    let result = async {
        // Discovery is distinct from actual HTTP health/model readiness.
        ready(Ready { port, pid:std::process::id(), stages, warmup:resources.warmup_status() })?;
        let stop = Arc::new(Notify::new());
        let stopped = stop.clone();
        let server = axum::serve(listener, crate::build_app(resources.http_state()))
            .with_graceful_shutdown(async move { stopped.notified().await }).into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result?,
            _ = shutdown_signal(shutdown) => {
                shutdown_deadline = Some(tokio::time::Instant::now() + config.shutdown_budget);
                resources.request_shutdown();
                stop.notify_one();
                tokio::time::timeout_at(shutdown_deadline.unwrap(), &mut server).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP shutdown deadline"))??;
            }
        }
        Ok::<_, anyhow::Error>(())
    }.await;
    let deadline =
        shutdown_deadline.unwrap_or_else(|| tokio::time::Instant::now() + config.shutdown_budget);
    let close = tokio::time::timeout_at(deadline, resources.shutdown()).await;
    let close_error = match close {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(io::Error::new(
            io::ErrorKind::TimedOut,
            "resource shutdown deadline",
        )),
    };
    if close_error.is_some() {
        // Preserve audio/resources for recovery instead of dropping a still-owned directory.
        let _preserved = temporary.keep();
    }
    if let Err(error) = &result {
        tracing::error!(?error, "daemon execution failed");
    }
    if let Some(error) = close_error {
        tracing::error!(?error, "daemon resource cleanup failed");
        result?;
        return Err(error.into());
    }
    result
}

async fn shutdown_signal(shutdown: Arc<Notify>) {
    tokio::select! {
        _ = shutdown.notified() => tracing::info!("shutdown: parent EOF"),
        _ = tokio::signal::ctrl_c() => tracing::info!("shutdown: SIGINT"),
        _ = terminate() => tracing::info!("shutdown: SIGTERM"),
    }
}

#[cfg(unix)]
async fn terminate() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut signal) => {
            signal.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}
#[cfg(not(unix))]
async fn terminate() {
    std::future::pending::<()>().await
}
