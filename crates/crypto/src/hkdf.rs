//! HKDF-SHA256 域隔离派生（ST-M2.1）。
//!
//! master_DEK[acct] → K_sqlite（SQLCipher PRAGMA raw key）/ K_files（内容 AEAD 钥）。
//! info 串全局恒定、**不含 account_id**（账户隔离靠独立 DEK，故账户改名不影响解密），
//! 一经发布不可改（改=解不出历史）。salt=None：IKM 已为高熵 32B 随机密钥，无需再加盐。

use hkdf::Hkdf;
use sha2::Sha256;

use crate::Error;

/// K_sqlite 的 HKDF info 串。**不可变常量**——一经发布改名即解不出历史整库。
const INFO_K_SQLITE: &[u8] = b"seasnail/sqlite/v1";
/// K_files 的 HKDF info 串。**不可变常量**——一经发布改名即解不出历史内容文件。
const INFO_K_FILES: &[u8] = b"seasnail/files/v1";

/// 派生 K_sqlite[acct] = HKDF-SHA256(master_DEK, info="seasnail/sqlite/v1")。
/// 作 SQLCipher `PRAGMA key="x'<64hex>'"` raw key 透明加解密该账户整库（元数据+tokens）。
pub fn derive_k_sqlite(master_dek: &[u8; 32]) -> [u8; 32] {
    hkdf_sha256(master_dek, INFO_K_SQLITE)
}

/// 派生 K_files[acct] = HKDF-SHA256(master_DEK, info="seasnail/files/v1")。
/// 作 chacha20poly1305 密钥加解密内容文件树（音频 + 转译）。
pub fn derive_k_files(master_dek: &[u8; 32]) -> [u8; 32] {
    hkdf_sha256(master_dek, INFO_K_FILES)
}

/// HKDF-SHA256：salt=None → RFC5869 以 HashLen 零字节为盐；expand 出 32B OKM。
/// 32B IKM + 短 info 下 expand 不可能失败（OKM 上限 255×32），故失败映射 HkdfFailed
/// 仅作防御，实践中不可达。
fn hkdf_sha256(ikm: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let h = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 32];
    h.expand(info, &mut okm)
        // 唯一失败路径：OKM 长度超 255×HashLen；32 远小于上限，不可达。
        .map_err(|_| Error::HkdfFailed)
        .expect("hkdf expand: 32B OKM 在上限内，不可达");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收"派生确定"：同 DEK 同 info → 同子密钥。
    #[test]
    fn derive_is_deterministic() {
        let dek = [0x01u8; 32];
        assert_eq!(derive_k_sqlite(&dek), derive_k_sqlite(&dek));
        assert_eq!(derive_k_files(&dek), derive_k_files(&dek));
    }

    /// 域隔离：同 DEK，不同 info → 不同子密钥（K_sqlite ≠ K_files）。
    #[test]
    fn domain_separation_yields_distinct_keys() {
        let dek = [0x01u8; 32];
        assert_ne!(
            derive_k_sqlite(&dek),
            derive_k_files(&dek),
            "不同 info 域隔离应派生不同子密钥"
        );
    }

    /// 子密钥随 DEK 变：不同 DEK → 不同子密钥（避免 DEK 碰撞无感）。
    #[test]
    fn derive_scales_with_dek() {
        let a = [0x01u8; 32];
        let b = [0x02u8; 32];
        assert_ne!(derive_k_sqlite(&a), derive_k_sqlite(&b));
        assert_ne!(derive_k_files(&a), derive_k_files(&b));
    }
}
