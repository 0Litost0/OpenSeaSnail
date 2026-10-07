//! root token 生成与解析（ST-M2.4）。
//!
//! 设计见 `doc/architecture.md#accounts-and-storage`「鉴权与会话模块」边界 case。token
//! bearer 自描述内嵌 `account_id`，使 verify 在**不开任何 per-account 库**下即可
//! 区分 401（无效）/ 423（未解锁）/ 403（跨账户）。
//!
//! 格式（实现细节、不进 OpenAPI 契约；前端/第三方只见 `ss_live_xxx`）：
//! ```text
//! bearer = "ss_live_" + base64url( account_id[16] ‖ secret[32] )
//!        = "ss_live_" + 64 字符（48B → base64url 无填充）
//! ```
//! - `account_id` = UUID v4 的 16 原始字节（即 registry 的 account_id）。
//! - `secret` = 32B `OsRng` 随机。
//! - `prefix` 展示字段取 **secret 部分**前缀（`ss_live_` + base64url(secret) 前 8 字符），
//!   不含 account_id。
//! - DB 存 `token_hash = SHA-256(secret)`（hex），verify 比对此哈希；明文 secret 仅创建时返回一次。
//!
//! root token（`is_root=true`）持全 scope；第三方 token 绑 `account_id`、scope 受
//! `ScopeGuard` 硬约束（ST-M2.5）。本模块仅生成/解析，不校验授权。

use base64::Engine;
use sha2::Digest;
use uuid::Uuid;

use super::error::AccountError;

/// token 前缀（公开 bearer 格式标记，进 OpenAPI `bearerFormat`）。
pub const TOKEN_PREFIX: &str = "ss_live_";

/// root token 持有的全 scope 集合。ST-M2.5 `ScopeGuard` 据此硬约束第三方 token 不可
/// 授 `sessions:write` / `tokens:manage`，任何人不可授 `is_root`。
pub const ROOT_SCOPES: &[&str] = &[
    "sessions:read",
    "sessions:write",
    "sessions:delete",
    "tokens:manage",
];

/// root token 的展示名。
pub const ROOT_TOKEN_NAME: &str = "root";

/// account_id 的字节长度（UUID v4 = 16B）。
const ACCOUNT_ID_LEN: usize = 16;
/// secret 的字节长度。
const SECRET_LEN: usize = 32;

/// 已签发 token 的完整数据（明文 secret 仅创建时持有一次）。
///
/// 供编排器插入 per-account DB 的 `tokens` 行（`token_hash` 比对）并返回给调用方
/// （ST-M2.6 handler 组装 `TokenCreated` DTO）。
#[derive(Debug, Clone)]
pub struct IssuedToken {
    /// token id（UUID v4）。
    pub id: String,
    /// 所属账户（token 绑定此 account_id，仅可访问该账户且仅该账户已解锁时）。
    pub account_id: String,
    /// 用户命名（root token 用 [`ROOT_TOKEN_NAME`]）。
    pub name: String,
    /// 展示前缀（`ss_live_` + secret 前缀，不含 account_id）。
    pub prefix: String,
    /// 完整 bearer 明文（`ss_live_xxx`），仅创建时返回一次。
    pub secret_bearer: String,
    /// `SHA-256(secret)` 的 hex，存 DB `tokens.token_hash`。
    pub token_hash: String,
    /// 是否 root token。
    pub is_root: bool,
    /// scope 列表（root 持 [`ROOT_SCOPES`]）。
    pub scopes: Vec<String>,
    /// 创建时间（epoch 秒 UTC）。
    pub created_at: i64,
}

/// 从 bearer 解析出的自描述字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToken {
    /// account_id（UUID 字符串，用于 registry 查找与跨账户判断）。
    pub account_id: String,
    /// 32B secret 原始字节。
    pub secret: [u8; SECRET_LEN],
}

