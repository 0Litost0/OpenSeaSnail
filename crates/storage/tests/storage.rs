//! 集成验收（ST-M2.2）：SQLCipher raw key 开库 / schema migrate / CRUD / 错钥 / per-account 隔离。

use rusqlite::{params, Connection};
use seasnail_storage::model;
use seasnail_storage::{
    checkpoint_raw, complete_cleanup, create_provider_config, fail_transcription,
    list_provider_configs, migrate, normalize_provider_endpoint, open_db,
    recover_interrupted_cleanup, remove_provider_config_and_disable, replace_provider_config,
    CleanupCompletion, CleanupFailureCode, Error, ProviderConfigInput, ProviderType,
    RawCheckpointDecision, RecoveredCleanupOutcome, RecoveryExpectedState, SessionRow, TokenRow,
};
use tempfile::tempdir;

/// 32B 测试钥：同一字节填满。不同 byte 模拟不同账户的 K_sqlite。
fn key(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn session(id: &str, acct: &str, created_at: i64) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        account_id: acct.to_string(),
        created_at,
        source: "realtime".into(),
        language: "zh".into(),
        duration_sec: 12.5,
        status: "completed".into(),
        model: "whisper-base".into(),
        input_device: Some("MacBook Mic".into()),
        file_name: None,
        audio_path: Some(format!("2026-08-10/{id}/audio.enc")),
        transcript_path: Some(format!("2026-08-10/{id}/transcript.pb.enc")),
        failure_reason: None,
        context_present: false,
        cleanup_status: "not_requested".into(),
        cleanup_path: None,
        cleanup_error_code: None,
    }
}

fn transcribing_session(id: &str) -> SessionRow {
    let mut row = session(id, "acct", 1000);
    row.status = "transcribing".into();
    row.duration_sec = 0.0;
    row.transcript_path = None;
    row.cleanup_status = "not_requested".into();
    row.cleanup_path = None;
    row.cleanup_error_code = None;
    row
}

fn token(id: &str, acct: &str, hash: &str) -> TokenRow {
    TokenRow {
        id: id.to_string(),
        account_id: acct.to_string(),
        name: "test token".into(),
        prefix: "ss_live_abcd".into(),
        token_hash: hash.to_string(),
        is_root: false,
        scopes: vec!["sessions:read".into()],
        created_at: 1000,
        last_used_at: None,
    }
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            params![name],
            |r| r.get(0),
        )
        .unwrap();
    n > 0
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name=?1");
    conn.query_row(&sql, [column], |r| r.get::<_, i64>(0))
        .unwrap()
        > 0
}

fn v3_connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY NOT NULL,
            account_id TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            source TEXT NOT NULL,
            language TEXT NOT NULL,
            duration_sec REAL NOT NULL,
            status TEXT NOT NULL,
            model TEXT NOT NULL,
            input_device TEXT,
            file_name TEXT,
            audio_path TEXT,
            transcript_path TEXT,
            failure_reason TEXT,
            context_present INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE tokens (
            id TEXT PRIMARY KEY NOT NULL,
            account_id TEXT NOT NULL,
            name TEXT NOT NULL,
            prefix TEXT NOT NULL,
            token_hash TEXT NOT NULL,
            is_root INTEGER NOT NULL DEFAULT 0,
            scopes TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            last_used_at INTEGER
        );
        PRAGMA user_version = 3;
        "#,
    )
    .unwrap();
    conn
}

/// 新库开库即建表，user_version=5（v1 至 v5 顺序迁移）。
#[test]
fn open_new_db_creates_tables_and_user_version() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("fresh.db"), &key(0x70)).unwrap();
    assert!(table_exists(&conn, "sessions"));
    assert!(table_exists(&conn, "tokens"));
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 5, "user_version 应为 5");
    // v2 加的 failure_reason 列存在。
    let cols: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('sessions') WHERE name='failure_reason'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cols, 1, "sessions.failure_reason 列应存在");
    for column in ["cleanup_status", "cleanup_path", "cleanup_error_code"] {
        assert!(column_exists(&conn, "sessions", column));
    }
    assert!(table_exists(&conn, "reasoning_provider_configs"));
    assert!(table_exists(&conn, "cleanup_settings"));
    assert!(table_exists(&conn, "dictionary_entries"));
}

