//! AES-256-GCM 加密原语。数据密钥由 vault 模块提供，本模块无状态。

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use rand::{rngs::OsRng, RngCore};
use std::path::Path;

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
pub const LEGACY_KEY_FILE: &str = "m1-interim.key";

/// M1 过渡方案遗留密钥的加载（仅用于一次性迁移进保险库）。
pub fn load_legacy_key(dir: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(dir.join(LEGACY_KEY_FILE)).ok()?;
    if bytes.len() == KEY_LEN {
        Some(bytes)
    } else {
        None
    }
}

pub fn remove_legacy_key(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(LEGACY_KEY_FILE));
}

/// Argon2id 口令派生 KEK（保险库与备份文件共用，OWASP 参数 19MiB/t=2/p=1）。
pub fn argon2_kek(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19_456, 2, 1, Some(KEY_LEN))
        .map_err(|e| format!("Argon2 参数非法: {e}"))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut kek = [0u8; KEY_LEN];
    a2.hash_password_into(password.as_bytes(), salt, &mut kek)
        .map_err(|e| format!("Argon2 派生失败: {e}"))?;
    Ok(kek)
}

/// 加密：输出 nonce(12B) || ciphertext。
pub fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("加密失败: {e}"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解密：输入 nonce(12B) || ciphertext。
pub fn decrypt(key: &[u8; KEY_LEN], blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < NONCE_LEN {
        return Err("密文格式错误".into());
    }
    let (n, ct) = blob.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(n), ct)
        .map_err(|_| "解密失败：密钥不匹配或数据损坏".to_string())
}