/// 生成一个新 token。
///
/// `account_id` 须为合法 UUID v4 字符串（registry 中即此形态）。`is_root=true` 时
/// 覆盖 `name` 为 [`ROOT_TOKEN_NAME`]、`scopes` 为 [`ROOT_SCOPES`]。
pub fn generate_token(
    account_id: &str,
    name: &str,
    is_root: bool,
    scopes: Vec<String>,
    created_at: i64,
) -> Result<IssuedToken, AccountError> {
    let acct_uuid = Uuid::parse_str(account_id)
        .map_err(|_| AccountError::AccountNotFound(account_id.to_string()))?;
    let mut secret = [0u8; SECRET_LEN];
    rand::rngs::OsRng.fill_bytes(&mut secret);

    // bearer = ss_live_ + base64url( account_id[16] ‖ secret[32] )
    let mut payload = Vec::with_capacity(ACCOUNT_ID_LEN + SECRET_LEN);
    payload.extend_from_slice(acct_uuid.as_bytes());
    payload.extend_from_slice(&secret);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
    let secret_bearer = format!("{TOKEN_PREFIX}{b64}");

    // prefix = ss_live_ + base64url(secret) 前 8 字符（仅展示，不含 account_id）。
    let secret_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    let prefix = format!("{TOKEN_PREFIX}{}", &secret_b64[..8]);

    // token_hash = SHA-256(secret) hex
    let hash = sha2::Sha256::digest(secret);
    let token_hash = hex_encode(&hash);

    let (name, scopes) = if is_root {
        (
            ROOT_TOKEN_NAME.to_string(),
            ROOT_SCOPES.iter().map(|s| s.to_string()).collect(),
        )
    } else {
        (name.to_string(), scopes)
    };

    Ok(IssuedToken {
        id: Uuid::new_v4().to_string(),
        account_id: account_id.to_string(),
        name,
        prefix,
        secret_bearer,
        token_hash,
        is_root,
        scopes,
        created_at,
    })
}

/// 解析 bearer：拆出 `account_id`（UUID 字符串）与 32B secret。
///
/// 失败情形（verify 第 1 步 → 401，统一映射 `InvalidToken`，不泄露账户存在性）：
/// - 无 `ss_live_` 前缀。
/// - base64url 解码失败。
/// - payload 长度 ≠ 48B。
/// - account_id 前 16B 非合法 UUID。
pub fn parse_bearer(bearer: &str) -> Result<ParsedToken, AccountError> {
    let rest = bearer
        .strip_prefix(TOKEN_PREFIX)
        .ok_or_else(|| AccountError::InvalidToken("missing ss_live_ prefix".into()))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(rest)
        .map_err(|_| AccountError::InvalidToken("base64url decode failed".into()))?;
    if payload.len() != ACCOUNT_ID_LEN + SECRET_LEN {
        return Err(AccountError::InvalidToken("payload length mismatch".into()));
    }
    let mut acct_bytes = [0u8; ACCOUNT_ID_LEN];
    acct_bytes.copy_from_slice(&payload[..ACCOUNT_ID_LEN]);
    let acct_uuid = Uuid::from_slice(&acct_bytes)
        .map_err(|_| AccountError::InvalidToken("account_id not a uuid".into()))?;
    let mut secret = [0u8; SECRET_LEN];
    secret.copy_from_slice(&payload[ACCOUNT_ID_LEN..]);
    Ok(ParsedToken {
        account_id: acct_uuid.to_string(),
        secret,
    })
}

/// `SHA-256(secret)` 的 hex（与生成时一致，verify 路径比对 DB `token_hash`）。
pub fn hash_secret(secret: &[u8; SECRET_LEN]) -> String {
    let h = sha2::Sha256::digest(secret);
    hex_encode(&h)
}

/// 32B → 小写 hex。
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// 引入 rand trait（fill_bytes）。
use rand::RngCore;

#[cfg(test)]
mod tests {
    use super::*;

    fn acct() -> String {
        Uuid::new_v4().to_string()
    }

    /// 生成 token：bearer = ss_live_ + 64 字符。
    #[test]
    fn bearer_shape() {
        let t = generate_token(&acct(), "n", false, vec![], 0).unwrap();
        assert!(t.secret_bearer.starts_with("ss_live_"));
        let body = &t.secret_bearer["ss_live_".len()..];
        assert_eq!(body.len(), 64, "48B base64url 无填充 = 64 字符");
    }

    /// prefix = ss_live_ + 8 字符，不含 account_id。
    #[test]
    fn prefix_is_secret_only_8chars() {
        let t = generate_token(&acct(), "n", false, vec![], 0).unwrap();
        assert!(t.prefix.starts_with("ss_live_"));
        let body = &t.prefix["ss_live_".len()..];
        assert_eq!(body.len(), 8);
        // prefix 不应等于完整 bearer（不含 account_id 部分）。
        assert_ne!(t.prefix, t.secret_bearer);
    }