/// migrate 幂等：open_db 内已跑一次，再跑无错、表仍在。
#[test]
fn migrate_is_idempotent() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("idem.db"), &key(0x80)).unwrap();
    migrate(&conn).unwrap();
    assert!(table_exists(&conn, "sessions"));
    assert!(table_exists(&conn, "tokens"));
}

/// 崩溃幂等（ST-M3.7 migrate v2/v3）：模拟 ALTER 已提交但 user_version 未推进。
/// 未及提交（进程被杀/掉电）的半截态——列已存在、version 仍 1——再跑 migrate
/// 不应报「duplicate column name」锁死账户，应跳过 ALTER、推进 version=2。
/// 旧实现（无列探测、直接 ALTER）在此态下会 duplicate column name → 账户永久不可开。
#[test]
fn migrate_v2_crash_safe_after_partial_alter() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("partial.db"), &key(0x90)).unwrap();
    // 模拟半截态：failure_reason 列已存在（v2 ALTER 已跑），但 user_version 回拨到 1。
    conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    // 再跑 migrate 不应报错。
    migrate(&conn).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 5, "半截态后 migrate 应推进到 version=5");
    let cols: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('sessions') WHERE name='failure_reason'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cols, 1, "failure_reason 列仍存在");
}

#[test]
fn migrate_v3_to_v4_preserves_historical_rows_with_raw_defaults() {
    let conn = v3_connection();
    conn.execute(
        "INSERT INTO sessions
         (id, account_id, created_at, source, language, duration_sec, status, model,
          context_present)
         VALUES ('old', 'acct', 1, 'realtime', 'zh', 2.0, 'completed', 'old-model', 0)",
        [],
    )
    .unwrap();

    migrate(&conn).unwrap();
    let row = model::get_session(&conn, "old").unwrap().unwrap();
    assert_eq!(row.cleanup_status, "not_requested");
    assert_eq!(row.cleanup_path, None);
    assert_eq!(row.cleanup_error_code, None);
    assert!(table_exists(&conn, "reasoning_provider_configs"));
    assert!(table_exists(&conn, "cleanup_settings"));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
}

#[test]
fn migrate_v4_is_crash_idempotent_from_partial_columns_and_tables() {
    let conn = v3_connection();
    conn.execute_batch(
        r#"
        ALTER TABLE sessions ADD COLUMN cleanup_status TEXT NOT NULL DEFAULT 'not_requested';
        CREATE TABLE reasoning_provider_configs (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            provider_type TEXT NOT NULL,
            endpoint TEXT NOT NULL,
            endpoint_fingerprint TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        "#,
    )
    .unwrap();

    migrate(&conn).unwrap();
    migrate(&conn).unwrap();
    for column in ["cleanup_status", "cleanup_path", "cleanup_error_code"] {
        assert!(column_exists(&conn, "sessions", column));
    }
    assert!(table_exists(&conn, "reasoning_provider_configs"));
    assert!(table_exists(&conn, "cleanup_settings"));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
}

#[test]
fn migrate_v4_to_v5_preserves_existing_data_and_is_crash_idempotent() {
    let conn = v3_connection();
    migrate(&conn).unwrap();
    conn.execute(
        "INSERT INTO sessions
         (id, account_id, created_at, source, language, duration_sec, status, model,
          context_present, cleanup_status)
         VALUES ('old-v4', 'acct', 1, 'realtime', 'zh', 2.0, 'completed', 'old-model', 0,
                 'not_requested')",
        [],
    )
    .unwrap();

    conn.execute_batch("DROP TABLE dictionary_entries; PRAGMA user_version = 4;")
        .unwrap();
    migrate(&conn).unwrap();
    assert!(model::get_session(&conn, "old-v4").unwrap().is_some());
    assert!(table_exists(&conn, "dictionary_entries"));

    // 模拟 CREATE TABLE 已提交而版本号尚未推进。
    conn.execute_batch("PRAGMA user_version = 4;").unwrap();
    migrate(&conn).unwrap();
    assert!(table_exists(&conn, "dictionary_entries"));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
}

fn dictionary_entry(
    id: &str,
    term: &str,
    normalized_term: &str,
    source: &str,
    updated_at: i64,
) -> model::DictionaryEntryRow {
    model::DictionaryEntryRow {
        id: id.into(),
        term: term.into(),
        normalized_term: normalized_term.into(),
        source: source.into(),
        learning_event_id: (source == "learned").then(|| "event-1".into()),
        created_at: updated_at,
        updated_at,
    }
}

#[test]
fn dictionary_repository_enforces_uniqueness_search_and_keyset_order() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    for entry in [
        dictionary_entry("m-new", "100%_Real", "100%_real", "manual", 30),
        dictionary_entry("m-old", "Alpha", "alpha", "manual", 20),
        dictionary_entry("l-new", "Beta", "beta", "learned", 40),
        dictionary_entry("l-old", "Gamma", "gamma", "learned", 10),
    ] {
        model::insert_dictionary_entry(&conn, &entry).unwrap();
    }

    let duplicate = dictionary_entry("duplicate", "ALPHA", "alpha", "manual", 50);
    assert!(model::insert_dictionary_entry(&conn, &duplicate).is_err());

    let first = model::list_dictionary_entries(&conn, None, None, 2).unwrap();
    assert_eq!(
        first.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["m-new", "m-old"]
    );
    let cursor = model::DictionaryCursor {
        source_rank: 0,
        updated_at: first[1].updated_at,
        id: first[1].id.clone(),
    };
    let second = model::list_dictionary_entries(&conn, None, Some(&cursor), 2).unwrap();
    assert_eq!(
        second.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["l-new", "l-old"]
    );

    let escaped = model::list_dictionary_entries(&conn, Some("%_"), None, 10).unwrap();
    assert_eq!(
        escaped
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["m-new"]
    );
}

