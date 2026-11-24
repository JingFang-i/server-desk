//! write_file 自动备份存储：data_dir/file_backups/{id}.bak + registry.json。
//! 独立于 AppState（纯路径 IO），可在单元测试中直接构造。
//!
//! 内容默认用保险库数据密钥（DEK）加密后再落盘：远程配置文件常带口令/令牌，
//! 明文 `.bak` 在保险库锁定后仍是可读的。旧版本留下的明文备份按原样读回（兼容），
//! 但新写入的一律是 `SDBK1 || nonce || ciphertext` 结构。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupMeta {
    pub id: String,
    pub server_id: String,
    pub path: String,
    pub size: usize,
    pub ts: String,
    /// 内容是否用 DEK 加密（旧版本备份为 false，读取时按明文处理）
    #[serde(default)]
    pub encrypted: bool,
}

/// 加密备份文件的魔数头；旧明文备份没有这个头。
const BLOB_MAGIC: &[u8; 5] = b"SDBK1";

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("file_backups")
}

fn registry_path(data_dir: &Path) -> PathBuf {
    dir(data_dir).join("registry.json")
}

static REG_LOCK: Mutex<()> = Mutex::new(());

fn read_registry(data_dir: &Path) -> Vec<BackupMeta> {
    let p = registry_path(data_dir);
    match std::fs::read(&p) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn write_registry(data_dir: &Path, entries: &[BackupMeta]) -> Result<(), String> {
    std::fs::create_dir_all(dir(data_dir)).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let json = serde_json::to_vec_pretty(entries).map_err(|e| e.to_string())?;
    std::fs::write(registry_path(data_dir), json).map_err(|e| format!("写入备份注册表失败: {e}"))
}

/// 保存一份备份（明文写入，仅测试与旧路径使用）。
#[allow(dead_code)]
pub fn save(
    data_dir: &Path,
    server_id: &str,
    path: &str,
    content: &[u8],
) -> Result<BackupMeta, String> {
    write_entry(data_dir, server_id, path, content, content.len(), false)
}

/// 保存一份**加密**备份：远程文件内容用 DEK 加密后落盘（推荐路径）。
pub fn save_encrypted(
    data_dir: &Path,
    key: &[u8; crate::crypto::KEY_LEN],
    server_id: &str,
    path: &str,
    content: &[u8],
) -> Result<BackupMeta, String> {
    let sealed = crate::crypto::encrypt(key, content)?;
    let mut blob = Vec::with_capacity(BLOB_MAGIC.len() + sealed.len());
    blob.extend_from_slice(BLOB_MAGIC);
    blob.extend_from_slice(&sealed);
    // size 记录原文件大小，便于界面展示；实际落盘大小是密文长度
    write_entry(data_dir, server_id, path, &blob, content.len(), true)
}

fn write_entry(
    data_dir: &Path,
    server_id: &str,
    path: &str,
    bytes: &[u8],
    original_size: usize,
    encrypted: bool,
) -> Result<BackupMeta, String> {
    let _g = REG_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let ts = chrono::Utc::now();
    let id = format!(
        "{}-{}",
        ts.format("%Y%m%d%H%M%S"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let blob_path = dir(data_dir).join(format!("{id}.bak"));
    std::fs::create_dir_all(dir(data_dir)).map_err(|e| format!("创建备份目录失败: {e}"))?;
    std::fs::write(&blob_path, bytes).map_err(|e| format!("写入备份内容失败: {e}"))?;

    let meta = BackupMeta {
        id: id.clone(),
        server_id: server_id.to_string(),
        path: path.to_string(),
        size: original_size,
        ts: ts.to_rfc3339(),
        encrypted,
    };
    let mut entries = read_registry(data_dir);
    entries.push(meta.clone());
    write_registry(data_dir, &entries)?;
    Ok(meta)
}

/// 列出备份（可按服务器过滤），新→旧排序。
pub fn list(data_dir: &Path, server_id: Option<&str>) -> Vec<BackupMeta> {
    let mut v: Vec<BackupMeta> = read_registry(data_dir)
        .into_iter()
        .filter(|m| server_id.map(|s| m.server_id == s).unwrap_or(true))
        .collect();
    v.sort_by(|a, b| b.ts.cmp(&a.ts));
    v
}

/// 读取备份元数据与**解密后**的内容；旧明文备份原样返回（兼容）。
pub fn load_encrypted(
    data_dir: &Path,
    key: &[u8; crate::crypto::KEY_LEN],
    id: &str,
) -> Result<(BackupMeta, Vec<u8>), String> {
    let (meta, blob) = load(data_dir, id)?;
    if blob.starts_with(BLOB_MAGIC) {
        let plain = crate::crypto::decrypt(key, &blob[BLOB_MAGIC.len()..])
            .map_err(|e| format!("备份解密失败（保险库密钥已变化？）：{e}"))?;
        Ok((meta, plain))
    } else {
        // 旧版本明文备份：按原样返回，避免升级后无法回滚
        Ok((meta, blob))
    }
}

/// 读取备份元数据与内容（原始字节，不做解密）。
pub fn load(data_dir: &Path, id: &str) -> Result<(BackupMeta, Vec<u8>), String> {
    // id 白名单校验：字母数字与连字符，防路径穿越
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("非法的备份 id".into());
    }
    let meta = read_registry(data_dir)
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| "备份不存在".to_string())?;
    let blob = std::fs::read(dir(data_dir).join(format!("{id}.bak")))
        .map_err(|_| "备份内容文件缺失".to_string())?;
    Ok((meta, blob))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sd-fbk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let m1 = save(&dir, "srv-1", "/etc/app/config.yml", b"hello=v1").expect("save");
        assert!(m1.id.starts_with("2"));
        let m2 = save(&dir, "srv-2", "/etc/other.conf", b"x=1").expect("save");

        // 全量列表（新→旧）
        let all = list(&dir, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, m2.id);

        // 按服务器过滤
        assert_eq!(list(&dir, Some("srv-1")).len(), 1);

        // 读取内容一致
        let (meta, content) = load(&dir, &m1.id).expect("load");
        assert_eq!(meta.path, "/etc/app/config.yml");
        assert_eq!(content, b"hello=v1");

        // 非法 id 拒绝
        assert!(load(&dir, "../../etc").is_err());
        assert!(load(&dir, "nonexistent").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_roundtrip_and_legacy_plaintext() {
        let dir = std::env::temp_dir().join(format!("sd-fbk-enc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = [3u8; crate::crypto::KEY_LEN];

        let m = save_encrypted(&dir, &key, "srv-1", "/etc/app.conf", b"password=hunter2")
            .expect("save");
        assert!(m.encrypted);
        // 落盘内容不得是明文
        let raw = std::fs::read(dir.join("file_backups").join(format!("{}.bak", m.id))).unwrap();
        assert!(raw.starts_with(BLOB_MAGIC));
        assert!(!String::from_utf8_lossy(&raw).contains("hunter2"));

        let (_, plain) = load_encrypted(&dir, &key, &m.id).expect("load");
        assert_eq!(plain, b"password=hunter2");

        // 密钥不匹配要明确报错
        let (_, enc) = load(&dir, &m.id).unwrap();
        assert!(crate::crypto::decrypt(&[9u8; 32], &enc[BLOB_MAGIC.len()..]).is_err());

        // 旧版本明文备份：无魔数头，按原样读回
        let legacy = save(&dir, "srv-1", "/etc/legacy.conf", b"old-plain").expect("legacy");
        let (_, content) = load_encrypted(&dir, &key, &legacy.id).expect("legacy load");
        assert_eq!(content, b"old-plain");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
