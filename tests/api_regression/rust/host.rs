mod keychain;
mod scenario;
use seasnail_daemon::{
    api::RuntimeFactory,
    launch::{self, LaunchConfig},
};
use std::os::unix::fs::PermissionsExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

fn isolated_home(root: &Path, home: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        root.is_absolute() && home.is_absolute(),
        "isolation paths must be absolute"
    );
    anyhow::ensure!(
        !root.is_symlink() && !home.is_symlink(),
        "symlink isolation root/home rejected"
    );
    let root = root.canonicalize()?;
    let home = home.canonicalize()?;
    anyhow::ensure!(
        home.starts_with(&root) && home != root,
        "home must be below isolated root"
    );
    let marker = root.join(".api-regression-root");
    anyhow::ensure!(
        !marker.is_symlink()
            && std::fs::read_to_string(marker)?.trim() == "seasnail-api-regression-v1",
        "missing isolation marker"
    );
    let production = seasnail_daemon::bootstrap::Bootstrap::default_dir()?;
    // Never derive the production path from an inherited test override.
    let personal = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/SeaSnail"));
    anyhow::ensure!(
        home != production && personal.as_ref().is_none_or(|p| !home.starts_with(p)),
        "production directory rejected"
    );
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    anyhow::ensure!(
        !home.join("fixture-keychain.json").is_symlink(),
        "keychain symlink rejected"
    );
    Ok(home)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity_args: Vec<String> = std::env::args().skip(1).collect();
    if identity_args
        .first()
        .is_some_and(|v| v == "--process-identity")
    {
        anyhow::ensure!(identity_args.len() == 2, "identity requires PID");
        println!(
            "{}",
            serde_json::to_string(&seasnail_runtime::process_identity::inspect(
                identity_args[1].parse()?
            )?)?
        );
        return Ok(());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 1,
        "usage: seasnail-api-test-host <restricted-config.json>"
    );
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let root = Path::new(
        config["root"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing root"))?,
    );
    let home = Path::new(
        config["home"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing home"))?,
    );
    let home = isolated_home(root, home)?;
    let keychain = Arc::new(keychain::FixtureKeychain::new(
        home.clone(),
        config["keychain"].as_str().unwrap_or("persistent"),
    )?);
    let mut launch_config = LaunchConfig::production(home, keychain);
    let mode = config["runtime"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing runtime"))?;
    match mode {
        "deterministic" => {
            let scenario = config["scenario"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing scenario"))?;
            let path = Path::new(
                config["scenario_file"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing scenario file"))?,
            );
            let runtime = Arc::new(scenario::ScenarioRuntime::load(
                "sensevoice-small-sherpa-int8",
                path,
                scenario,
            )?);
            launch_config.runtime_factory = Some(RuntimeFactory {
                installed: Arc::new(|id| id == "sensevoice-small-sherpa-int8"),
                build: Arc::new(move |id| {
                    if id != "sensevoice-small-sherpa-int8" {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "unsupported fixture model",
                        ));
                    }
                    Ok(runtime.clone())
                }),
            });
        }
        "sherpa" => {}
        _ => anyhow::bail!("unsupported runtime; no fallback"),
    }
    let mut observer = None;
    let result = launch::run(launch_config, |ready| {
        println!("{}",serde_json::json!({"event":"ready","port":ready.port,"pid":ready.pid,"stages":ready.stages,"runtime":mode}));
        std::io::stdout().flush()?;
        let mut status = ready.warmup;
        observer = Some(tokio::spawn(async move {
            loop {
                let state = *status.borrow_and_update();
                if let Some(success) = state {
                    println!("{}",serde_json::json!({"event":"model-ready","success":success}));
                    let _ = std::io::stdout().flush();
                    break;
                }
                if status.changed().await.is_err() {break}
            }
        }));
        Ok(())
    }).await;
    if let Some(observer) = observer {
        observer.abort();
        let _ = observer.await;
    }
    result
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn home_requires_explicit_isolation_and_rejects_escape_or_symlink() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("case");
        std::fs::create_dir(&home).unwrap();
        assert!(isolated_home(root.path(), &home).is_err());
        std::fs::write(
            root.path().join(".api-regression-root"),
            "seasnail-api-regression-v1",
        )
        .unwrap();
        assert!(isolated_home(root.path(), root.path()).is_err());
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(isolated_home(root.path(), elsewhere.path()).is_err());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert!(isolated_home(root.path(), &alias).is_err());
        assert!(isolated_home(root.path(), &home).is_ok());
        std::os::unix::fs::symlink(
            elsewhere.path().join("keychain"),
            home.join("fixture-keychain.json"),
        )
        .unwrap();
        assert!(isolated_home(root.path(), &home).is_err());
    }
}