#[test]
fn dictionary_snapshot_orders_manual_before_learned() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    for entry in [
        dictionary_entry("learned-new", "Learned", "learned", "learned", 50),
        dictionary_entry("manual-old", "Manual", "manual", "manual", 10),
        dictionary_entry("manual-new", "Manual New", "manual new", "manual", 40),
    ] {
        model::insert_dictionary_entry(&conn, &entry).unwrap();
    }
    let rows = model::list_dictionary_entries_for_snapshot(&conn).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["manual-new", "manual-old", "learned-new"]
    );
}

#[test]
fn dictionary_repository_promotes_and_undoes_only_remaining_learned_rows() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    model::insert_dictionary_entry(
        &conn,
        &dictionary_entry("promoted", "SeaSnail", "seasnail", "learned", 1),
    )
    .unwrap();
    model::insert_dictionary_entry(
        &conn,
        &dictionary_entry("removed", "SenseVoice", "sensevoice", "learned", 1),
    )
    .unwrap();

    assert!(model::promote_dictionary_entry(&conn, "promoted", "SeaSnail", 2).unwrap());
    assert_eq!(
        model::delete_dictionary_learning_event(&conn, "event-1").unwrap(),
        1
    );
    assert_eq!(
        model::delete_dictionary_learning_event(&conn, "event-1").unwrap(),
        0
    );
    let promoted = model::get_dictionary_entry(&conn, "promoted")
        .unwrap()
        .unwrap();
    assert_eq!(promoted.source, "manual");
    assert_eq!(promoted.learning_event_id, None);
}

/// session CRUD 往返：插入→取回等值→list→删→取 None→二次删 false。
#[test]
fn session_crud_roundtrip() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("crud.db"), &key(0x60)).unwrap();
    let s = session("s1", "acct", 1000);
    model::insert_session(&conn, &s).unwrap();
    let got = model::get_session(&conn, "s1").unwrap().expect("应取到");
    assert_eq!(got, s, "取回行应与写入逐字段相等");
    assert_eq!(
        model::list_sessions(&conn, "acct", 10, None).unwrap().len(),
        1,
    );
    assert!(model::delete_session(&conn, "s1").unwrap(), "删应返 true");
    assert!(
        model::get_session(&conn, "s1").unwrap().is_none(),
        "删后取应 None"
    );
    assert!(
        !model::delete_session(&conn, "s1").unwrap(),
        "二次删应返 false",
    );
}

