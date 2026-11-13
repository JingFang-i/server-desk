//! 服务器资产加密备份。
//!
//! 两种形态：
//! - 手动导出（.sdbackup）：用户口令 Argon2id 派生 KEK + AES-256-GCM，可携到任意机器恢复
//! - 自动备份（data_dir/backups/*.sdbackup）：用当前 DEK 加密（防误删/损坏），恢复需保险库解锁
//!
//! 文件格式：MAGIC("SDBK") | ver(1B) | salt(16B) | nonce(12B) | ciphertext(JSON payload)

use crate::crypto;
use crate::store::{ServerRow, Store};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"SDBK";
const FORMAT_VER: u8 = 1;
const SALT_LEN: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
pub struct BackupPayload {
    version: u32,
    exported_at: String,
    servers: Vec<BackupServer>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupServer {
    id: String,
    name: String,
    host: String,
    port: u16,
    username: String,
    auth_type: String,
    secret_blob: String, // base64(密文)
    group_name: Option<String>,
    tags: String,
    notes: Option<String>,
    allow_ai: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub count: usize,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub imported: usize,
    pub mode: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFileMeta {
    pub name: String,
    pub size_bytes: u64,
    pub created_at: String,
}

fn payload_from_store(store: &Store) -> Result<BackupPayload, String> {
    let rows = store.list()?;
    let servers = rows
        .iter()
        .map(|r| BackupServer {
            id: r.id.clone(),
            name: r.name.clone(),
            host: r.host.clone(),
            port: r.port,
            username: r.username.clone(),
            auth_type: r.auth_type.clone(),
            secret_blob: B64.encode(&r.secret_blob),
            group_name: r.group_name.clone(),
            tags: r.tags.clone(),
            notes: r.notes.clone(),
            allow_ai: r.allow_ai,
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
        })
        .collect();
    Ok(BackupPayload {
        version: 1,
        exported_at: chrono::Utc::now().to_rfc3339(),
        servers,
    })
}

fn rows_from_payload(payload: &BackupPayload) -> Result<Vec<ServerRow>, String> {
    payload
        .servers
        .iter()
        .map(|s| {
            Ok(ServerRow {
                id: s.id.clone(),
                name: s.name.clone(),
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                auth_type: s.auth_type.clone(),
                secret_blob: B64.decode(&s.secret_blob)
                    .map_err(|_| format!("服务器「{}」凭据密文损坏", s.name))?,
                group_name: s.group_name.clone(),
                tags: s.tags.clone(),
                notes: s.notes.clone(),
                allow_ai: s.allow_ai,
                created_at: s.created_at.clone(),
                updated_at: s.updated_at.clone(),
            })
        })
        .collect()
}

/// 手动导出：口令加密写入指定路径。
pub fn export_encrypted(store: &Store, path: &Path, password: &str) -> Result<ExportResult, String> {
    if password.chars().count() < 8 {
        return Err("备份口令至少 8 位".into());
    }
    let payload = payload_from_store(store)?;
    let json = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let kek = crypto::argon2_kek(password, &salt)?;
    let ct = crypto::encrypt(&kek, &json)?; // 输出 = nonce(12B) || ciphertext

    let mut out = Vec::with_capacity(MAGIC.len() + 1 + SALT_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VER);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&ct);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(path, &out).map_err(|e| format!("写入备份文件失败: {e}"))?;
    Ok(ExportResult {
        count: payload.servers.len(),
        path: path.display().to_string(),
    })
}

/// 手动恢复：口令解密读取。
pub fn read_payload(path: &Path, password: &str) -> Result<BackupPayload, String> {
    let raw = std::fs::read(path).map_err(|e| format!("读取备份文件失败: {e}"))?;
    if raw.len() < MAGIC.len() + 1 + SALT_LEN + 12 {
        return Err("备份文件格式错误".into());
    }
    if &raw[0..4] != MAGIC {
        return Err("不是 ServerDesk 备份文件".into());
    }
    let ver = raw[4];
    if ver != FORMAT_VER {
        return Err(format!("不支持的备份格式版本: {ver}"));
    }
    let salt: [u8; SALT_LEN] = raw[5..5 + SALT_LEN]
        .try_into()
        .map_err(|_| "备份文件格式错误".to_string())?;
    let ct = &raw[5 + SALT_LEN..];
    let kek = crypto::argon2_kek(password, &salt)?;
    let json = crypto::decrypt(&kek, ct).map_err(|_| "备份口令错误或文件已损坏".to_string())?;
    let payload: BackupPayload =
        serde_json::from_slice(&json).map_err(|e| format!("备份内容解析失败: {e}"))?;
    Ok(payload)
}

pub fn restore_payload(
    store: &Store,
    payload: &BackupPayload,
    mode: &str,
) -> Result<RestoreResult, String> {
    let rows = rows_from_payload(payload)?;
    match mode {
        "overwrite" => {
            store.replace_all_servers(&rows)?;
        }
        "merge" => {
            for r in &rows {
                store.upsert_server(r)?;
            }
        }
        other => return Err(format!("未知恢复模式: {other}")),
    }
    Ok(RestoreResult {
        imported: rows.len(),
        mode: mode.to_string(),
    })
}

// ---------- 自动备份（DEK 加密，存 data_dir/backups） ----------

pub fn auto_backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backups")
}

