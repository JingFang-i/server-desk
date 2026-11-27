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
    updated_at  TEXT NOT NULL,
    -- 列表排序（drag & drop）：同一分组内按 sort_order 升序，新建行排到末尾。
    -- 旧库由 open() 里的增量迁移补列，默认 0（等价于原来的 created_at DESC）。
    sort_order  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_servers_group ON servers(group_name);
CREATE TABLE IF NOT EXISTS audit_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        TEXT NOT NULL,
    source    TEXT NOT NULL,
    server_id TEXT,
    server_name TEXT NOT NULL DEFAULT '',
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
CREATE TABLE IF NOT EXISTS ai_review_log (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  ts             TEXT NOT NULL,
  session        TEXT NOT NULL DEFAULT '',
  tool           TEXT NOT NULL,
  server_id      TEXT,
  server_name    TEXT NOT NULL DEFAULT '',
  command_hash   TEXT NOT NULL,
  command_digest TEXT NOT NULL,
  static_verdict  TEXT NOT NULL,
  ai_verdict     TEXT NOT NULL,
  final_verdict  TEXT NOT NULL,
  risk           TEXT NOT NULL DEFAULT '',
  reason         TEXT NOT NULL DEFAULT '',
  categories     TEXT NOT NULL DEFAULT '[]',
  model          TEXT NOT NULL DEFAULT '',
  latency_ms     INTEGER NOT NULL DEFAULT 0,
  tokens_in      INTEGER NOT NULL DEFAULT 0,
  tokens_out     INTEGER NOT NULL DEFAULT 0,
  cached         INTEGER NOT NULL DEFAULT 0,
  error          TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_ai_review_ts ON ai_review_log(ts DESC);
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
    /// 分组内排序位置（拖拽排序）。旧库迁移后统一为 0，等价于原来的时间倒序。
    pub sort_order: i64,
}

const COLS: &str = "id,name,host,port,username,auth_type,secret_blob,group_name,tags,notes,allow_ai,created_at,updated_at,sort_order";

pub(crate) fn group_matches(name: &str, group: &str) -> bool {
    name.trim().eq_ignore_ascii_case(group.trim())
}

/// 审计分页查询结果：total 供前端渲染分页器。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub ts: String,
    pub source: String,
    pub server_id: Option<String>,
    /// 写入时的服务器名称快照（服务器改名/删除后仍能看懂历史记录）
    pub server_name: String,
    pub action: String,
    pub detail: String,
    pub outcome: String,
    pub session: String,
    /// 由 SQL 统一判定「是否成功」：前端不再靠 `outcome.startsWith("ok")` 猜。
    /// 成功 = ok / ok:... / exit=Some(0)（且未读超时）。
    pub ok: bool,
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
        sort_order: row.get(13)?,
    })
}

pub struct Store {
    conn: Mutex<Connection>,
}

// 保留旧记录供原主机核对。升级和备份恢复依据同一套规则判断受影响资产。
fn legacy_secret_assets(conn: &Connection) -> rusqlite::Result<std::collections::HashSet<String>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='secret_refs')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(Default::default());
    }
    let has_copy_flag: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('secret_refs') WHERE name='remote_copy')",
        [],
        |row| row.get(0),
    )?;
    let query = if has_copy_flag {
        "SELECT DISTINCT server_id FROM secret_refs WHERE remote_copy=1"
    } else {
        "SELECT DISTINCT server_id FROM secret_refs"
    };
    let mut stmt = conn.prepare(query)?;
    let ids = stmt.query_map([], |row| row.get(0))?;
    ids.collect()
}

