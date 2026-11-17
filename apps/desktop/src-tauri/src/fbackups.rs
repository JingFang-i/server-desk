//! write_file 自动备份存储：data_dir/file_backups/{id}.bak + registry.json。
//! 独立于 AppState（纯路径 IO），可在单元测试中直接构造。

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
}

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

/// 保存一份备份：写入 blob + 追加注册表。
pub fn save(
    data_dir: &Path,
    server_id: &str,
    path: &str,
    content: &[u8],
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
    std::fs::write(&blob_path, content).map_err(|e| format!("写入备份内容失败: {e}"))?;

    let meta = BackupMeta {
        id: id.clone(),
        server_id: server_id.to_string(),
        path: path.to_string(),
        size: content.len(),
        ts: ts.to_rfc3339(),
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

/// 读取备份元数据与内容。
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
}
