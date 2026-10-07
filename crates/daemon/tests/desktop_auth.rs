//! Desktop capability login/logout lifecycle, separate from public root-only account APIs.
use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::daemon_resources::DaemonResources;
use seasnail_daemon::{
    build_app, http_get, http_get_with_headers, http_post, AppState, Auth, Crypto,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

#[tokio::test]
async fn desktop_logout_preserves_account_but_disables_restart_auto_login() {
    let home = tempfile::tempdir().unwrap();
    let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.path().into(), keychain.clone(), params()).unwrap());
    let auth = Arc::new(Auth::new(crypto.clone()));
    let capability = seasnail_daemon::desktop_auth::rotate_capability(home.path()).unwrap();
    let state = AppState::new(auth, home.path().into()).with_desktop_capability(capability.clone());
    let app = build_app(DaemonResources::new(state).http_state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let headers = [("x-seasnail-desktop-capability", capability.as_str())];
    for bad in [&[][..], &[("x-seasnail-desktop-capability", "wrong")][..]] {
        assert_eq!(
            http_get_with_headers(port, "/internal/desktop-auth/status", bad)
                .await
                .unwrap()
                .0,
            401
        );
        assert_eq!(
            http_post(
                port,
                "/internal/desktop-auth/create",
                r#"{"username":"x","password":"p"}"#,
                bad
            )
            .await
            .unwrap()
            .0,
            401
        );
    }
    let (code, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"secret-password"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 201);
    let token: Value = serde_json::from_str(&body).unwrap();
    let bearer = format!("Bearer {}", token["secret"].as_str().unwrap());
    assert_eq!(
        http_get_with_headers(
            port,
            "/internal/desktop-auth/status",
            &[("Authorization", &bearer)]
        )
        .await
        .unwrap()
        .0,
        401
    );
    let (_, body) = http_get_with_headers(port, "/internal/desktop-auth/status", &headers)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(&body).unwrap();
    let id = status["accounts"][0]["id"].as_str().unwrap();
    assert_eq!(status["authenticated"], true);
    assert!(
        !body.contains("secret-password")
            && !body.contains("ss_live_")
            && !body.contains(&capability)
    );
    assert_eq!(
        http_post(
            port,
            "/internal/desktop-auth/create",
            r#"{"username":"bob","password":"p"}"#,
            &headers
        )
        .await
        .unwrap()
        .0,
        409
    );
    assert_eq!(
        http_post(
            port,
            "/api/v1/dictionary/entries",
            r#"{"terms":["RetainedTerm"]}"#,
            &[("Authorization", &bearer)]
        )
        .await
        .unwrap()
        .0,
        200
    );
    // An encrypted DB exists before logout; it remains unchanged on disk.
    let files_before: Vec<_> = walkdir::WalkDir::new(home.path())
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().to_path_buf(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    let (code, body) = http_post(port, "/internal/desktop-auth/logout", "{}", &headers)
        .await
        .unwrap();
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["authenticated"],
        false
    );
    assert!(crypto.active_account_id().is_none());
    let reopened = Crypto::new(home.path().into(), keychain.clone(), params()).unwrap();
    assert!(reopened.active_account_id().is_none());
    assert_eq!(reopened.list_accounts().len(), 1);
    for (path, bytes) in files_before {
        if path.extension().is_some_and(|ext| ext == "db") {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    assert_eq!(
        http_get_with_headers(port, "/api/v1/accounts", &[("Authorization", &bearer)])
            .await
            .unwrap()
            .0,
        423
    );
    assert_eq!(http_get(port, "/api/v1/accounts").await.unwrap().0, 401);
    assert_eq!(
        http_post(
            port,
            "/api/v1/accounts",
            r#"{"username":"bob","password":"p"}"#,
            &[]
        )
        .await
        .unwrap()
        .0,
        401
    );
    let wrong = json!({"id":id,"password":"wrong"}).to_string();
    assert_eq!(
        http_post(port, "/internal/desktop-auth/login", &wrong, &headers)
            .await
            .unwrap()
            .0,
        403
    );
    let correct = json!({"id":id,"password":"secret-password"}).to_string();
    let (code, body) = http_post(port, "/internal/desktop-auth/login", &correct, &headers)
        .await
        .unwrap();
    assert_eq!(code, 200, "{body}");
    assert_eq!(crypto.active_account_id().as_deref(), Some(id));
    let (code, dictionary) =
        http_get_with_headers(port, "/api/v1/dictionary", &[("Authorization", &bearer)])
            .await
            .unwrap();
    assert_eq!(code, 200);
    assert!(dictionary.contains("RetainedTerm"));
    assert!(Crypto::new(home.path().into(), keychain, params())
        .unwrap()
        .active_account_id()
        .is_some());
    http_post(port, "/internal/desktop-auth/logout", "{}", &headers)
        .await
        .unwrap();
    let (code, body) = http_post(
        port,
        "/internal/desktop-auth/create",
        r#"{"username":"bob","password":"p"}"#,
        &headers,
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "{body}");
    assert_eq!(crypto.list_accounts().len(), 2);
    assert_ne!(crypto.active_account_id().as_deref(), Some(id));
    server.abort();
}

#[test]
fn desktop_capability_rotates_and_rejects_unsafe_files() {
    let home = tempfile::tempdir().unwrap();
    let first = seasnail_daemon::desktop_auth::rotate_capability(home.path()).unwrap();
    assert_eq!(
        seasnail_daemon::desktop_auth::read_capability(home.path()).unwrap(),
        first
    );
    let next = seasnail_daemon::desktop_auth::rotate_capability(home.path()).unwrap();
    assert_ne!(next, first);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = home.path().join(".desktop-auth-capability");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(seasnail_daemon::desktop_auth::read_capability(home.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("target", path).unwrap();
        assert!(seasnail_daemon::desktop_auth::read_capability(home.path()).is_err());
    }
}