fn retire_legacy_secret_refs(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let retired: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM meta WHERE key='legacy_secret_refs_retired')",
        [],
        |row| row.get(0),
    )?;
    if retired {
        return Ok(());
    }
    let assets = legacy_secret_assets(&tx)?;
    if assets.is_empty() {
        return Ok(());
    }
    for id in assets {
        tx.execute("UPDATE servers SET allow_ai=0 WHERE id=?1", [id])?;
    }
    tx.execute("INSERT INTO meta(key,value) VALUES ('legacy_secret_refs_retired','1')", [])?;
    tx.commit()
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

        // 审计表增量迁移（旧库自动升级）：
        // R5 → session（AI 会话归因）；R6 → server_name（服务器名称快照）
        {
            let mut stmt = conn
                .prepare("PRAGMA table_info(audit_log)")
                .map_err(|e| format!("检查审计表失败: {e}"))?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("检查审计表失败: {e}"))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| format!("检查审计表失败: {e}"))?;
            if !cols.iter().any(|c| c == "session") {
                conn.execute_batch("ALTER TABLE audit_log ADD COLUMN session TEXT NOT NULL DEFAULT '';")
                    .map_err(|e| format!("迁移审计表失败: {e}"))?;
            }
            if !cols.iter().any(|c| c == "server_name") {
                conn.execute_batch(
                    "ALTER TABLE audit_log ADD COLUMN server_name TEXT NOT NULL DEFAULT '';",
                )
                .map_err(|e| format!("迁移审计表失败: {e}"))?;
            }
        }
        retire_legacy_secret_refs(&conn).map_err(|e| format!("迁移旧版凭据引用失败: {e}"))?;

        // 列表排序列（拖拽排序）：旧库补列，已有行统一 0 → 保持原来的时间倒序观感
        {
            let mut stmt = conn
                .prepare("PRAGMA table_info(servers)")
                .map_err(|e| format!("检查服务器表失败: {e}"))?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("检查服务器表失败: {e}"))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| format!("检查服务器表失败: {e}"))?;
            if !cols.iter().any(|c| c == "sort_order") {
                conn.execute_batch(
                    "ALTER TABLE servers ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;",
                )
                .map_err(|e| format!("迁移服务器排序列失败: {e}"))?;
            }
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_servers_sort ON servers(sort_order);",
            )
            .map_err(|e| format!("创建排序索引失败: {e}"))?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "数据库锁不可用".to_string())
    }

    // ---------- 服务器资产 ----------

    pub fn list(&self) -> Result<Vec<ServerRow>, String> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLS} FROM servers ORDER BY sort_order ASC, created_at DESC");
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
        let sql = format!("SELECT {COLS} FROM servers WHERE lower(trim(group_name)) = lower(trim(?1)) ORDER BY sort_order ASC, created_at DESC");
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

    /// 新增服务器：sort_order 取当前最大值 +1（排到列表末尾），
    /// 让「新增」不会插到用户手工排序的中间。
    pub fn insert(&self, r: &ServerRow) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            &format!(
                "INSERT INTO servers({COLS}) VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13, \
                  (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM servers))"
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

    /// 应用前端拖拽结果：整表「分组归属 + 组内顺序」一次性落库。
    ///
    /// 传全量而不是增量是有意的：拖拽只改变列表长相，一个事务覆盖全部行，
    /// 不会留下「拖到一半、数据库只写了一半」的中间态。返回真实更新的行数。
    pub fn apply_layout(&self, items: &[(String, Option<String>, i64)]) -> Result<usize, String> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启排序事务失败: {e}"))?;
        let mut updated = 0usize;
        for (id, group, order) in items {
            updated += tx
                .execute(
                    "UPDATE servers SET group_name=?2, sort_order=?3 WHERE id=?1",
                    params![id, group, order],
                )
                .map_err(|e| format!("更新排序失败: {e}"))?;
        }
        tx.commit().map_err(|e| format!("提交排序失败: {e}"))?;
        Ok(updated)
    }

    /// 重命名分组：分组只是服务器行上的一个字符串列，改名 = 批量改这一列。
    /// 新名字与已有分组同名时等价于「合并分组」（返回受影响的服务器台数）。
    pub fn rename_group(
        &self,
        from: &str,
        to: &str,
        meta: &[(&str, &str)],
    ) -> Result<usize, String> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| format!("开启分组改名事务失败: {e}"))?;
        let groups = (|| -> rusqlite::Result<Vec<String>> {
            let mut stmt = tx.prepare("SELECT DISTINCT group_name FROM servers WHERE group_name IS NOT NULL")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect()
        })().map_err(|e| format!("读取服务器分组失败: {e}"))?;
        let mut updated = 0;
        for group in groups.iter().filter(|name| group_matches(name, from)) {
            updated += tx.execute(
                "UPDATE servers SET group_name=?2 WHERE group_name=?1",
                params![group, to],
            ).map_err(|e| format!("重命名分组失败: {e}"))?;
        }
        if updated > 0 {
            Self::write_meta_values(&tx, meta)?;
        }
        tx.commit().map_err(|e| format!("提交分组改名失败: {e}"))?;
        Ok(updated)
    }

    pub fn server_group(&self, id: &str) -> Result<Option<String>, String> {
        self.lock()?
            .query_row("SELECT group_name FROM servers WHERE id = ?1", params![id], |r| r.get(0))
            .map_err(|e| format!("读取服务器分组失败: {e}"))
    }

    /// 重置前快照：用 `VACUUM INTO` 生成一致性单文件备份（包含 WAL 中已提交的数据）。
    /// 文件内含密文凭据与保险库元数据，可直接用文件替换方式回滚。
    pub fn snapshot_to(&self, path: &std::path::Path) -> Result<(), String> {
        let conn = self.lock()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建备份目录失败: {e}"))?;
        }
        // VACUUM INTO 不支持绑定参数：路径按 SQLite 单引号规则转义
        let escaped = path.display().to_string().replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))
            .map_err(|e| format!("重置前快照失败: {e}"))?;
        Ok(())
    }

    /// 清空保险库关联的资产和审查数据。旧版远端副本记录继续用于恢复时的保护。
    pub fn wipe_all(&self) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| format!("开启重置事务失败: {e}"))?;
        tx.execute_batch("DELETE FROM servers; DELETE FROM known_hosts;
                          DELETE FROM audit_log; DELETE FROM ai_review_log;")
            .map_err(|e| format!("清空数据失败: {e}"))?;
        tx.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            [crate::review::config::META_CONFIG, crate::review::config::META_KEY_BLOB],
        ).map_err(|e| format!("清除审查配置失败: {e}"))?;
        tx.commit().map_err(|e| format!("提交重置失败: {e}"))
    }

    /// 按资产 ID 恢复备份。历史备份中的开关不能解除旧版凭据副本保护。
    pub fn restore_servers(&self, rows: &[ServerRow], overwrite: bool) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| format!("开启恢复事务失败: {e}"))?;
        let protected = legacy_secret_assets(&tx).map_err(|e| format!("查询旧版凭据资产失败: {e}"))?;
        if overwrite {
            tx.execute_batch("DELETE FROM servers; DELETE FROM known_hosts;")
                .map_err(|e| format!("清空服务器失败: {e}"))?;
        }
        for row in rows {
            tx.execute(
                &format!(
                    "INSERT INTO servers({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) \
                     ON CONFLICT(id) DO UPDATE SET name=?2, host=?3, port=?4, username=?5, auth_type=?6, \
                     secret_blob=?7, group_name=?8, tags=?9, notes=?10, allow_ai=?11, updated_at=?13, \
                     sort_order=?14"
                ),
                params![
                    row.id, row.name, row.host, row.port as i64, row.username, row.auth_type,
                    row.secret_blob, row.group_name, row.tags, row.notes,
                    row.allow_ai && !protected.contains(&row.id), row.created_at, row.updated_at,
                    row.sort_order,
                ],
            ).map_err(|e| format!("恢复服务器失败: {e}"))?;
        }
        tx.commit().map_err(|e| format!("提交恢复失败: {e}"))
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
        self.set_meta_values(&[(key, value)])
    }

    pub fn set_meta_values(&self, entries: &[(&str, &str)]) -> Result<(), String> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| format!("写入 meta 失败: {e}"))?;
        Self::write_meta_values(&tx, entries)?;
        tx.commit().map_err(|e| format!("提交 meta 失败: {e}"))
    }

    fn write_meta_values(conn: &Connection, entries: &[(&str, &str)]) -> Result<(), String> {
        for (key, value) in entries {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|e| format!("写入 meta 失败: {e}"))?;
        }
        Ok(())
    }

    // ---------- 审计 ----------

    pub fn insert_audit(
        &self,
        ts: &str,
        session: &str,
        source: &str,
        server_id: Option<&str>,
        server_name: &str,
        action: &str,
        detail: &str,
        outcome: &str,
    ) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO audit_log(ts, source, server_id, server_name, action, detail, outcome, session) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![ts, source, server_id, server_name, action, detail, outcome, session],
        )
        .map_err(|e| format!("写审计失败: {e}"))?;
        Ok(())
    }

    /// 查询审计记录。`source` / `session` 传空或 "all" 表示不过滤；
    /// 会话过滤在 SQL 层完成，不再只筛前端最近 200 行。
    /// 兼容入口：取最新 limit 条（第一页），变更记录页等仍在用。
    pub fn audit_list(
        &self,
        limit: i64,
        source: Option<&str>,
        session: Option<&str>,
        action: Option<&str>,
    ) -> Result<Vec<AuditEntry>, String> {
        Ok(self
            .audit_page(1, limit.min(1000), source, session, action)?
            .entries)
    }

    /// 服务端分页查询审计：SQL 层 LIMIT/OFFSET + COUNT(*)，
    /// 几百上千条也不再一次性塞进前端。
    pub fn audit_page(
        &self,
        page: i64,
        page_size: i64,
        source: Option<&str>,
        session: Option<&str>,
        action: Option<&str>,
    ) -> Result<AuditPage, String> {
        let conn = self.lock()?;
        let page = page.max(1);
        let page_size = page_size.clamp(10, 500);
        let source = source.filter(|s| !s.is_empty() && *s != "all").unwrap_or("");
        let session = session.filter(|s| !s.is_empty() && *s != "all").unwrap_or("");
        let action = action.filter(|s| !s.is_empty() && *s != "all").unwrap_or("");
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<AuditEntry> {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                source: r.get(2)?,
                server_id: r.get(3)?,
                server_name: r.get(4)?,
                action: r.get(5)?,
                detail: r.get(6)?,
                outcome: r.get(7)?,
                session: r.get(8)?,
                ok: r.get::<_, i64>(9)? != 0,
            })
        };
        // server_name 为空的历史记录：按 server_id 关联当前服务器名补全（改名/删除后可读性最好）
        const SELECT: &str = "SELECT a.id,a.ts,a.source,a.server_id,\
             COALESCE(NULLIF(a.server_name,''), s.name, ''),\
             a.action,a.detail,a.outcome,a.session,\
             CASE WHEN lower(a.outcome) = 'ok' \
                    OR lower(a.outcome) LIKE 'ok:%' \
                    OR lower(a.outcome) LIKE 'ok %' \
                    OR (lower(a.outcome) LIKE 'exit=some(0)%' \
                        AND lower(a.outcome) NOT LIKE '%timed_out%') \
                  THEN 1 ELSE 0 END \
             FROM audit_log a LEFT JOIN servers s ON s.id = a.server_id";
        const WHERE: &str = "WHERE (?1 = '' OR a.source = ?1) AND (?2 = '' OR a.session = ?2) \
              AND (?3 = '' OR a.action = ?3)";
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM audit_log a LEFT JOIN servers s ON s.id = a.server_id {WHERE}"),
                params![source, session, action],
                |r| r.get(0),
            )
            .map_err(|e| format!("查询审计失败: {e}"))?;
        let offset = (page - 1) * page_size;
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT} {WHERE} ORDER BY a.id DESC LIMIT ?4 OFFSET ?5"
            ))
            .map_err(|e| format!("查询审计失败: {e}"))?;
        let rows = stmt
            .query_map(params![source, session, action, page_size, offset], map_row)
            .map_err(|e| format!("查询审计失败: {e}"))?;
        let mut entries = Vec::new();
        for r in rows {
            entries.push(r.map_err(|e| e.to_string())?);
        }
        Ok(AuditPage { entries, total, page, page_size })
    }

    /// 出现过的 AI 会话 id（按最近活动排序），供审计页做会话筛选。
    pub fn audit_sessions(&self, limit: i64) -> Result<Vec<String>, String> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT session FROM audit_log WHERE session <> '' \
                 GROUP BY session ORDER BY MAX(ts) DESC LIMIT ?1",
            )
            .map_err(|e| format!("查询会话失败: {e}"))?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 500)], |r| r.get::<_, String>(0))
            .map_err(|e| format!("查询会话失败: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
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

    // ---------- AI 审查日志（R8.5） ----------

    /// 写入一条审查记录。正文只存脱敏摘要与 hash，不落原始命令全文（设计 §3.3）。
    pub fn insert_review_log(&self, r: &ReviewLogRow) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO ai_review_log (ts,session,tool,server_id,server_name,command_hash,\
             command_digest,static_verdict,ai_verdict,final_verdict,risk,reason,categories,\
             model,latency_ms,tokens_in,tokens_out,cached,error) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                r.ts,
                r.session,
                r.tool,
                r.server_id,
                r.server_name,
                r.command_hash,
                r.command_digest,
                r.static_verdict,
                r.ai_verdict,
                r.final_verdict,
                r.risk,
                r.reason,
                serde_json::to_string(&r.categories).unwrap_or_else(|_| "[]".into()),
                r.model,
                r.latency_ms,
                r.tokens_in,
                r.tokens_out,
                r.cached as i64,
                r.error
            ],
        )
        .map_err(|e| format!("写入审查日志失败: {e}"))?;
        Ok(())
    }

    /// 最近审查记录（新→旧），设置页「最近审查记录」与排查用。
    pub fn recent_reviews(&self, limit: i64) -> Result<Vec<crate::review::ReviewEntry>, String> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT id,ts,tool,server_name,ai_verdict,final_verdict,risk,reason,categories,\
                 model,latency_ms,cached,error FROM ai_review_log ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| format!("查询审查日志失败: {e}"))?;
        let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<crate::review::ReviewEntry> {
            Ok(crate::review::ReviewEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                tool: r.get(2)?,
                server_name: r.get(3)?,
                ai_verdict: r.get(4)?,
                final_verdict: r.get(5)?,
                risk: r.get(6)?,
                reason: r.get(7)?,
                categories: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                model: r.get(9)?,
                latency_ms: r.get(10)?,
                cached: r.get::<_, i64>(11)? != 0,
                error: r.get(12)?,
            })
        };
        let rows = stmt
            .query_map(params![limit.clamp(1, 100)], map)
            .map_err(|e| format!("查询审查日志失败: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

/// 一条审查日志的写入载荷（R8.5）。
#[derive(Debug, Clone)]
pub struct ReviewLogRow {
    pub ts: String,
    pub session: String,
    pub tool: String,
    pub server_id: Option<String>,
    pub server_name: String,
    /// sha256(tool|命令|静态判定|prompt 版本|模型) 的十六进制，用于命中率统计
    pub command_hash: String,
    /// 脱敏后的命令摘要（≤200 字），不落原始全文
    pub command_digest: String,
    pub static_verdict: String,
    /// allow | review | deny | error | skipped
    pub ai_verdict: String,
    /// allow | approval | blocked
    pub final_verdict: String,
    pub risk: String,
    pub reason: String,
    pub categories: Vec<String>,
    pub model: String,
    pub latency_ms: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cached: bool,
    /// 降级/跳过的短标签（http/parse/breaker/key_error/no_key/blocked_by_static/rate_limited）
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_refs_only_disable_affected_assets_on_first_upgrade() {
        for has_copy_flag in [false, true] {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE servers(id TEXT, allow_ai INTEGER);
                 CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO servers VALUES ('remote',1),('local',1),('other',1);",
            ).unwrap();
            retire_legacy_secret_refs(&conn).unwrap();
            conn.execute_batch(
                "CREATE TABLE secret_refs(server_id TEXT);
                 INSERT INTO secret_refs VALUES ('remote'),('local');",
            ).unwrap();
            if has_copy_flag {
                conn.execute_batch(
                    "ALTER TABLE secret_refs ADD COLUMN remote_copy INTEGER DEFAULT 1;
                     UPDATE secret_refs SET remote_copy=0 WHERE server_id='local';",
                ).unwrap();
            }
            retire_legacy_secret_refs(&conn).unwrap();
            let enabled = |id: &str| -> bool {
                conn.query_row("SELECT allow_ai FROM servers WHERE id=?1", [id], |r| r.get(0)).unwrap()
            };
            assert!(!enabled("remote"));
            assert_eq!(enabled("local"), has_copy_flag);
            assert!(enabled("other"));
            conn.execute("UPDATE servers SET allow_ai=1 WHERE id='remote'", []).unwrap();
            retire_legacy_secret_refs(&conn).unwrap();
            assert!(enabled("remote"));
        }
    }

    #[test]
    fn restoring_a_backup_preserves_legacy_asset_protection() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store.lock().unwrap().execute_batch(
            "CREATE TABLE secret_refs(server_id TEXT, remote_copy INTEGER);
             INSERT INTO secret_refs VALUES ('remote',1), ('local',0);
             INSERT INTO meta VALUES ('legacy_secret_refs_retired','1');",
        ).unwrap();
        let row = |id: &str| ServerRow {
            id: id.into(), name: id.into(), host: "localhost".into(), port: 22,
            username: "test".into(), auth_type: "password".into(), secret_blob: vec![],
            group_name: None, tags: "[]".into(), notes: None, allow_ai: true,
            created_at: "2026-11-25T00:00:00Z".into(), updated_at: "2026-11-25T00:00:00Z".into(),
            sort_order: 0,
        };
        let rows = [row("remote"), row("local"), row("other")];
        for overwrite in [false, true] {
            store.restore_servers(&rows, overwrite).unwrap();
            assert!(!store.get("remote").unwrap().unwrap().allow_ai);
            assert!(store.get("local").unwrap().unwrap().allow_ai);
            assert!(store.get("other").unwrap().unwrap().allow_ai);
        }
    }

    #[test]
    fn layout_reorder_and_group_rename_round_trip() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let mk = |id: &str| ServerRow {
            id: id.into(), name: id.into(), host: "localhost".into(), port: 22,
            username: "test".into(), auth_type: "password".into(), secret_blob: vec![],
            group_name: Some("prod".into()), tags: "[]".into(), notes: None, allow_ai: true,
            created_at: "2026-11-25T00:00:00Z".into(), updated_at: "2026-11-25T00:00:00Z".into(),
            sort_order: 0,
        };
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let mut row = mk(*id);
            row.created_at = format!("2026-11-25T00:00:0{i}Z");
            store.insert(&row).unwrap();
        }
        // 新插入的行排在末尾（sort_order = max + 1）
        let ids = |rows: &[ServerRow]| rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&store.list().unwrap()), vec!["a", "b", "c"]);

        // 拖拽：c 提到最前、a 归入「生产」、b 落到未分组
        store
            .apply_layout(&[
                ("c".into(), Some("prod".into()), 0),
                ("a".into(), Some("生产".into()), 1),
                ("b".into(), None, 2),
            ])
            .unwrap();
        let rows = store.list().unwrap();
        assert_eq!(ids(&rows), vec!["c", "a", "b"], "组内顺序按 sort_order");
        assert_eq!(rows[0].group_name.as_deref(), Some("prod"));
        assert_eq!(rows[1].group_name.as_deref(), Some("生产"));
        assert_eq!(rows[2].group_name, None, "拖进未分组要清空 group_name");

        // 重命名分组：大小写/首尾空格不敏感，返回受影响台数（只有 c 的分组是 prod）
        assert_eq!(store.rename_group(" PROD ", "生产", &[]).unwrap(), 1);
        let groups: Vec<Option<String>> = store.list().unwrap().into_iter().map(|r| r.group_name).collect();
        assert_eq!(groups, vec![Some("生产".into()), Some("生产".into()), None]);
        assert_eq!(store.rename_group("不存在", "x", &[]).unwrap(), 0);
    }

    #[test]
    fn review_log_insert_and_recent() {
        let db = std::env::temp_dir().join(format!("sd-review-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");
        let mk = |ts: &str, ai: &str, cached: bool| ReviewLogRow {
            ts: ts.into(),
            session: "s-x".into(),
            tool: "exec_command".into(),
            server_id: Some("srv".into()),
            server_name: "web-01".into(),
            command_hash: "ab12".into(),
            command_digest: "uptime".into(),
            static_verdict: "allow".into(),
            ai_verdict: ai.into(),
            final_verdict: "allow".into(),
            risk: "low".into(),
            reason: "r".into(),
            categories: vec!["data".into(), "network".into()],
            model: "mini".into(),
            latency_ms: 42,
            tokens_in: 300,
            tokens_out: 24,
            cached,
            error: String::new(),
        };
        store
            .insert_review_log(&mk("2026-11-25T01:00:00+00:00", "allow", false))
            .unwrap();
        store
            .insert_review_log(&mk("2026-11-25T02:00:00+00:00", "deny", true))
            .unwrap();
        let all = store.recent_reviews(10).unwrap();
        assert_eq!(all.len(), 2, "两条都应可查");
        assert_eq!(all[0].ai_verdict, "deny", "新→旧排序");
        assert!(all[0].cached, "cached 标志落库往返");
        assert_eq!(all[0].categories, vec!["data", "network"], "categories JSON 往返");
        assert_eq!(all[0].latency_ms, 42);
        assert_eq!(all[0].server_name, "web-01");
        assert_eq!(store.recent_reviews(1).unwrap().len(), 1, "limit 生效");
        store.set_meta(crate::review::config::META_CONFIG, "fixture").unwrap();
        store.set_meta(crate::review::config::META_KEY_BLOB, "fixture").unwrap();
        store.wipe_all().unwrap();
        assert!(store.recent_reviews(10).unwrap().is_empty());
        assert!(store.get_meta(crate::review::config::META_CONFIG).unwrap().is_none());
        assert!(store.get_meta(crate::review::config::META_KEY_BLOB).unwrap().is_none());
    }

    #[test]
    fn pre_reset_snapshot_keeps_data() {
        let db = std::env::temp_dir().join(format!("sd-snap-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");
        store
            .insert_audit("2026-11-25T00:00:00+00:00", "", "user", None, "", "ssh_exec", "d", "ok")
            .unwrap();
        let snap = std::env::temp_dir().join(format!("sd-snap-out-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&snap);
        store.snapshot_to(&snap).expect("snapshot");

        // 快照是独立文件：主库被清空后，快照仍保留数据
        store.wipe_all().unwrap();
        assert_eq!(store.audit_list(10, None, None, None).unwrap().len(), 0);
        let restored = Store::open(&snap).expect("open snapshot");
        assert_eq!(
            restored.audit_list(10, None, None, None).unwrap().len(),
            1,
            "重置前快照必须保留数据"
        );
        let _ = std::fs::remove_file(&snap);
    }

    #[test]
    fn purge_audit_respects_cutoff_and_source() {
        let db = std::env::temp_dir().join(format!("sd-audit-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");

        let old = "2026-11-01T00:00:00.000000+00:00";
        let recent = "2026-11-16T00:00:00.000000+00:00";
        for (ts, source, action) in [
            (old, "ai", shared::action::MCP_EXEC_COMMAND),
            (old, "manual", "ssh_exec"),
            (recent, "ai", shared::action::MCP_LIST_SERVERS),
            (recent, "user", "vault_unlock"),
        ] {
            store.insert_audit(ts, "", source, None, "", action, "d", "ok").unwrap();
        }
        // 清理 7 天前的 AI 记录
        let n = store
            .purge_audit_before("2026-11-10T00:00:00.000000+00:00", Some("ai"))
            .unwrap();
        assert_eq!(n, 1);
        // 旧的 manual 仍在；新的 ai 仍在
        assert_eq!(store.audit_list(100, Some("manual"), None, None).unwrap().len(), 1);
        assert_eq!(store.audit_list(100, Some("ai"), None, None).unwrap().len(), 1);
        // 全量清理剩余旧行（cutoff 覆盖 recent 前）
        let n = store
            .purge_audit_before("2026-11-15T00:00:00.000000+00:00", None)
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.audit_list(100, None, None, None).unwrap().len(), 2);

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn audit_session_filter_and_ok_flag() {
        let db = std::env::temp_dir().join(format!("sd-audit-sess-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");
        // 时间戳错开：会话下拉按最近活动排序，同一时刻会并列
        let t1 = "2026-11-23T00:00:01.000000+00:00";
        let t2 = "2026-11-23T00:00:02.000000+00:00";
        let t3 = "2026-11-23T00:00:03.000000+00:00";
        let t4 = "2026-11-23T00:00:04.000000+00:00";
        store.insert_audit(t1, "s-aaa", "ai", None, "", shared::action::MCP_EXEC_COMMAND, "d", "ok").unwrap();
        store.insert_audit(t2, "s-bbb", "ai", None, "", shared::action::MCP_EXEC_COMMAND, "d", "blocked").unwrap();
        store.insert_audit(t3, "", "ai", None, "", shared::action::MCP_EXEC_COMMAND, "d", "exit=Some(0)").unwrap();
        store.insert_audit(t4, "s-bbb", "ai", None, "", shared::action::MCP_EXEC_COMMAND, "d", "exit=Some(0) timed_out").unwrap();

        // 会话筛选在 SQL 层完成
        let a = store.audit_list(100, Some("ai"), Some("s-aaa"), None).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].session, "s-aaa");
        assert!(a[0].ok);

        let b = store.audit_list(100, None, Some("s-bbb"), None).unwrap();
        assert_eq!(b.len(), 2);
        // blocked / timed_out 不算成功，"exit=Some(0)" 算成功
        assert!(b.iter().all(|r| !r.ok));

        let none = store.audit_list(100, Some("ai"), None, None).unwrap();
        assert_eq!(none.len(), 4);
        assert_eq!(none.iter().filter(|r| r.ok).count(), 2);

        let sessions = store.audit_sessions(10).unwrap();
        // 按最近活动倒序：s-bbb 最后一次活动更晚
        assert_eq!(sessions, vec!["s-bbb".to_string(), "s-aaa".to_string()]);

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn audit_page_pagination_and_total() {
        let db = std::env::temp_dir().join(format!("sd-audit-page-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).expect("store");
        // page_size 下限是 10，这里插 25 条、每页 10 条：3 页（10/10/5）
        for i in 0..25u32 {
            let ts = format!("2026-11-23T00:{:02}:{:02}.000000+00:00", i / 60, i % 60);
            store
                .insert_audit(&ts, "s-p", "ai", None, "", "mcp_exec_command", "d", "ok")
                .unwrap();
        }
        let p1 = store.audit_page(1, 10, Some("ai"), Some("s-p"), None).unwrap();
        assert_eq!(p1.total, 25);
        assert_eq!(p1.entries.len(), 10);
        let p3 = store.audit_page(3, 10, Some("ai"), Some("s-p"), None).unwrap();
        assert_eq!(p3.entries.len(), 5);
        let empty = store.audit_page(9, 10, Some("ai"), Some("s-p"), None).unwrap();
        assert!(empty.entries.is_empty());
        assert_eq!(empty.total, 25);
        // session 筛选影响 total
        let none = store.audit_page(1, 10, Some("ai"), Some("other"), None).unwrap();
        assert_eq!(none.total, 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn audit_server_name_snapshot_and_legacy_migration() {
        let db = std::env::temp_dir().join(format!("sd-audit-name-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        // 先造一个 R5 之前的老库（audit_log 无 server_name / session 列）
        {
            let conn = Connection::open(&db).expect("legacy db");
            conn.execute_batch(
                "CREATE TABLE audit_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts TEXT NOT NULL, source TEXT NOT NULL, server_id TEXT,
                    action TEXT NOT NULL, detail TEXT NOT NULL, outcome TEXT NOT NULL
                );",
            )
            .expect("legacy schema");
        }
        let store = Store::open(&db).expect("store");
        store
            .insert_audit(
                "2026-11-23T00:00:00+00:00",
                "",
                "user",
                Some("srv-1"),
                "生产 A",
                "ssh_exec",
                "uptime",
                "ok",
            )
            .unwrap();
        let rows = store.audit_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].server_id.as_deref(), Some("srv-1"));
        assert_eq!(rows[0].server_name, "生产 A");

        let _ = std::fs::remove_file(&db);
    }
}