#[test]
fn checkpoint_and_complete_cleanup_enforce_expected_state_model_and_invariants() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("cas.db"), &key(0x62)).unwrap();

    for id in [
        "not-requested",
        "disabled",
        "processing",
        "failed",
        "wrong",
        "bad-cleanup",
        "deleted",
    ] {
        model::insert_session(&conn, &transcribing_session(id)).unwrap();
    }

    assert!(checkpoint_raw(
        &conn,
        "not-requested",
        "whisper-base",
        "n/transcript.pb.enc",
        1.0,
        RawCheckpointDecision::NotRequested,
    )
    .unwrap());

    assert!(checkpoint_raw(
        &conn,
        "disabled",
        "whisper-base",
        "d/transcript.pb.enc",
        1.5,
        RawCheckpointDecision::Disabled,
    )
    .unwrap());
    let disabled = model::get_session(&conn, "disabled").unwrap().unwrap();
    assert_eq!(
        (disabled.status.as_str(), disabled.cleanup_status.as_str()),
        ("completed", "disabled")
    );
    assert_eq!(disabled.cleanup_path, None);
    assert_eq!(disabled.cleanup_error_code, None);

    assert!(!checkpoint_raw(
        &conn,
        "wrong",
        "other-model",
        "w/transcript.pb.enc",
        1.0,
        RawCheckpointDecision::Processing,
    )
    .unwrap());
    assert_eq!(
        model::get_session(&conn, "wrong").unwrap().unwrap().status,
        "transcribing"
    );
    assert!(!complete_cleanup(
        &conn,
        "wrong",
        "whisper-base",
        CleanupCompletion::Succeeded {
            cleanup_path: "wrong/cleanup.pb.enc",
        },
    )
    .unwrap());
    conn.execute(
        "UPDATE sessions SET status='cleaning_up' WHERE id='bad-cleanup'",
        [],
    )
    .unwrap();
    assert!(!complete_cleanup(
        &conn,
        "bad-cleanup",
        "whisper-base",
        CleanupCompletion::Succeeded {
            cleanup_path: "bad/cleanup.pb.enc",
        },
    )
    .unwrap());

    assert!(checkpoint_raw(
        &conn,
        "processing",
        "whisper-base",
        "p/transcript.pb.enc",
        2.0,
        RawCheckpointDecision::Processing,
    )
    .unwrap());
    assert!(!complete_cleanup(
        &conn,
        "processing",
        "other-model",
        CleanupCompletion::Succeeded {
            cleanup_path: "p/cleanup.pb.enc"
        },
    )
    .unwrap());
    assert!(complete_cleanup(
        &conn,
        "processing",
        "whisper-base",
        CleanupCompletion::Succeeded {
            cleanup_path: "p/cleanup.pb.enc"
        },
    )
    .unwrap());
    let succeeded = model::get_session(&conn, "processing").unwrap().unwrap();
    assert_eq!(
        (succeeded.status.as_str(), succeeded.cleanup_status.as_str()),
        ("completed", "succeeded")
    );
    assert_eq!(succeeded.cleanup_path.as_deref(), Some("p/cleanup.pb.enc"));
    assert_eq!(succeeded.cleanup_error_code, None);

    assert!(checkpoint_raw(
        &conn,
        "failed",
        "whisper-base",
        "f/transcript.pb.enc",
        2.0,
        RawCheckpointDecision::Processing,
    )
    .unwrap());
    assert!(complete_cleanup(
        &conn,
        "failed",
        "whisper-base",
        CleanupCompletion::Failed {
            cleanup_path: None,
            error_code: CleanupFailureCode::Timeout,
        },
    )
    .unwrap());
    let failed = model::get_session(&conn, "failed").unwrap().unwrap();
    assert_eq!(
        (failed.status.as_str(), failed.cleanup_status.as_str()),
        ("completed", "failed")
    );
    assert_eq!(failed.cleanup_path, None);
    assert_eq!(
        failed.cleanup_error_code.as_deref(),
        Some("cleanup_timeout")
    );

    model::delete_session(&conn, "deleted").unwrap();
    assert!(!checkpoint_raw(
        &conn,
        "deleted",
        "whisper-base",
        "x/transcript.pb.enc",
        1.0,
        RawCheckpointDecision::NotRequested,
    )
    .unwrap());
}

