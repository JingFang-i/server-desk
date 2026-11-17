//! SQLite 存储层：服务器资产（凭据字段以密文 BLOB 落盘，绝不存明文）、
//! known_hosts（TOFU 主机密钥指纹）、audit_log（M1 预建，M3 承接 AI 审计）。

use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL,
    username    TEXT NOT NULL,
    auth_type   TEXT NOT NULL,
    secret_blob BLOB NOT NULL,
    group_name  TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',
    notes       TEXT,
    allow_ai    INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_servers_group ON servers(group_name);
CREATE TABLE IF NOT EXISTS audit_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        TEXT NOT NULL,
    source    TEXT NOT NULL,
    server_id TEXT,
    action    TEXT NOT NULL,
    detail    TEXT NOT NULL,
    outcome   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS known_hosts (
    server_id   TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    first_seen  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

#[derive(Debug, Clone)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    pub secret_blob: Vec<u8>,
    pub group_name: Option<String>,
    pub tags: String,
    pub notes: Option<String>,
    pub allow_ai: bool,
    pub created_at: String,
    pub updated_at: String,
}

const COLS: &str = "id,name,host,port,username,auth_type,secret_blob,group_name,tags,notes,allow_ai,created_at,updated_at";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub ts: String,
    pub source: String,
    pub server_id: Option<String>,
    pub action: String,
    pub detail: String,
    pub outcome: String,
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServerRow> {
    Ok(ServerRow {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, i64>(3)? as u16,
        username: row.get(4)?,
        auth_type: row.get(5)?,
        secret_blob: row.get(6)?,
        group_name: row.get(7)?,
        tags: row.get(8)?,
        notes: row.get(9)?,
        allow_ai: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("设置 WAL 失败: {e}"))?;
        conn.execute_batch(SCHEMA_V1)
            .map_err(|e| format!("初始化表结构失败: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1')",
            [],
        )
        .map_err(|e| format!("写入 schema 版本失败: {e}"))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "数据库锁不可用".to_string())
    }

    // ---------- 服务器资产 ----------

    pub fn list(&self) -> Result<Vec<ServerRow>, String> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLS} FROM servers ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("查询失败: {e}"))?;
        let rows = stmt
            .query_map([], map_row)
            .map_err(|e| format!("查询失败: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取行失败: {e}"))?;
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> Result<Option<ServerRow>, String> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLS} FROM servers WHERE id = ?1");
        conn.query_row(&sql, params![id], map_row)
            .optional()
            .map_err(|e| format!("查询失败: {e}"))
    }

    pub fn list_by_group(&self, group: &str) -> Result<Vec<ServerRow>, String> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLS} FROM servers WHERE group_name = ?1 ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("查询失败: {e}"))?;
        let rows = stmt
            .query_map(params![group], map_row)
            .map_err(|e| format!("查询失败: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取行失败: {e}"))?;
        Ok(rows)
    }

    pub fn find_by_name(&self, name: &str) -> Result<Option<ServerRow>, String> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLS} FROM servers WHERE name = ?1");
        conn.query_row(&sql, params![name], map_row)
            .optional()
            .map_err(|e| format!("查询失败: {e}"))
    }

    pub fn insert(&self, r: &ServerRow) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            &format!(
                "INSERT INTO servers({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"
            ),
            params![
                r.id,
                r.name,
                r.host,
                r.port as i64,
                r.username,
                r.auth_type,
                r.secret_blob,
                r.group_name,
                r.tags,
                r.notes,
                r.allow_ai as i64,
                r.created_at,
                r.updated_at
            ],
        )
        .map_err(|e| format!("插入失败: {e}"))?;
        Ok(())
    }

    pub fn update(&self, r: &ServerRow) -> Result<bool, String> {
        let conn = self.lock()?;
        let n = conn
            .execute(
                "UPDATE servers SET name=?2, host=?3, port=?4, username=?5, auth_type=?6, \
                 secret_blob=?7, group_name=?8, tags=?9, notes=?10, allow_ai=?11, updated_at=?12 \
                 WHERE id=?1",
                params![
                    r.id,
                    r.name,
                    r.host,
                    r.port as i64,
                    r.username,
                    r.auth_type,
                    r.secret_blob,
                    r.group_name,
                    r.tags,
                    r.notes,
                    r.allow_ai as i64,
                    r.updated_at
                ],
            )
            .map_err(|e| format!("更新失败: {e}"))?;
        Ok(n > 0)
    }

    /// 仅更新凭据密文（用于 M1 → M2 凭据迁移，不触碰 updated_at 语义）。
    pub fn update_secret_blob(&self, id: &str, blob: &[u8]) -> Result<bool, String> {
        let conn = self.lock()?;
        let n = conn
            .execute(
                "UPDATE servers SET secret_blob=?2 WHERE id=?1",
                params![id, blob],
            )
            .map_err(|e| format!("更新凭据失败: {e}"))?;
        Ok(n > 0)
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let conn = self.lock()?;
        let n = conn
            .execute("DELETE FROM servers WHERE id = ?1", params![id])
            .map_err(|e| format!("删除失败: {e}"))?;
        let _ = conn.execute("DELETE FROM known_hosts WHERE server_id = ?1", params![id]);
        Ok(n > 0)
    }

    /// 危险操作：清空全部资产与审计（保险库重置时调用）。
    pub fn wipe_all(&self) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute_batch("DELETE FROM servers; DELETE FROM known_hosts; DELETE FROM audit_log;")
            .map_err(|e| format!("清空数据失败: {e}"))
    }

    /// 备份恢复-覆盖模式：事务内清空并重建服务器表（保留审计日志）。
    pub fn replace_all_servers(&self, rows: &[ServerRow]) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启事务失败: {e}"))?;
        tx.execute_batch("DELETE FROM servers; DELETE FROM known_hosts;")
            .map_err(|e| format!("清空服务器失败: {e}"))?;
        for r in rows {
            tx.execute(
                &format!(
                    "INSERT INTO servers({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"
                ),
                params![
                    r.id,
                    r.name,
                    r.host,
                    r.port as i64,
                    r.username,
                    r.auth_type,
                    r.secret_blob,
                    r.group_name,
                    r.tags,
                    r.notes,
                    r.allow_ai as i64,
                    r.created_at,
                    r.updated_at
                ],
            )
            .map_err(|e| format!("插入失败: {e}"))?;
        }
        tx.commit().map_err(|e| format!("提交事务失败: {e}"))
    }

    /// 备份恢复-合并模式：按 id 存在则整体更新，不存在则插入。
    pub fn upsert_server(&self, r: &ServerRow) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            &format!(
                "INSERT INTO servers({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) \
                 ON CONFLICT(id) DO UPDATE SET name=?2, host=?3, port=?4, username=?5, auth_type=?6, \
                 secret_blob=?7, group_name=?8, tags=?9, notes=?10, allow_ai=?11, updated_at=?13"
            ),
            params![
                r.id,
                r.name,
                r.host,
                r.port as i64,
                r.username,
                r.auth_type,
                r.secret_blob,
                r.group_name,
                r.tags,
                r.notes,
                r.allow_ai as i64,
                r.created_at,
                r.updated_at
            ],
        )
        .map_err(|e| format!("合并失败: {e}"))?;
        Ok(())
    }

    // ---------- known_hosts（TOFU） ----------

    pub fn get_host_fingerprint(&self, server_id: &str) -> Result<Option<String>, String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT fingerprint FROM known_hosts WHERE server_id=?1",
            params![server_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("查询指纹失败: {e}"))
    }

    pub fn set_host_fingerprint(&self, server_id: &str, fp: &str, ts: &str) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO known_hosts(server_id, fingerprint, first_seen) VALUES (?1,?2,?3)
             ON CONFLICT(server_id) DO UPDATE SET fingerprint = excluded.fingerprint",
            params![server_id, fp, ts],
        )
        .map_err(|e| format!("记录指纹失败: {e}"))?;
        Ok(())
    }

    pub fn clear_host_fingerprint(&self, server_id: &str) -> Result<bool, String> {
        let conn = self.lock()?;
        let n = conn
            .execute("DELETE FROM known_hosts WHERE server_id=?1", params![server_id])
            .map_err(|e| format!("清除指纹失败: {e}"))?;
        Ok(n > 0)
    }

    // ---------- meta ----------

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.lock()?;
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .map_err(|e| format!("读取 meta 失败: {e}"))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| format!("写入 meta 失败: {e}"))?;
        Ok(())
    }

    // ---------- 审计 ----------

    pub fn insert_audit(
        &self,
        ts: &str,
        source: &str,
        server_id: Option<&str>,
        action: &str,
        detail: &str,
        outcome: &str,
    ) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO audit_log(ts, source, server_id, action, detail, outcome) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![ts, source, server_id, action, detail, outcome],
        )
        .map_err(|e| format!("写审计失败: {e}"))?;
        Ok(())
    }

    pub fn audit_list(&self, limit: i64, source: Option<&str>) -> Result<Vec<AuditEntry>, String> {
        let conn = self.lock()?;
        let limit = limit.clamp(1, 1000);
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<AuditEntry> {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                source: r.get(2)?,
                server_id: r.get(3)?,
                action: r.get(4)?,
                detail: r.get(5)?,
                outcome: r.get(6)?,
            })
        };
        let mut entries = Vec::new();
        match source {
            Some(s) if s != "all" => {
                let mut stmt = conn
                    .prepare("SELECT id,ts,source,server_id,action,detail,outcome FROM audit_log WHERE source=?1 ORDER BY id DESC LIMIT ?2")
                    .map_err(|e| format!("查询审计失败: {e}"))?;
                let rows = stmt
                    .query_map(params![s, limit], map_row)
                    .map_err(|e| format!("查询审计失败: {e}"))?;
                for r in rows {
                    entries.push(r.map_err(|e| e.to_string())?);
                }
            }
            _ => {
                let mut stmt = conn
                    .prepare("SELECT id,ts,source,server_id,action,detail,outcome FROM audit_log ORDER BY id DESC LIMIT ?1")
                    .map_err(|e| format!("查询审计失败: {e}"))?;
                let rows = stmt
                    .query_map(params![limit], map_row)
                    .map_err(|e| format!("查询审计失败: {e}"))?;
                for r in rows {
                    entries.push(r.map_err(|e| e.to_string())?);
                }
            }
        }
        Ok(entries)
    }

    /// 按 RFC3339 时间戳清理审计记录（ts 为字典序可比的 UTC 字符串）。
    /// source 传 Some 仅清理指定来源（如 AI），None 清理全部。
    pub fn purge_audit_before(&self, ts: &str, source: Option<&str>) -> Result<usize, String> {
        let conn = self.lock()?;
        let n = match source {
            Some(s) => conn
                .execute(
                    "DELETE FROM audit_log WHERE ts < ?1 AND source = ?2",
                    params![ts, s],
                )
                .map_err(|e| format!("清理审计失败: {e}"))?,
            None => conn
                .execute("DELETE FROM audit_log WHERE ts < ?1", params![ts])
                .map_err(|e| format!("清理审计失败: {e}"))?,
        };
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_audit_respects_cutoff_and_source() {
        let db = std::env::temp_dir().join(format!("sd-audit-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");

        let old = "2026-11-01T00:00:00.000000+00:00";
        let recent = "2026-11-16T00:00:00.000000+00:00";
        for (ts, source, action) in [
            (old, "ai", "mcp_exec_command"),
            (old, "manual", "ssh_exec"),
            (recent, "ai", "mcp_list_servers"),
            (recent, "user", "vault_unlock"),
        ] {
            store.insert_audit(ts, source, None, action, "d", "ok").unwrap();
        }
        // 清理 7 天前的 AI 记录
        let n = store
            .purge_audit_before("2026-11-10T00:00:00.000000+00:00", Some("ai"))
            .unwrap();
        assert_eq!(n, 1);
        // 旧的 manual 仍在；新的 ai 仍在
        assert_eq!(store.audit_list(100, Some("manual")).unwrap().len(), 1);
        assert_eq!(store.audit_list(100, Some("ai")).unwrap().len(), 1);
        // 全量清理剩余旧行（cutoff 覆盖 recent 前）
        let n = store
            .purge_audit_before("2026-11-15T00:00:00.000000+00:00", None)
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.audit_list(100, None).unwrap().len(), 2);

        let _ = std::fs::remove_file(&db);
    }
}
