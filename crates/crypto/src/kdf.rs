//! Argon2id 密码派生（ST-M2.1）。
//!
//! 密码 → KEK[acct]（仅 wrap/unwrap master_DEK，不作内容加密钥）。
//! 参数（m/t/p cost + salt）须随件落盘于 wrapped-DEK，不可硬编码——改参即解不出历史。
//! M 芯片上调到 ~250–500ms（ST-M2.4 setup 实测调参，本步给 sane 起点）。

use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::Error;

/// Argon2id 参数（内存 m_kib KiB / 迭代 t_cost / 并行 p_cost）。
/// 随 wrapped-DEK 落盘；改参=KEK 变=解不出历史 wrapped-DEK，故参数一经写入不可变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Argon2Params {
    /// 内存成本（KiB）。argon2 要求 m_kib ≥ 8 × p_cost。
    pub m_kib: u32,
    /// 时间成本（迭代轮数）。
    pub t_cost: u32,
    /// 并行成本（lanes）。
    pub p_cost: u32,
}

impl Argon2Params {
    /// 默认参数：64 MiB / 3 轮 / 4 并行。M 芯片 ~250–500ms 待 ST-M2.4 实测微调。
    pub const DEFAULT: Self = Self {
        m_kib: 65_536,
        t_cost: 3,
        p_cost: 4,
    };
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 由参数构造 Argon2id 实例。非法参数（m_kib < 8×p 等）→ KdfFailed。
fn argon2_with(params: &Argon2Params) -> Result<Argon2<'static>, Error> {
    let p = Params::new(params.m_kib, params.t_cost, params.p_cost, None)
        .map_err(|_| Error::KdfFailed)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, p))
}

/// 派生 KEK = Argon2id(password, salt, params) → 32B。仅用于 wrap/unwrap master_DEK。
/// 非法参数 / 内存不足 → KdfFailed。
pub fn derive_kek(password: &str, salt: &[u8], params: &Argon2Params) -> Result<[u8; 32], Error> {
    let argon2 = argon2_with(params)?;
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|_| Error::KdfFailed)?;
    Ok(out)
}

/// 生成 16B salt（OsRng）。每账户一份，随 wrapped-DEK 落盘。
pub fn generate_salt() -> [u8; 16] {
    let mut s = [0u8; 16];
    OsRng.fill_bytes(&mut s);
    s
}

/// 生成 32B 随机 master_DEK（OsRng）。每账户一份，明文存 keychain（活跃账户）。
pub fn random_master_dek() -> [u8; 32] {
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{unwrap_dek, wrap_dek};

    /// 测试用小参数（快）；非生产 DEFAULT。
    fn fast_a() -> Argon2Params {
        Argon2Params {
            m_kib: 8192,
            t_cost: 1,
            p_cost: 1,
        }
    }
    /// 与 fast_a 仅 m_kib 不同，用于"改参"对照。
    fn fast_b() -> Argon2Params {
        Argon2Params {
            m_kib: 16384,
            t_cost: 1,
            p_cost: 1,
        }
    }

    /// 派生确定：同密码同盐同参 → 同 KEK。
    #[test]
    fn kek_derivation_is_deterministic() {
        let salt = [0u8; 16];
        let k1 = derive_kek("password", &salt, &fast_a()).unwrap();
        let k2 = derive_kek("password", &salt, &fast_a()).unwrap();
        assert_eq!(k1, k2, "同参同密码同盐应派生同 KEK");
    }

    /// 改参 → 不同 KEK（"改参解不出历史"的前提）。
    #[test]
    fn kek_distinct_across_params() {
        let salt = [0u8; 16];
        let ka = derive_kek("password", &salt, &fast_a()).unwrap();
        let kb = derive_kek("password", &salt, &fast_b()).unwrap();
        assert_ne!(ka, kb, "不同参数应派生不同 KEK");
    }

    /// KEK 随密码与盐变。
    #[test]
    fn kek_distinct_across_password_and_salt() {
        let salt = [0u8; 16];
        let k1 = derive_kek("password", &salt, &fast_a()).unwrap();
        let k2 = derive_kek("drowssap", &salt, &fast_a()).unwrap();
        let salt2 = [0xff; 16];
        let k3 = derive_kek("password", &salt2, &fast_a()).unwrap();
        assert_ne!(k1, k2, "不同密码应派生不同 KEK");
        assert_ne!(k1, k3, "不同盐应派生不同 KEK");
    }

    /// 验收"改参解不出历史"：KEK_A 包裹的 DEK，用 KEK_B（改参）解不开。
    #[test]
    fn change_params_cannot_unwrap_history() {
        let salt = [0u8; 16];
        let kek_a = derive_kek("password", &salt, &fast_a()).unwrap();
        let kek_b = derive_kek("password", &salt, &fast_b()).unwrap();
        let dek = [0x42u8; 32];
        let wrapped = wrap_dek(&kek_a, &dek);
        // 原参可解。
        assert_eq!(unwrap_dek(&kek_a, &wrapped).unwrap(), dek);
        // 改参 → KEK 变 → AEAD 认证失败，解不出历史。
        assert!(matches!(
            unwrap_dek(&kek_b, &wrapped).unwrap_err(),
            crate::Error::AuthenticationFailed
        ));
    }

    /// 两次随机 master_DEK 不同（概率上）。
    #[test]
    fn random_master_dek_is_distinct() {
        assert_ne!(random_master_dek(), random_master_dek());
    }

    /// 两次随机 salt 不同（概率上）。
    #[test]
    fn salt_is_distinct() {
        assert_ne!(generate_salt(), generate_salt());
    }
}