#[test]
fn recover_cleanup_only_accepts_compatible_expected_state_and_model() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    for id in ["raw", "success", "corrupt", "wrong-model", "missing-raw"] {
        model::insert_session(&conn, &transcribing_session(id)).unwrap();
    }

    assert!(recover_interrupted_cleanup(
        &conn,
        "raw",
        "whisper-base",
        RecoveryExpectedState::TranscribingNotRequested,
        RecoveredCleanupOutcome::RawCheckpoint {
            transcript_path: "raw/transcript.pb.enc",
            duration_sec: 3.0,
        },
    )
    .unwrap());
    let raw = model::get_session(&conn, "raw").unwrap().unwrap();
    assert_eq!(
        (raw.status.as_str(), raw.cleanup_status.as_str()),
        ("completed", "not_requested")
    );

    for id in ["success", "corrupt", "wrong-model"] {
        assert!(checkpoint_raw(
            &conn,
            id,
            "whisper-base",
            "transcript.pb.enc",
            1.0,
            RawCheckpointDecision::Processing,
        )
        .unwrap());
    }
    assert!(recover_interrupted_cleanup(
        &conn,
        "success",
        "whisper-base",
        RecoveryExpectedState::CleaningUpProcessing,
        RecoveredCleanupOutcome::Succeeded {
            cleanup_path: "cleanup.pb.enc"
        },
    )
    .unwrap());
    assert!(complete_cleanup(
        &conn,
        "corrupt",
        "whisper-base",
        CleanupCompletion::Succeeded {
            cleanup_path: "cleanup.pb.enc"
        },
    )
    .unwrap());
    assert!(recover_interrupted_cleanup(
        &conn,
        "corrupt",
        "whisper-base",
        RecoveryExpectedState::CompletedSucceeded,
        RecoveredCleanupOutcome::Failed {
            cleanup_path: None,
            error_code: CleanupFailureCode::ArtifactInvalid,
        },
    )
    .unwrap());
    assert!(!recover_interrupted_cleanup(
        &conn,
        "wrong-model",
        "other-model",
        RecoveryExpectedState::CleaningUpProcessing,
        RecoveredCleanupOutcome::Failed {
            cleanup_path: None,
            error_code: CleanupFailureCode::Interrupted,
        },
    )
    .unwrap());
    assert!(recover_interrupted_cleanup(
        &conn,
        "wrong-model",
        "whisper-base",
        RecoveryExpectedState::CompletedSucceeded,
        RecoveredCleanupOutcome::RawCheckpoint {
            transcript_path: "wrong/transcript.pb.enc",
            duration_sec: 1.0,
        },
    )
    .is_err());

    conn.execute(
        "UPDATE sessions SET status='cleaning_up', cleanup_status='processing'
         WHERE id='missing-raw'",
        [],
    )
    .unwrap();
    assert!(!recover_interrupted_cleanup(
        &conn,
        "missing-raw",
        "whisper-base",
        RecoveryExpectedState::CleaningUpProcessing,
        RecoveredCleanupOutcome::Failed {
            cleanup_path: None,
            error_code: CleanupFailureCode::Interrupted,
        },
    )
    .unwrap());
}

#[test]
fn fail_transcription_requires_current_model_and_transcribing_state() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    model::insert_session(&conn, &transcribing_session("failure-race")).unwrap();

    assert!(!fail_transcription(&conn, "failure-race", "other-model", "late").unwrap());
    assert!(fail_transcription(&conn, "failure-race", "whisper-base", "backend failed").unwrap());
    let row = model::get_session(&conn, "failure-race").unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("backend failed"));
    assert!(!fail_transcription(&conn, "failure-race", "whisper-base", "late").unwrap());
}

#[test]
fn concurrent_cleanup_completion_allows_exactly_one_winner() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("concurrent-cas.db");
    let raw_key = key(0x63);
    {
        let conn = open_db(&path, &raw_key).unwrap();
        model::insert_session(&conn, &transcribing_session("race")).unwrap();
        checkpoint_raw(
            &conn,
            "race",
            "whisper-base",
            "transcript.pb.enc",
            1.0,
            RawCheckpointDecision::Processing,
        )
        .unwrap();
    }

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["a/cleanup.pb.enc", "b/cleanup.pb.enc"]
        .into_iter()
        .map(|cleanup_path| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let conn = open_db(&path, &raw_key).unwrap();
                conn.busy_timeout(Duration::from_secs(2)).unwrap();
                barrier.wait();
                complete_cleanup(
                    &conn,
                    "race",
                    "whisper-base",
                    CleanupCompletion::Succeeded { cleanup_path },
                )
                .unwrap()
            })
        })
        .collect();
    let wins = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
}

