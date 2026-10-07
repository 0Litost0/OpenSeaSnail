//! SQLCipher raw key 开库 + schema migrate（ST-M2.2）。
//!
//! raw key 经 `PRAGMA key = "x'<64 hex>'"` 喂入，绕过 SQLCipher 内置 PBKDF2——
//! 喂的是 HKDF 派生的 K_sqlite[acct]（32B），故 KDF 不再参与。开库后立即读
//! `sqlite_master` 校验密钥：错钥 / 非加密库 → WrongKey；新建空库读得 0 行不误报。

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// 存储层错误。SQLCipher 操作错误统一经 `Sqlite`；开库密钥校验失败映射 `WrongKey`
///（供 ST-M2.4 unlock 流程区分"密码错"与"库损坏"）。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// rusqlite / SQLCipher 操作错误。
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// 密钥错误或库非 SeaSnail 加密库（读 sqlite_master 失败）。
    #[error("wrong key or not a valid seasnail encrypted database")]
    WrongKey,
    /// schema 迁移失败。
    #[error("migration failed: {0}")]
    Migrate(String),
    /// 调用方提供的状态迁移参数无法满足 session/cleanup 组合不变量。
    #[error("invalid session mutation: {0}")]
    InvalidMutation(String),
}

/// 32B raw key → 64 hex（小写）。
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// 以 raw key 打开 / 创建 per-account SQLCipher 库并跑迁移。
///
/// `raw_key` 为 HKDF 派生的 K_sqlite[acct]（32B）。PRAGMA key 须为首个触碰库的操作，
/// 故 `Connection::open` 后立即设。设毕读 `sqlite_master` 校验：错钥下 SQLCipher
/// 解 page 1 失败、报 "file is not a database" → 映射 `WrongKey`。新建空库读到 0 行
/// 不误报。校验通过后跑 `migrate`。
pub fn open_db(path: &Path, raw_key: &[u8; 32]) -> Result<Connection, Error> {
    let conn = Connection::open(path)?;
    let hex = hex_encode(raw_key);
    // x'<hex>' 为 SQLCipher blob 字面量，hex 来自本机随机钥、仅 0-9a-f，无注入风险。
    let pragma = format!("PRAGMA key = \"x'{}'\";", hex);
    conn.execute_batch(&pragma)?;
    // 校验密钥：错钥下读 sqlite_master 报错 → WrongKey。
    let _check: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .map_err(|_| Error::WrongKey)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrate(&conn)?;
    Ok(conn)
}

