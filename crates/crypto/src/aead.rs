//! AEAD 加密（chacha20poly1305，ST-M2.1）。
//!
//! 内容文件（K_files 钥）与 DEK 包裹（KEK 钥）共用同一 AEAD 原语，仅密钥不同。
//! 输出格式：`nonce(12) ‖ ciphertext ‖ tag(16)`；nonce 每次 OsRng 随机、绝不重用
//! （key+nonce 重用即破）。AAD 支持预留（当前内容文件与 DEK 包裹均传空 AAD）。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::Error;

/// chacha20poly1305 nonce 长度（12B）。
const NONCE_LEN: usize = 12;
/// poly1305 认证 tag 长度（16B）。
const TAG_LEN: usize = 16;

/// AEAD seal：输出 `nonce(12) ‖ ct ‖ tag(16)`。
fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    // chacha20poly1305 encrypt 对合法 32B 钥不失败（aead::Error 仅 nonce 计数溢出，
    // 随机 12B nonce 单次加密不可达）。
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("chacha20poly1305 encrypt: 合法 32B 钥不可达失败");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    out
}

/// AEAD open：输入 `nonce(12) ‖ ct ‖ tag(16)`，认证后返明文。
/// 密钥错 / 篡改 → AuthenticationFailed；过短 → CiphertextTooShort。
fn open(key: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    if ciphertext.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::CiphertextTooShort);
    }
    let nonce = Nonce::from_slice(&ciphertext[..NONCE_LEN]);
    let ct = &ciphertext[NONCE_LEN..];
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(nonce, Payload { msg: ct, aad })
        .map_err(|_| Error::AuthenticationFailed)
}

/// 加密内容文件（K_files 钥，空 AAD）。输出 `nonce ‖ ct ‖ tag`。
pub fn encrypt_file(k_files: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    seal(k_files, b"", plaintext)
}

/// 解密内容文件。密钥错 / 篡改 → AuthenticationFailed；过短 → CiphertextTooShort。
pub fn decrypt_file(k_files: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    open(k_files, b"", ciphertext)
}

/// 加密需绑定到特定资源的内容文件。AAD 认证但不写入明文，适用于 context 等不能
/// 被跨会话替换的文件。
pub fn encrypt_file_with_aad(k_files: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    seal(k_files, aad, plaintext)
}

/// 解密需绑定到特定资源的内容文件；AAD 不匹配与密文篡改同样失败。
pub fn decrypt_file_with_aad(
    k_files: &[u8; 32],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, Error> {
    open(k_files, aad, ciphertext)
}

/// 包裹 master_DEK（KEK 钥，AEAD seal）。输出 `nonce ‖ ct(=DEK) ‖ tag`。
pub fn wrap_dek(kek: &[u8; 32], master_dek: &[u8; 32]) -> Vec<u8> {
    seal(kek, b"", master_dek)
}

/// 解包 master_DEK（KEK 钥，AEAD open）。认证通过且明文恰 32B 才返 DEK。
pub fn unwrap_dek(kek: &[u8; 32], wrapped: &[u8]) -> Result<[u8; 32], Error> {
    let pt = open(kek, b"", wrapped)?;
    if pt.len() != 32 {
        return Err(Error::InvalidDekLength);
    }
    let mut dek = [0u8; 32];
    dek.copy_from_slice(&pt);
    Ok(dek)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收"AEAD 往返"：加密后解密回原文。
    #[test]
    fn aead_roundtrip_file() {
        let key = [0x11u8; 32];
        let pt = b"transcript content here";
        let ct = encrypt_file(&key, pt);
        assert_eq!(decrypt_file(&key, &ct).unwrap(), pt);
    }

    /// 验收"nonce 唯一"：同明文同钥两次加密 → nonce 随机 → 密文不同；各自仍解回原文。
    #[test]
    fn nonce_uniqueness_yields_distinct_ciphertext() {
        let key = [0x22u8; 32];
        let pt = b"same plaintext";
        let ct1 = encrypt_file(&key, pt);
        let ct2 = encrypt_file(&key, pt);
        assert_ne!(ct1, ct2, "nonce 随机应使两次密文不同");
        // nonce 前置，各自解密仍回原文。
        assert_eq!(decrypt_file(&key, &ct1).unwrap(), pt);
        assert_eq!(decrypt_file(&key, &ct2).unwrap(), pt);
        // 两次 nonce（前 12B）确不同。
        assert_ne!(&ct1[..NONCE_LEN], &ct2[..NONCE_LEN]);
    }

    /// 输出格式 = nonce(12) ‖ ct(=明文长) ‖ tag(16)。
    #[test]
    fn output_format_is_nonce_ct_tag() {
        let key = [0x33u8; 32];
        let pt = b"0123456789ABCDEF";
        let ct = encrypt_file(&key, pt);
        assert_eq!(ct.len(), NONCE_LEN + pt.len() + TAG_LEN);
    }

    /// 篡改密文 → 认证失败。
    #[test]
    fn tamper_fails_authentication() {
        let key = [0x44u8; 32];
        let pt = b"integrity check";
        let mut ct = encrypt_file(&key, pt);
        let last = ct.len() - 1;
        ct[last] ^= 0xff; // 翻 tag 末位
        assert!(matches!(
            decrypt_file(&key, &ct).unwrap_err(),
            Error::AuthenticationFailed
        ));
    }

    /// 错密钥解密 → 认证失败。
    #[test]
    fn wrong_key_fails_authentication() {
        let key = [0x55u8; 32];
        let wrong = [0x99u8; 32];
        let ct = encrypt_file(&key, b"secret");
        assert!(matches!(
            decrypt_file(&wrong, &ct).unwrap_err(),
            Error::AuthenticationFailed
        ));
    }

    /// 密文过短 → CiphertextTooShort。
    #[test]
    fn too_short_ciphertext_rejected() {
        let key = [0u8; 32];
        assert!(matches!(
            decrypt_file(&key, b"short").unwrap_err(),
            Error::CiphertextTooShort
        ));
    }

    /// DEK 包裹往返。
    #[test]
    fn wrap_unwrap_dek_roundtrip() {
        let kek = [0xaa; 32];
        let dek = [0xbb; 32];
        let wrapped = wrap_dek(&kek, &dek);
        assert_eq!(unwrap_dek(&kek, &wrapped).unwrap(), dek);
    }

    /// 错 KEK 解包 → 认证失败。
    #[test]
    fn unwrap_wrong_kek_fails() {
        let kek = [0xaa; 32];
        let wrong = [0xcc; 32];
        let dek = [0xbb; 32];
        let wrapped = wrap_dek(&kek, &dek);
        assert!(matches!(
            unwrap_dek(&wrong, &wrapped).unwrap_err(),
            Error::AuthenticationFailed
        ));
    }
}