/// 用当前 DEK 执行一次自动备份并裁剪旧文件。
pub fn run_auto_backup(
    store: &Store,
    vault: &crate::vault::Vault,
    data_dir: &Path,
    keep: usize,
) -> Result<PathBuf, String> {
    let dek = vault.dek().map_err(|_| "保险库已锁定，本次自动备份跳过".to_string())?;
    let payload = payload_from_store(store)?;
    let json = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    let blob = crypto::encrypt(&dek, &json)?;

    let dir = auto_backup_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("auto-{ts}.sdbackup"));
    std::fs::write(&path, &blob).map_err(|e| format!("写入自动备份失败: {e}"))?;

    prune_old(&dir, keep)?;
    Ok(path)
}

/// 自动备份恢复：当前 DEK 解密（需已解锁）。
pub fn restore_auto_backup(
    store: &Store,
    vault: &crate::vault::Vault,
    data_dir: &Path,
    name: &str,
    mode: &str,
) -> Result<RestoreResult, String> {
    // 名称安全：仅允许 auto-*.sdbackup 白名单形式
    if !name.starts_with("auto-") || !name.ends_with(".sdbackup") || name.contains("..") || name.contains('/') {
        return Err("非法的备份文件名".into());
    }
    let dek = vault.dek().map_err(|e| e.to_string())?;
    let path = auto_backup_dir(data_dir).join(name);
    let raw = std::fs::read(&path).map_err(|e| format!("读取备份失败: {e}"))?;
    let json = crypto::decrypt(&dek, &raw).map_err(|_| "自动备份解密失败（密钥不匹配或文件损坏）".to_string())?;
    let payload: BackupPayload =
        serde_json::from_slice(&json).map_err(|e| format!("备份内容解析失败: {e}"))?;
    restore_payload(store, &payload, mode)
}

pub fn list_auto_backups(data_dir: &Path) -> Vec<BackupFileMeta> {
    let dir = auto_backup_dir(data_dir);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !(name.starts_with("auto-") && name.ends_with(".sdbackup")) {
                continue;
            }
            let meta = e.metadata().ok();
            out.push(BackupFileMeta {
                created_at: name
                    .trim_start_matches("auto-")
                    .trim_end_matches(".sdbackup")
                    .to_string(),
                size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                name,
            });
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

pub fn delete_auto_backup(data_dir: &Path, name: &str) -> Result<(), String> {
    if !name.starts_with("auto-") || !name.ends_with(".sdbackup") || name.contains("..") || name.contains('/') {
        return Err("非法的备份文件名".into());
    }
    std::fs::remove_file(auto_backup_dir(data_dir).join(name))
        .map_err(|e| format!("删除失败: {e}"))
}

fn prune_old(dir: &Path, keep: usize) -> Result<(), String> {
    if keep == 0 {
        return Ok(()); // 0 = 不裁剪
    }
    let mut files = list_auto_backups(dir);
    if files.len() <= keep {
        return Ok(());
    }
    // list 已按时间倒序，保留前 keep 个
    for f in files.split_off(keep) {
        let _ = std::fs::remove_file(dir.join(&f.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<ServerRow> {
        vec![ServerRow {
            id: "id-1".into(),
            name: "test-01".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            auth_type: "password".into(),
            secret_blob: vec![1u8; 40],
            group_name: Some("prod".into()),
            tags: r#"["web"]"#.into(),
            notes: None,
            allow_ai: true,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }]
    }

    #[test]
    fn export_roundtrip_and_wrong_password() {
        let db = std::env::temp_dir().join(format!("sd-bk-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");
        for r in sample_rows() {
            store.insert(&r).expect("insert");
        }
        let path = std::env::temp_dir().join(format!("sd-bk-{}.sdbackup", std::process::id()));

        let res = export_encrypted(&store, &path, "strong-pass-1").expect("export");
        assert_eq!(res.count, 1);

        // 正确口令
        let payload = read_payload(&path, "strong-pass-1").expect("read");
        assert_eq!(payload.servers.len(), 1);
        assert_eq!(payload.servers[0].name, "test-01");

        // 错误口令
        assert!(read_payload(&path, "wrong-password").is_err());

        // 文件确为密文
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[0..4], MAGIC);
        let text = String::from_utf8_lossy(&raw[21..]);
        assert!(!text.contains("10.0.0.1"), "不应出现明文主机信息");

        // 合并恢复到空库
        let res2 = restore_payload(&store, &payload, "merge").expect("restore");
        assert_eq!(res2.imported, 1);
        assert_eq!(store.list().unwrap().len(), 1);

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&path);
    }
}
