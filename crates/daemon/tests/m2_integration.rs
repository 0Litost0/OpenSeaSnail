//! ST-M2.8 集成验证：per-account 隔离端到端（mock 音频，无 ASR）。
//!
//! 跨 Crypto / Auth / Storage 三层，在**同一共享态**（`Arc<Crypto>` +
//! `MemoryKeychain` + `tempfile`）下串联完整账户生命周期，验证各层拼装后的
//! 端到端行为。覆盖路线图 ST-M2.8 验收八点：
//!
//! 1. 建首账户  2. 免密重开  3. 追加/切换 unlock  4. 改密
//! 5. 第三方 token 绑 account_id  6. 跨账户 403  7. 未解锁 423  8. reconcile 兜底
//!
//! 与既有测试正交、不重复：
//! - `tests/auth_endpoints.rs`：HTTP 面（提取器/handler/路由/错误码映射），12 测。
//! - `crates/storage/tests/storage.rs`：数据面单账户（AEAD 往返/reconcile/分页），9 测。
//! - 本测试：跨层、多账户、共享态下的端到端 per-account 隔离。
//!
//! 编排层断言 `AccountError::CrossAccount`/`NotUnlocked` 等价 HTTP 403/423（映射
//! 已在 `auth_endpoints.rs` 验证）；数据面经 `Storage` API 直测（sessions HTTP
//! 端点属 M5）。每个 `{ ... }` 块重建 `Crypto` 实例，模拟守护进程重启——状态
//! 仅靠盘（registry/wrapped-dek/文件树）+ keychain 持久化，不依赖进程内存。

use std::sync::Arc;

use seasnail_crypto::{decrypt_file, Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::account::{check_target_account, AccountError, ReconcileReport, Storage};
use seasnail_daemon::{Auth, Crypto};
use seasnail_proto::seasnail::v1::{
    Source, Speaker, TranscriptFile, TranscriptUnit, UnitGranularity,
};
use seasnail_storage::SessionRow;
use uuid::Uuid;

/// 测试用小 Argon2 参数（快）；非生产 DEFAULT。
fn fast_params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// 由同一 data_dir + keychain 构造 Crypto + Auth + Storage（共享 `Arc<Crypto>`）。
/// 多次调用模拟「重启」：registry 从盘载、活跃账户从 keychain 恢复。
fn build_stack(
    dir: &tempfile::TempDir,
    kc: Arc<dyn KeychainStore>,
) -> (Arc<Crypto>, Auth, Storage) {
    let crypto = Arc::new(Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap());
    let auth = Auth::new(crypto.clone());
    let storage = Storage::new(crypto.clone());
    (crypto, auth, storage)
}

/// 构造一个最小 TranscriptFile（mock 转译内容）。
fn sample_transcript(session_id: &str, account_id: &str, full: &str) -> TranscriptFile {
    TranscriptFile {
        schema_version: 2,
        session_id: session_id.to_string(),
        account_id: account_id.to_string(),
        created_at_ms: 1_700_000_000_000,
        model: "whisper".into(),
        language: "zh".into(),
        source: Source::Realtime as i32,
        duration_ms: 5000,
        speakers: vec![Speaker {
            id: "A".into(),
            label: "说话人 A".into(),
        }],
        units: vec![TranscriptUnit {
            sequence: 0,
            speaker: "A".into(),
            start_ms: Some(0),
            end_ms: Some(1000),
            text: full.to_string(),
            confidence: Some(0.9),
            granularity: UnitGranularity::Segment as i32,
        }],
        full_text: full.to_string(),
    }
}

/// 构造一个 sessions 行（mock 音频/转译已落盘，相对路径由调用方给）。
fn new_session_row(
    id: &str,
    account_id: &str,
    created_at: i64,
    audio: &str,
    trans: &str,
) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        account_id: account_id.to_string(),
        created_at,
        source: "realtime".into(),
        language: "zh".into(),
        duration_sec: 5.0,
        status: "completed".into(),
        model: "whisper".into(),
        input_device: Some("mic".into()),
        file_name: None,
        audio_path: Some(audio.to_string()),
        transcript_path: Some(trans.to_string()),
        failure_reason: None,
        context_present: false,
        cleanup_status: "not_requested".into(),
        cleanup_path: None,
        cleanup_error_code: None,
    }
}

