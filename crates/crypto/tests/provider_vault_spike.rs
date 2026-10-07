//! ST-M0.3 spike：验证账户级 Provider credential vault 的编码和容量边界。
//!
//! 这里刻意保持 codec 私有；M2.1 会在已验证的边界上实现正式
//! `ProviderCredentialStore`，并补齐生产错误类型与 secret 清零。

use seasnail_crypto::{KeychainLabel, KeychainStore, MemoryKeychain};

const MAGIC: &[u8; 4] = b"SSPV";
const VERSION: u16 = 1;
const MAX_CONFIGS: usize = 32;
const MAX_SECRET_BYTES: usize = 8 * 1024;
const MAX_ENCODED_BYTES: usize = 128 * 1024;

struct Entry {
    config_id: String,
    provider_type: u8,
    endpoint_fingerprint: [u8; 32],
    secret: Vec<u8>,
}

fn entry(index: usize, secret_len: usize) -> Entry {
    Entry {
        config_id: format!("00000000-0000-4000-8000-{index:012}"),
        provider_type: 1,
        endpoint_fingerprint: [index as u8; 32],
        secret: vec![b'k'; secret_len],
    }
}

fn encode(entries: &[Entry]) -> Result<Vec<u8>, &'static str> {
    if entries.len() > MAX_CONFIGS {
        return Err("too_many_configs");
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_be_bytes());
    bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());

    for value in entries {
        if value.secret.is_empty() || value.secret.len() > MAX_SECRET_BYTES {
            return Err("invalid_secret_length");
        }
        let id = value.config_id.as_bytes();
        let id_len = u16::try_from(id.len()).map_err(|_| "config_id_too_long")?;
        let secret_len = u32::try_from(value.secret.len()).map_err(|_| "secret_too_long")?;
        bytes.extend_from_slice(&id_len.to_be_bytes());
        bytes.extend_from_slice(id);
        bytes.push(value.provider_type);
        bytes.extend_from_slice(&value.endpoint_fingerprint);
        bytes.extend_from_slice(&secret_len.to_be_bytes());
        bytes.extend_from_slice(&value.secret);
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err("vault_too_large");
        }
    }
    Ok(bytes)
}

fn decode_header(bytes: &[u8]) -> Result<(u16, u16), &'static str> {
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return Err("invalid_magic");
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err("unsupported_version");
    }
    Ok((version, u16::from_be_bytes([bytes[6], bytes[7]])))
}

#[test]
fn versioned_vault_roundtrips_through_memory_keychain_with_32_configs() {
    let entries: Vec<_> = (0..MAX_CONFIGS).map(|i| entry(i, 16)).collect();
    let encoded = encode(&entries).expect("32 configurations must fit");
    let keychain = MemoryKeychain::new();

    keychain
        .set_secret(KeychainLabel::ProviderCredentials, "account-a", &encoded)
        .unwrap();
    let stored = keychain
        .get_secret(KeychainLabel::ProviderCredentials, "account-a")
        .unwrap()
        .expect("vault must exist");
    assert_eq!(stored, encoded);
    assert_eq!(decode_header(&stored), Ok((VERSION, MAX_CONFIGS as u16)));

    keychain
        .delete_secret(KeychainLabel::ProviderCredentials, "account-a")
        .unwrap();
    assert_eq!(
        keychain
            .get_secret(KeychainLabel::ProviderCredentials, "account-a")
            .unwrap(),
        None
    );
}

#[test]
fn accepts_8192_byte_secret_and_rejects_8193_bytes() {
    assert!(encode(&[entry(0, MAX_SECRET_BYTES)]).is_ok());
    assert_eq!(
        encode(&[entry(0, MAX_SECRET_BYTES + 1)]).unwrap_err(),
        "invalid_secret_length"
    );
}

#[test]
fn enforces_config_count_and_total_encoded_size() {
    let thirty_three: Vec<_> = (0..(MAX_CONFIGS + 1)).map(|i| entry(i, 1)).collect();
    assert_eq!(encode(&thirty_three).unwrap_err(), "too_many_configs");

    // V1 + canonical UUID 的固定大小为 8 + N * 75，先放 15 个最大 key，
    // 再精确填充最后一个 key，分别命中 128 KiB 与超出 1 byte。
    let mut exact_limit: Vec<_> = (0..15).map(|i| entry(i, MAX_SECRET_BYTES)).collect();
    exact_limit.push(entry(15, 6_984));
    let encoded = encode(&exact_limit).expect("exactly 128 KiB must fit");
    assert_eq!(encoded.len(), MAX_ENCODED_BYTES);

    let mut one_byte_over: Vec<_> = (0..15).map(|i| entry(i, MAX_SECRET_BYTES)).collect();
    one_byte_over.push(entry(15, 6_985));
    assert_eq!(encode(&one_byte_over).unwrap_err(), "vault_too_large");
}

#[test]
fn rejects_unknown_codec_version() {
    let mut encoded = encode(&[entry(0, 32)]).unwrap();
    encoded[4..6].copy_from_slice(&2_u16.to_be_bytes());
    assert_eq!(decode_header(&encoded), Err("unsupported_version"));
}