#[test]
fn provider_repository_normalizes_validates_and_cruds_without_secrets() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    let created = create_provider_config(
        &conn,
        ProviderConfigInput {
            name: "Primary",
            provider_type: ProviderType::SelfHostedPrivate,
            endpoint: "HTTP://Example.COM:80/chat/completions/",
            model: "manual-model",
        },
        100,
    )
    .unwrap();
    assert_eq!(created.endpoint, "http://example.com/v1");
    assert_eq!(
        created.provider_type,
        "openai_compatible_self_hosted_private"
    );
    assert_eq!(created.endpoint_fingerprint.len(), 64);
    assert_eq!(
        uuid::Uuid::parse_str(&created.id).unwrap().to_string(),
        created.id
    );

    let old_fingerprint = created.endpoint_fingerprint.clone();
    let updated = replace_provider_config(
        &conn,
        &created.id,
        ProviderConfigInput {
            name: "Primary 2",
            provider_type: ProviderType::SelfHostedPrivate,
            endpoint: "https://example.com/base/models",
            model: "other-model",
        },
        200,
    )
    .unwrap()
    .unwrap();
    assert_eq!(updated.created_at, 100);
    assert_eq!(updated.updated_at, 200);
    assert_eq!(updated.endpoint, "https://example.com/base");
    assert_ne!(updated.endpoint_fingerprint, old_fingerprint);
    assert_eq!(list_provider_configs(&conn).unwrap(), vec![updated]);
    assert!(remove_provider_config_and_disable(&conn, &created.id, 300).unwrap());
    assert!(!remove_provider_config_and_disable(&conn, &created.id, 301).unwrap());

    assert!(
        normalize_provider_endpoint(ProviderType::SelfHostedPublic, "http://example.com/v1")
            .is_err()
    );
    assert!(normalize_provider_endpoint(ProviderType::OpenAi, "https://evil.example/v1").is_err());
    assert!(normalize_provider_endpoint(
        ProviderType::SelfHostedPrivate,
        "https://user@example.com/v1"
    )
    .is_err());
}

#[test]
fn provider_repository_enforces_bounds_limit_sorting_and_db_isolation() {
    let first = Connection::open_in_memory().unwrap();
    let second = Connection::open_in_memory().unwrap();
    migrate(&first).unwrap();
    migrate(&second).unwrap();
    let draft = ProviderConfigInput {
        name: "P",
        provider_type: ProviderType::OpenAi,
        endpoint: "https://api.openai.com/v1/chat/completions",
        model: "gpt-model",
    };
    for index in 0..32 {
        create_provider_config(&first, draft, index).unwrap();
    }
    assert!(create_provider_config(&first, draft, 33).is_err());
    let rows = list_provider_configs(&first).unwrap();
    assert_eq!(rows.len(), 32);
    assert!(rows.windows(2).all(|pair| {
        (pair[0].created_at, pair[0].id.as_str()) <= (pair[1].created_at, pair[1].id.as_str())
    }));
    assert!(list_provider_configs(&second).unwrap().is_empty());

    assert!(create_provider_config(
        &second,
        ProviderConfigInput {
            name: " bad",
            ..draft
        },
        1,
    )
    .is_err());
    let oversized_model = "m".repeat(257);
    assert!(create_provider_config(
        &second,
        ProviderConfigInput {
            model: &oversized_model,
            ..draft
        },
        1,
    )
    .is_err());
}

/// 游标分页：3 条递增 created_at，limit 2 取最新两条；以末尾为游标取最旧一条。
#[test]
fn session_cursor_pagination() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("page.db"), &key(0x30)).unwrap();
    model::insert_session(&conn, &session("old", "acct", 1000)).unwrap();
    model::insert_session(&conn, &session("mid", "acct", 2000)).unwrap();
    model::insert_session(&conn, &session("new", "acct", 3000)).unwrap();

    let p1 = model::list_sessions(&conn, "acct", 2, None).unwrap();
    assert_eq!(p1.len(), 2);
    assert_eq!(p1[0].id, "new", "首条应最新");
    assert_eq!(p1[1].id, "mid");

    let p2 = model::list_sessions(&conn, "acct", 2, Some(2000)).unwrap();
    assert_eq!(p2.len(), 1, "游标后只剩最旧一条");
    assert_eq!(p2[0].id, "old");
}