/// 创建 sessions / tokens 表 + 索引（幂等）。per-account 库结构一致，每账户库各跑一次。
///
/// MVP 手写迁移（schema 简单），refinery 后置。`PRAGMA user_version` 记迁移版本，
/// 按 user_version 分支前向迁移（仅加、不改、不删列）。列类型与 OpenAPI 契约对齐：
/// source(realtime|imported)、language(zh|en|mixed)、
/// status(transcribing|cleaning_up|completed|failed)。
///
/// - **v1**：sessions（12 列）+ tokens 基础 schema。
/// - **v2**（ST-M3.7 状态机）：`sessions.failure_reason TEXT`（failed 行记因，可空）。
/// - **v3**：`sessions.context_present INTEGER NOT NULL DEFAULT 0`（旧会话无 context）。
/// - **v4**：sessions cleanup 状态/路径/错误 + provider config / cleanup settings 表。
/// - **v5**：账户级 Dictionary 词条表与稳定排序索引。
///
/// 新库（user_version=0）依次跑 v1→v5；既有库按序补列/表。所有 ALTER 都先做列
/// 存在性探测，使“ALTER 已提交但 user_version 未推进”的崩溃窗口可重复恢复。
pub fn migrate(conn: &Connection) -> Result<(), Error> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);

    if current < 1 {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id              TEXT    PRIMARY KEY NOT NULL,
                account_id      TEXT    NOT NULL,
                created_at      INTEGER NOT NULL,            -- epoch seconds UTC；游标分页键
                source          TEXT    NOT NULL,           -- realtime | imported
                language        TEXT    NOT NULL,           -- zh | en | mixed
                duration_sec    REAL    NOT NULL,
                status          TEXT    NOT NULL,           -- transcribing | completed | failed
                model           TEXT    NOT NULL,
                input_device    TEXT,
                file_name       TEXT,
                audio_path      TEXT,                       -- 文件树相对路径
                transcript_path TEXT                        -- 文件树相对路径
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_account_created
                ON sessions (account_id, created_at DESC, id);

            CREATE TABLE IF NOT EXISTS tokens (
                id            TEXT    PRIMARY KEY NOT NULL,
                account_id    TEXT    NOT NULL,
                name          TEXT    NOT NULL,
                prefix        TEXT    NOT NULL,             -- 展示前缀，ss_live_abcd…
                token_hash    TEXT    NOT NULL,             -- SHA-256(secret)，verify 用
                is_root       INTEGER NOT NULL DEFAULT 0,  -- 0/1
                scopes        TEXT    NOT NULL,             -- 空格分隔 scope 列表
                created_at    INTEGER NOT NULL,
                last_used_at  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_tokens_account ON tokens (account_id);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tokens_hash ON tokens (token_hash);

            PRAGMA user_version = 1;
            "#,
        )
        .map_err(|e| Error::Migrate(e.to_string()))?;
    }

    if current < 2 {
        // ST-M3.7 状态机：failed 行记 failure_reason（可空）。
        // 探测列是否已存在再 ALTER——SQLite 无 `ADD COLUMN IF NOT EXISTS`，靠
        // `pragma_table_info` 探测使 v2 **崩溃幂等**：若上次 ALTER 已提交但
        // `PRAGMA user_version=2` 未及提交（进程被杀/掉电），下次开库 current 仍 1，
        // 探测到列已存在则跳过 ALTER，仅推进 user_version=2，避免「duplicate column
        // name」致账户永久锁死。`unwrap_or(0)` 把 PRAGMA 读取异常也安全归零（v1 块
        // 的 `IF NOT EXISTS` 幂等，v2 块靠探测幂等）。
        let has_col: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('sessions') WHERE name='failure_reason'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Error::Migrate(e.to_string()))?;
        if has_col == 0 {
            conn.execute_batch("ALTER TABLE sessions ADD COLUMN failure_reason TEXT;")
                .map_err(|e| Error::Migrate(e.to_string()))?;
        }
        conn.execute_batch("PRAGMA user_version = 2;")
            .map_err(|e| Error::Migrate(e.to_string()))?;
    }

    if current < 3 {
        let has_col: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('sessions') WHERE name='context_present'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Error::Migrate(e.to_string()))?;
        if has_col == 0 {
            conn.execute_batch(
                "ALTER TABLE sessions ADD COLUMN context_present INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|e| Error::Migrate(e.to_string()))?;
        }
        conn.execute_batch("PRAGMA user_version = 3;")
            .map_err(|e| Error::Migrate(e.to_string()))?;
    }

    if current < 4 {
        let columns = [
            (
                "cleanup_status",
                "ALTER TABLE sessions ADD COLUMN cleanup_status TEXT NOT NULL DEFAULT 'not_requested';",
            ),
            (
                "cleanup_path",
                "ALTER TABLE sessions ADD COLUMN cleanup_path TEXT;",
            ),
            (
                "cleanup_error_code",
                "ALTER TABLE sessions ADD COLUMN cleanup_error_code TEXT;",
            ),
        ];
        for (name, alter) in columns {
            let has_col: i64 = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('sessions') WHERE name=?1",
                    [name],
                    |r| r.get(0),
                )
                .map_err(|e| Error::Migrate(e.to_string()))?;
            if has_col == 0 {
                conn.execute_batch(alter)
                    .map_err(|e| Error::Migrate(e.to_string()))?;
            }
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS reasoning_provider_configs (
                id                   TEXT PRIMARY KEY NOT NULL,
                name                 TEXT NOT NULL,
                provider_type        TEXT NOT NULL,
                endpoint             TEXT NOT NULL,
                endpoint_fingerprint TEXT NOT NULL,
                model                TEXT NOT NULL,
                created_at           INTEGER NOT NULL,
                updated_at           INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS cleanup_settings (
                singleton                   INTEGER PRIMARY KEY CHECK (singleton = 1),
                enabled                     INTEGER NOT NULL DEFAULT 0,
                selected_provider_config_id TEXT,
                custom_prompt               TEXT,
                updated_at                  INTEGER NOT NULL
            );

            PRAGMA user_version = 4;
            "#,
        )
        .map_err(|e| Error::Migrate(e.to_string()))?;
    }

    if current < 5 {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS dictionary_entries (
                id                TEXT    PRIMARY KEY NOT NULL,
                term              TEXT    NOT NULL,
                normalized_term   TEXT    NOT NULL,
                source            TEXT    NOT NULL,
                learning_event_id TEXT,
                created_at        INTEGER NOT NULL,
                updated_at        INTEGER NOT NULL,
                CHECK (source IN ('manual', 'learned')),
                UNIQUE (normalized_term)
            );

            CREATE INDEX IF NOT EXISTS idx_dictionary_updated
                ON dictionary_entries (source, updated_at DESC, id);

            PRAGMA user_version = 5;
            "#,
        )
        .map_err(|e| Error::Migrate(e.to_string()))?;
    }

    Ok(())
}
