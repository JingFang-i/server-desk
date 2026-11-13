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
}