/// 写一个完整会话（mock 音频 + 转译 + 元数据行）。返回 (audio_rel, trans_rel)。
/// 「mock 音频」= 任意字节，不经 ffmpeg / ASR，仅验加密落盘往返。
fn write_session(
    storage: &Storage,
    sid: &str,
    account_id: &str,
    created_at: i64,
    full: &str,
) -> (String, String) {
    let audio_rel = storage
        .write_audio(sid, created_at, b"fake-audio-pcm-bytes")
        .unwrap();
    let trans_rel = storage
        .write_transcript(sid, created_at, &sample_transcript(sid, account_id, full))
        .unwrap();
    storage
        .insert_session(&new_session_row(
            sid, account_id, created_at, &audio_rel, &trans_rel,
        ))
        .unwrap();
    (audio_rel, trans_rel)
}

// ──────────────────────────────────────────────────────────────────────────────
// ST-M2.8 验收点 1–7 + per-account 数据隔离
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn per_account_lifecycle_and_isolation() {
    let dir = tmpdir();
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;

    // 跨块传递的 alice 侧产物（块末 drop Crypto 实例，仅字符串/盘上文件留存）。
    let alice_id: String;
    let alice_root_bearer: String;
    let alice_audio_rel: String;
    let bob_id: String;
    let third_party_bearer: String;

    // ── 1. 建首账户 alice + 写 mock 会话 ──────────────────────────────────────
    {
        let (crypto, _auth, storage) = build_stack(&dir, kc.clone());
        assert!(!crypto.is_initialized());
        let t = crypto.setup_first_account("alice", "p1").unwrap();
        assert!(crypto.is_initialized());
        assert!(t.secret_bearer.starts_with("ss_live_"));
        alice_id = t.account_id.clone();
        alice_root_bearer = t.secret_bearer.clone();

        let sid = Uuid::new_v4().to_string();
        let (a, tr) = write_session(&storage, &sid, &alice_id, 1_700_000_000, "你好世界");
        alice_audio_rel = a;
        // 读回：alice 活跃，DEK 在内存。
        assert_eq!(
            storage.read_audio(&alice_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );
        assert_eq!(storage.read_transcript(&tr).unwrap().full_text, "你好世界");
    }

    // ── 2. 免密重开：新实例从 registry + keychain 恢复活跃，无需密码 ──────────
    {
        let (crypto, _auth, storage) = build_stack(&dir, kc.clone());
        assert!(crypto.is_initialized());
        assert_eq!(crypto.active_account_id(), Some(alice_id.clone()));
        assert!(
            crypto.active_root_token_bearer().unwrap().is_some(),
            "免密恢复 root bearer"
        );
        // 重开后历史数据仍可读（K_files 已从 keychain DEK 派生）。
        assert_eq!(
            storage.read_audio(&alice_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );
    }

    // ── 3. 追加账户 bob：活跃切到 bob，alice 非活跃 ────────────────────────────
    {
        let (crypto, auth, storage) = build_stack(&dir, kc.clone());
        let t_bob = crypto.create_account("bob", "p2").unwrap();
        bob_id = t_bob.account_id.clone();
        assert_eq!(crypto.active_account_id(), Some(bob_id.clone()));

        let bob_sid = Uuid::new_v4().to_string();
        let (bob_audio_rel, _) =
            write_session(&storage, &bob_sid, &bob_id, 1_700_000_001, "bob 的录音");
        assert_eq!(
            storage.read_audio(&bob_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );

        // alice 现在非活跃（DEK 已移出 keychain）→ verify 返 NotUnlocked（423）。
        assert!(matches!(
            auth.verify(&alice_root_bearer),
            Err(AccountError::NotUnlocked)
        ));
    }

    // ── 4. 切换 unlock 回 alice：密码 → DEK 入 keychain 作活跃 ────────────────
    {
        let (crypto, auth, storage) = build_stack(&dir, kc.clone());
        let t_alice2 = crypto.unlock_account(&alice_id, "p1").unwrap();
        assert_eq!(crypto.active_account_id(), Some(alice_id.clone()));
        // 新签发的 root token（新 secret）。
        assert_ne!(t_alice2.secret_bearer, alice_root_bearer);
        // alice 历史数据仍可读（DEK 恢复，K_files 不变）。
        assert_eq!(
            storage.read_audio(&alice_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );
        // 旧 root bearer 仍有效（unlock 不吊销旧 root，设计已定）。
        assert!(auth.verify(&alice_root_bearer).is_ok());
    }

    // ── 5. 改密：DEK 不变、历史可读；旧密码失效、新密码可用 ──────────────────
    {
        let (crypto, auth, storage) = build_stack(&dir, kc.clone());
        crypto.change_password(&alice_id, "p1", "p_new").unwrap();
        // 旧密码失效。
        assert!(matches!(
            crypto.verify_password(&alice_id, "p1"),
            Err(AccountError::WrongPassword)
        ));
        // 新密码可用。
        assert!(crypto.verify_password(&alice_id, "p_new").is_ok());
        // 历史数据仍可读（master_DEK / K_files 不变，改密只重 wrap）。
        assert_eq!(
            storage.read_audio(&alice_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );
        // 改密后 root token 仍有效（token secret 独立于密码）。
        assert!(
            auth.verify(&alice_root_bearer).is_ok(),
            "改密后 token 仍有效"
        );
    }

    // ── 6. 第三方 token 绑 account_id + 跨账户 403 ─────────────────────────────
    {
        let (crypto, auth, _storage) = build_stack(&dir, kc.clone());
        assert_eq!(crypto.active_account_id(), Some(alice_id.clone()));
        let caller_root = auth.verify(&alice_root_bearer).unwrap();
        assert!(caller_root.is_root);

        // 签发第三方 token（非 root、绑 alice_id、仅 read scope）。
        let t = auth
            .issue_token(&caller_root, "ci", vec!["sessions:read".into()])
            .unwrap();
        assert!(!t.is_root);
        assert_eq!(t.account_id, alice_id);
        third_party_bearer = t.secret_bearer.clone();

        // 第三方 token 可 verify（alice 活跃）。
        let caller_third = auth.verify(&third_party_bearer).unwrap();
        assert!(!caller_third.is_root);
        assert!(caller_third.scopes.contains("sessions:read"));
        assert!(!caller_third.scopes.contains("sessions:write"));

        // 跨账户：alice 的 token 指向 bob → CrossAccount（→403）。
        assert!(matches!(
            check_target_account(&caller_third, &bob_id),
            Err(AccountError::CrossAccount(_))
        ));
        // 同账户 → Ok。
        assert!(check_target_account(&caller_third, &alice_id).is_ok());
    }

    // ── 7. 切到 bob：alice 非活跃 → 未解锁 423 + per-account 数据隔离 ──────────
    {
        let (crypto, auth, storage) = build_stack(&dir, kc.clone());
        crypto.unlock_account(&bob_id, "p2").unwrap();
        assert_eq!(crypto.active_account_id(), Some(bob_id.clone()));

        // alice 的第三方 token verify → NotUnlocked（423）：token 有效但账户锁定。
        assert!(matches!(
            auth.verify(&third_party_bearer),
            Err(AccountError::NotUnlocked)
        ));
        // require_unlocked(alice) → NotUnlocked（423）。
        assert!(matches!(
            auth.require_unlocked(&alice_id),
            Err(AccountError::NotUnlocked)
        ));

        // per-account 数据隔离（bob 活跃时）：
        // (a) 路径隔离：alice 的音频文件不应出现在 bob 的文件树下；read_audio
        //     走 bob 树按 rel 找不到文件。
        assert!(
            !dir.path()
                .join("data")
                .join(&bob_id)
                .join(&alice_audio_rel)
                .exists(),
            "alice 的音频文件不应出现在 bob 的文件树下（路径隔离）"
        );
        assert!(storage.read_audio(&alice_audio_rel).is_err());
        // (b) 密钥隔离（defense-in-depth）：bob 的 K_files 解 alice 密文 → AEAD 认证失败。
        let alice_enc_path = dir
            .path()
            .join("data")
            .join(&alice_id)
            .join(&alice_audio_rel);
        let alice_ct = std::fs::read(&alice_enc_path).expect("alice 密文应存在");
        let bob_k_files = crypto.active_k_files().unwrap();
        assert!(
            decrypt_file(&bob_k_files, &alice_ct).is_err(),
            "bob 的 K_files 不可解 alice 密文（密钥隔离）"
        );

        // 切回 alice（密码已是 p_new）→ 又能读自己的数据（DEK 回到 keychain/内存）。
        crypto.unlock_account(&alice_id, "p_new").unwrap();
        assert_eq!(
            storage.read_audio(&alice_audio_rel).unwrap(),
            b"fake-audio-pcm-bytes"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ST-M2.8 验收点 8：reconcile 兜底
// ──────────────────────────────────────────────────────────────────────────────

/// reconcile 在集成栈下双向清理孤儿：孤儿 DB 行（文件缺失）+ 孤儿文件目录（无行），
/// 一致会话不动，幂等。
#[test]
fn reconcile_fallback_integrated() {
    let dir = tmpdir();
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let (crypto, _auth, storage) = build_stack(&dir, kc);
    let t = crypto.setup_first_account("alice", "p").unwrap();
    let acct = t.account_id;
    let created = 1_700_000_000;

    // 一致会话：audio + transcript + 行 齐全。
    let sid_ok = Uuid::new_v4().to_string();
    let _ = write_session(&storage, &sid_ok, &acct, created, "一致");

    // 孤儿文件：写 audio 但不 INSERT（模拟写文件后崩溃，未到 DB 提交点）。
    let sid_orphan_file = Uuid::new_v4().to_string();
    let _ = storage
        .write_audio(&sid_orphan_file, created + 1, b"orphan")
        .unwrap();

    // 孤儿行：INSERT 一行指向不存在的文件路径（模拟 DB 行在、文件被外部删）。
    let sid_orphan_row = Uuid::new_v4().to_string();
    let ghost_audio = format!("2099-01-01/{sid_orphan_row}/audio.enc");
    let ghost_trans = format!("2099-01-01/{sid_orphan_row}/transcript.pb.enc");
    storage
        .insert_session(&new_session_row(
            &sid_orphan_row,
            &acct,
            created + 2,
            &ghost_audio,
            &ghost_trans,
        ))
        .unwrap();

    let report = storage.reconcile().unwrap();
    assert_eq!(report.orphan_rows_removed, 1, "删 1 孤儿行（文件缺失）");
    assert_eq!(report.orphan_dirs_removed, 1, "删 1 孤儿文件目录（无行）");
    // 一致会话仍在。
    assert!(storage.get(&sid_ok).unwrap().is_some(), "一致会话不应被清");
    // 孤儿行已删。
    assert!(
        storage.get(&sid_orphan_row).unwrap().is_none(),
        "孤儿行应已删"
    );
    // 幂等：再次 reconcile 零清理。
    let report2 = storage.reconcile().unwrap();
    assert_eq!(
        report2,
        ReconcileReport::default(),
        "二次 reconcile 应零清理"
    );
}