    /// root token：name=root，scopes=ROOT_SCOPES，is_root=true。
    #[test]
    fn root_token_fields() {
        let t = generate_token(&acct(), "whatever", true, vec!["x".into()], 0).unwrap();
        assert_eq!(t.name, ROOT_TOKEN_NAME);
        assert!(t.is_root);
        assert_eq!(t.scopes, ROOT_SCOPES);
    }

    /// 非root token：保留传入 name/scopes。
    #[test]
    fn nonroot_token_preserves_name_scopes() {
        let t = generate_token(&acct(), "ci", false, vec!["sessions:read".into()], 0).unwrap();
        assert_eq!(t.name, "ci");
        assert!(!t.is_root);
        assert_eq!(t.scopes, vec!["sessions:read"]);
    }

    /// token_hash = SHA-256(secret) hex（64 字符）。
    #[test]
    fn token_hash_is_sha256_hex() {
        let t = generate_token(&acct(), "n", false, vec![], 0).unwrap();
        assert_eq!(t.token_hash.len(), 64);
        assert!(t.token_hash.chars().all(|c| c.is_ascii_hexdigit()));
        // 与独立计算一致。
        let parsed = parse_bearer(&t.secret_bearer).unwrap();
        assert_eq!(t.token_hash, hash_secret(&parsed.secret));
    }

    /// 生成 → 解析 往返：account_id 与 secret 一致。
    #[test]
    fn generate_then_parse_roundtrip() {
        let aid = acct();
        let t = generate_token(&aid, "n", false, vec![], 0).unwrap();
        let parsed = parse_bearer(&t.secret_bearer).unwrap();
        assert_eq!(parsed.account_id, aid);
        // hash 与 token_hash 一致 ⇒ secret 一致。
        assert_eq!(hash_secret(&parsed.secret), t.token_hash);
    }

    /// 解析：无前缀 → InvalidToken。
    #[test]
    fn parse_rejects_missing_prefix() {
        match parse_bearer("live_abcd") {
            Err(AccountError::InvalidToken(_)) => {}
            other => panic!("期望 InvalidToken，实际 {other:?}"),
        }
    }

    /// 解析：非法 base64 → InvalidToken。
    #[test]
    fn parse_rejects_bad_base64() {
        match parse_bearer("ss_live_!!!not-base64!!!") {
            Err(AccountError::InvalidToken(_)) => {}
            other => panic!("期望 InvalidToken，实际 {other:?}"),
        }
    }

    /// 解析：长度不对 → InvalidToken。
    #[test]
    fn parse_rejects_wrong_length() {
        let short = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"too short");
        match parse_bearer(&format!("ss_live_{short}")) {
            Err(AccountError::InvalidToken(_)) => {}
            other => panic!("期望 InvalidToken，实际 {other:?}"),
        }
    }

    /// 解析：account_id 非合法 UUID（虽然长度对但字节无法构成 UUID——实际 16B 必成 UUID，
    /// 故此处验长度路径为主；补一条随机 48B 仍可解析，account_id 为某 UUID）。
    #[test]
    fn parse_random_48bytes_yields_uuid() {
        let mut payload = vec![0u8; 48];
        rand::rngs::OsRng.fill_bytes(&mut payload);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
        let parsed = parse_bearer(&format!("ss_live_{b64}")).unwrap();
        assert!(Uuid::parse_str(&parsed.account_id).is_ok());
    }

    /// 两次生成不同（secret 随机）。
    #[test]
    fn generate_is_distinct() {
        let aid = acct();
        let a = generate_token(&aid, "n", false, vec![], 0).unwrap();
        let b = generate_token(&aid, "n", false, vec![], 0).unwrap();
        assert_ne!(a.secret_bearer, b.secret_bearer);
        assert_ne!(a.token_hash, b.token_hash);
    }

    /// account_id 非法 UUID → 生成失败。
    #[test]
    fn generate_rejects_non_uuid_account_id() {
        assert!(generate_token("not-a-uuid", "n", false, vec![], 0).is_err());
    }
}