/// token CRUD + by_hash 查 + last_used 触达。
#[test]
fn token_crud_roundtrip() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("tok.db"), &key(0x40)).unwrap();
    let t = token("t1", "acct", "hash-aaa");
    model::insert_token(&conn, &t).unwrap();

    let got = model::get_token(&conn, "t1")
        .unwrap()
        .expect("by id 应取到");
    assert_eq!(got.token_hash, "hash-aaa");
    assert_eq!(got.scopes, vec!["sessions:read".to_string()]);
    assert!(!got.is_root);

    let by_hash = model::get_token_by_hash(&conn, "hash-aaa")
        .unwrap()
        .expect("by hash 应取到");
    assert_eq!(by_hash.id, "t1");
    assert_eq!(model::list_tokens(&conn, "acct").unwrap().len(), 1);

    model::touch_token_last_used(&conn, "t1", 9999).unwrap();
    let got2 = model::get_token(&conn, "t1").unwrap().unwrap();
    assert_eq!(got2.last_used_at, Some(9999));

    assert!(model::delete_token(&conn, "t1").unwrap());
    assert!(model::get_token(&conn, "t1").unwrap().is_none());
}

/// token_hash 唯一索引：同 hash 二插被拒。
#[test]
fn token_hash_unique_constraint() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("uniq.db"), &key(0x50)).unwrap();
    model::insert_token(&conn, &token("t1", "acct", "samehash")).unwrap();
    let res = model::insert_token(&conn, &token("t2", "acct", "samehash"));
    assert!(res.is_err(), "同 token_hash 应被唯一索引拒绝");
}

/// 错钥打不开既有库：WrongKey。
#[test]
fn wrong_key_cannot_open_existing_db() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.db");
    let k = key(0x01);
    {
        let conn = open_db(&path, &k).unwrap();
        model::insert_session(&conn, &session("s1", "acct", 1000)).unwrap();
    }
    let err = open_db(&path, &key(0x02)).unwrap_err();
    assert!(
        matches!(err, Error::WrongKey),
        "错钥应返 WrongKey，实际 {err:?}"
    );
}

/// 正确钥重开库 → 历史数据仍在。
#[test]
fn reopen_with_correct_key_sees_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ok.db");
    let k = key(0x07);
    {
        let conn = open_db(&path, &k).unwrap();
        model::insert_session(&conn, &session("s1", "acct", 1000)).unwrap();
    }
    let conn = open_db(&path, &k).unwrap();
    let got = model::get_session(&conn, "s1")
        .unwrap()
        .expect("重开应取到历史行");
    assert_eq!(got.account_id, "acct");
    assert_eq!(got.status, "completed");
    assert_eq!(got.audio_path.as_deref(), Some("2026-08-10/s1/audio.enc"));
}

/// per-account 隔离：两库两钥，互不可见、互不可开。
#[test]
fn per_account_isolation() {
    let dir = tempdir().unwrap();
    let p1 = dir.path().join("acct1.db");
    let p2 = dir.path().join("acct2.db");
    let k1 = key(0x10);
    let k2 = key(0x20);
    {
        let c1 = open_db(&p1, &k1).unwrap();
        let c2 = open_db(&p2, &k2).unwrap();
        model::insert_session(&c1, &session("a1-s1", "acct1", 1000)).unwrap();
        model::insert_session(&c2, &session("a2-s1", "acct2", 2000)).unwrap();
        assert_eq!(
            model::list_sessions(&c1, "acct1", 10, None).unwrap().len(),
            1
        );
        assert_eq!(
            model::list_sessions(&c2, "acct2", 10, None).unwrap().len(),
            1
        );
        // acct1 连接查 acct2 account_id → 0 行（账户维度过滤）
        assert!(model::list_sessions(&c1, "acct2", 10, None)
            .unwrap()
            .is_empty());
    }
    // acct2 的钥打不开 acct1 的库
    let err = open_db(&p1, &k2).unwrap_err();
    assert!(
        matches!(err, Error::WrongKey),
        "跨账户钥不应打开他库，实际 {err:?}"
    );
}
