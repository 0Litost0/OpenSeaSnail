//! PoC-only executable. Never linked into the production daemon entry point.
use seasnail_crypto::{Argon2Params, Error, KeychainLabel, KeychainStore};
use seasnail_daemon::{build_app, daemon_resources::DaemonResources, AppState, Auth, Crypto};
use seasnail_runtime::{
    AudioNormalizer, MockRuntime, ModelRuntime, RuntimeOperationGate, SidecarRegistry,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

// Isolated synthetic credentials only. Atomic replacements, tempfile's owner-only file permissions.
struct FixtureKeychain {
    path: PathBuf,
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}
impl FixtureKeychain {
    fn open(path: PathBuf) -> anyhow::Result<Self> {
        let values = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?)?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            values: Mutex::new(values),
        })
    }
    fn persist(&self, values: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
        let save = || -> anyhow::Result<()> {
            let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
            serde_json::to_writer(&mut file, values)?;
            file.flush()?;
            file.as_file().sync_all()?;
            file.persist(&self.path)?;
            Ok(())
        };
        save().map_err(|_| Error::Keychain("PoC credential persistence failed".into()))
    }
}
impl KeychainStore for FixtureKeychain {
    fn set_secret(&self, label: KeychainLabel, account: &str, value: &[u8]) -> Result<(), Error> {
        let mut values = self.values.lock().unwrap();
        values.insert(format!("{label:?}/{account}"), value.to_vec());
        self.persist(&values)
    }
    fn get_secret(&self, label: KeychainLabel, account: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(&format!("{label:?}/{account}"))
            .cloned())
    }
    fn delete_secret(&self, label: KeychainLabel, account: &str) -> Result<(), Error> {
        let mut values = self.values.lock().unwrap();
        values.remove(&format!("{label:?}/{account}"));
        self.persist(&values)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var("POC_DATA_DIR")?);
    anyhow::ensure!(
        home.is_absolute() && home.is_dir(),
        "explicit existing PoC directory required"
    );
    let _lock = seasnail_daemon::SingletonLock::acquire(&home)?;
    let shutdown = Arc::new(tokio::sync::Notify::new());
    // A detached std thread does not hold Tokio runtime shutdown open on startup errors.
    let parent_shutdown = shutdown.clone();
    std::thread::spawn(move || {
        let mut byte = [0];
        while matches!(std::io::stdin().read(&mut byte), Ok(n) if n > 0) {}
        parent_shutdown.notify_one();
    });
    let crypto = Arc::new(Crypto::new(
        home.clone(),
        Arc::new(FixtureKeychain::open(home.join("credentials.json"))?),
        Argon2Params::default(),
    )?);
    let registry = Arc::new(SidecarRegistry::new());
    let runtime = std::env::var("POC_RUNTIME").unwrap_or_else(|_| "mock".into());
    anyhow::ensure!(
        runtime == "mock" || runtime == "sherpa",
        "unknown PoC runtime"
    );
    if runtime == "mock" {
        let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
        mock.start(0).await?;
        registry.register(mock).await;
    }
    let state = AppState::with_runtime_handles(
        Arc::new(Auth::new(crypto)),
        registry.clone(),
        Arc::new(RuntimeOperationGate::new_for_composition_root()),
        Arc::new(AudioNormalizer::new(
            std::env::var_os("FFMPEG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffmpeg".into()),
        )),
        home.clone(),
    );
    let resources = DaemonResources::new(state);
    // Like production: a fresh/locked account does not prevent API startup.
    if resources.reconcile_storage().is_err() {
        eprintln!("PoC startup reconcile deferred");
    }
    if runtime == "sherpa" {
        tokio::time::timeout(Duration::from_secs(90), resources.activate_default_model()).await??;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    println!(
        "{}",
        serde_json::json!({"port":port,"pid":std::process::id(),"runtime":runtime})
    );
    std::io::stdout().flush()?;
    let server = axum::serve(listener, build_app(resources.http_state())).with_graceful_shutdown(
        async move {
            shutdown.notified().await;
        },
    );
    server.await?;
    if let Some(active) = registry.active().await {
        active.stop().await?;
    }
    registry.clear().await;
    Ok(())
}
