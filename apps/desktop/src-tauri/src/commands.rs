//! Tauri IPC 命令层：前端唯一入口。
//! 凭据仅在解密后短暂存在于内存；用户本人的增删改查会经由前端表单往返（属预期行为）。
//! AI 侧走 MCP sidecar → ipc → ops 安全管线，结果不含凭据且已脱敏。

use crate::guard::GuardRules;
use crate::ops;
use crate::state::AppState;
use crate::vault::VaultStatus;
use chrono::Utc;
use shared::{AuthMethod, ServerConfig, TestResult};
use std::sync::Arc;
use tauri::{Emitter, State};
use uuid::Uuid;

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn default_true() -> bool {
    true
}

// ---------- 保险库 ----------

#[tauri::command]
pub fn vault_status(state: State<'_, Arc<AppState>>) -> Result<VaultStatus, String> {
    Ok(state.vault.status())
}

#[tauri::command]
pub fn vault_setup(state: State<'_, Arc<AppState>>, password: String) -> Result<Option<String>, String> {
    state.vault.setup(&password).map_err(|e| e.to_string())?;
    let mig = state
        .vault
        .migrate_legacy_if_needed(&state.store)
        .map_err(|e| e.to_string())?;
    ops::audit(&state, "user", None, "vault_setup", "初始化主密码保险库", "ok");
    Ok(mig.summary())
}

#[tauri::command]
pub fn vault_unlock(state: State<'_, Arc<AppState>>, password: String) -> Result<Option<String>, String> {
    state.vault.unlock(&password).map_err(|e| e.to_string())?;
    let mig = state
        .vault
        .migrate_legacy_if_needed(&state.store)
        .map_err(|e| e.to_string())?;
    ops::audit(&state, "user", None, "vault_unlock", "解锁保险库", "ok");
    Ok(mig.summary())
}

#[tauri::command]
pub fn vault_lock(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.vault.lock();
    ops::audit(&state, "user", None, "vault_lock", "手动锁定保险库", "ok");
    Ok(())
}

#[tauri::command]
pub fn vault_set_settings(
    state: State<'_, Arc<AppState>>,
    auto_lock_minutes: u64,
    keychain_auto_unlock: bool,
) -> Result<VaultStatus, String> {
    state
        .vault
        .set_settings(auto_lock_minutes, keychain_auto_unlock)
        .map_err(|e| e.to_string())?;
    ops::audit(
        &state,
        "user",
        None,
        "vault_settings",
        &format!("自动锁 {auto_lock_minutes} 分钟，钥匙串自动解锁 {keychain_auto_unlock}"),
        "ok",
    );
    Ok(state.vault.status())
}

/// 危险操作：忘记主密码时的重置（清空全部数据，不可恢复）。
#[tauri::command]
pub fn vault_reset(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.store.wipe_all()?;
    state.vault.reset().map_err(|e| e.to_string())?;
    ops::audit(&state, "user", None, "vault_reset", "重置保险库并清空数据", "ok");
    Ok(())
}

// ---------- 服务器资产 ----------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveServerInput {
    pub id: Option<String>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub group: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    #[serde(default = "default_true")]
    pub allow_ai: bool,
}

fn validate(input: &SaveServerInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if input.host.trim().is_empty() {
        return Err("主机地址不能为空".into());
    }
    if input.username.trim().is_empty() {
        return Err("用户名不能为空".into());
    }
    if input.port == 0 {
        return Err("端口不合法".into());
    }
    match &input.auth {
        AuthMethod::Password { password } if password.is_empty() => Err("密码不能为空".into()),
        AuthMethod::Key { private_key, .. } if private_key.trim().is_empty() => {
            Err("私钥内容不能为空".into())
        }
        _ => Ok(()),
    }
}

fn row_to_config(
    dek: &[u8; crate::crypto::KEY_LEN],
    r: &crate::store::ServerRow,
) -> Result<ServerConfig, String> {
    let plain = crate::crypto::decrypt(dek, &r.secret_blob)?;
    let auth: AuthMethod =
        serde_json::from_slice(&plain).map_err(|e| format!("凭据数据损坏: {e}"))?;
    let tags: Vec<String> = serde_json::from_str(&r.tags).unwrap_or_default();
    Ok(ServerConfig {
        id: r.id.clone(),
        name: r.name.clone(),
        host: r.host.clone(),
        port: r.port,
        username: r.username.clone(),
        auth,
        group: r.group_name.clone(),
        tags,
        notes: r.notes.clone(),
        allow_ai: r.allow_ai,
        created_at: r.created_at.clone(),
        updated_at: r.updated_at.clone(),
    })
}

#[tauri::command]
pub fn list_servers(state: State<'_, Arc<AppState>>) -> Result<Vec<ServerConfig>, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let rows = state.store.list()?;
    rows.iter().map(|r| row_to_config(&dek, r)).collect()
}

#[tauri::command]
pub fn save_server(
    state: State<'_, Arc<AppState>>,
    input: SaveServerInput,
) -> Result<ServerConfig, String> {
    validate(&input)?;
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let is_new = input.id.is_none();
    let id = input
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let ts = now();
    let created_at = if is_new {
        ts.clone()
    } else {
        state
            .store
            .get(&id)?
            .map(|r| r.created_at)
            .unwrap_or_else(|| ts.clone())
    };

    let cred_json = serde_json::to_vec(&input.auth).map_err(|e| e.to_string())?;
    let blob = crate::crypto::encrypt(&dek, &cred_json)?;

    let row = crate::store::ServerRow {
        id,
        name: input.name.trim().to_string(),
        host: input.host.trim().to_string(),
        port: input.port,
        username: input.username.trim().to_string(),
        auth_type: input.auth.kind().to_string(),
        secret_blob: blob,
        group_name: input
            .group
            .clone()
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty()),
        tags: serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".into()),
        notes: input
            .notes
            .clone()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty()),
        allow_ai: input.allow_ai,
        created_at,
        updated_at: ts,
    };

    if is_new {
        state.store.insert(&row)?;
    } else {
        let updated = state.store.update(&row)?;
        if !updated {
            return Err("服务器不存在或已被删除".into());
        }
    }
    ops::audit(
        &state,
        "user",
        Some(&row.id),
        "save_server",
        &format!("保存服务器「{}」（{}）", row.name, if is_new { "新增" } else { "更新" }),
        "ok",
    );
    row_to_config(&dek, &row)
}

#[tauri::command]
pub async fn delete_server(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let deleted = state.store.delete(&id)?;
    if !deleted {
        return Err("服务器不存在或已被删除".into());
    }
    let _ = state.ssh.disconnect(&id).await;
    ops::audit(&state, "user", Some(&id), "delete_server", "删除服务器", "ok");
    Ok(())
}

// ---------- 连接测试 ----------

/// 表单内未保存服务器的 TCP 层探测。
#[tauri::command]
pub async fn test_connection(host: String, port: u16) -> Result<TestResult, String> {
    let host = host.trim().to_string();
    if host.is_empty() {
        return Err("主机地址不能为空".into());
    }
    let addr = format!("{host}:{port}");
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(&addr),
    )
    .await;
    let latency = started.elapsed().as_millis() as u64;
    match res {
        Ok(Ok(_)) => Ok(TestResult {
            ok: true,
            latency_ms: Some(latency),
            message: format!("TCP 连接成功（{addr}，{latency}ms）"),
        }),
        Ok(Err(e)) => Ok(TestResult {
            ok: false,
            latency_ms: Some(latency),
            message: format!("连接失败：{e}"),
        }),
        Err(_) => Ok(TestResult {
            ok: false,
            latency_ms: None,
            message: format!("连接超时（>5s）：{addr}"),
        }),
    }
}

/// 已保存服务器的完整 SSH 认证测试（建立连接后立即断开）。
#[tauri::command]
pub async fn ssh_test(state: State<'_, Arc<AppState>>, server_id: String) -> Result<TestResult, String> {
    let cfg = ops::load_server_config(&state, &server_id)?;
    let started = std::time::Instant::now();
    let res = state
        .ssh
        .establish(&state.store, &server_id, &ops::conn_params(&cfg))
        .await;
    let latency = started.elapsed().as_millis() as u64;
    match res {
        Ok(session) => {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "test", "en")
                .await;
            ops::audit(&state, "user", Some(&server_id), "ssh_test", "SSH 认证测试", "ok");
            Ok(TestResult {
                ok: true,
                latency_ms: Some(latency),
                message: format!("SSH 认证成功（{}，{latency}ms）", cfg.host),
            })
        }
        Err(e) => {
            ops::audit(
                &state,
                "user",
                Some(&server_id),
                "ssh_test",
                "SSH 认证测试",
                &truncate(&e, 300),
            );
            Ok(TestResult { ok: false, latency_ms: Some(latency), message: e })
        }
    }
}

// ---------- SSH 连接池 ----------

#[tauri::command]
pub async fn ssh_connect(state: State<'_, Arc<AppState>>, server_id: String) -> Result<(), String> {
    let cfg = ops::load_server_config(&state, &server_id)?;
    state
        .ssh
        .connect(&state.store, &server_id, ops::conn_params(&cfg))
        .await?;
    ops::audit(
        &state,
        "user",
        Some(&server_id),
        "ssh_connect",
        &format!("连接 {}@{}:{}", cfg.username, cfg.host, cfg.port),
        "ok",
    );
    Ok(())
}

#[tauri::command]
pub async fn ssh_disconnect(state: State<'_, Arc<AppState>>, server_id: String) -> Result<(), String> {
    let _ = state.ssh.disconnect(&server_id).await;
    ops::audit(&state, "user", Some(&server_id), "ssh_disconnect", "断开连接", "ok");
    Ok(())
}

#[tauri::command]
pub async fn ssh_status(state: State<'_, Arc<AppState>>) -> Result<Vec<crate::ssh::ConnInfo>, String> {
    Ok(state.ssh.status().await)
}

/// 手动命令执行：不经过 AI 拦截/审批，但写审计（detail 脱敏）。
#[tauri::command]
pub async fn ssh_exec(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    command: String,
    timeout_secs: Option<u64>,
) -> Result<crate::ssh::ExecResult, String> {
    let timeout = timeout_secs.unwrap_or(30);
    ops::run_command(&state, &server_id, &command, timeout, "manual").await
}

/// 重置某服务器的主机密钥指纹（确认服务器重装/换钥后使用）。
#[tauri::command]
pub fn ssh_reset_hostkey(state: State<'_, Arc<AppState>>, server_id: String) -> Result<(), String> {
    state.store.clear_host_fingerprint(&server_id)?;
    ops::audit(&state, "user", Some(&server_id), "ssh_reset_hostkey", "重置主机密钥指纹", "ok");
    Ok(())
}

// ---------- AI 安全管线（guard） ----------

#[tauri::command]
pub fn guard_info(state: State<'_, Arc<AppState>>) -> crate::guard::GuardInfo {
    state.guard.info()
}

#[tauri::command]
pub fn guard_set_rules(
    state: State<'_, Arc<AppState>>,
    deny: Vec<String>,
    approval: Vec<String>,
) -> Result<crate::guard::GuardInfo, String> {
    state
        .guard
        .set_rules(&state.store, GuardRules { deny, approval })
        .map_err(|e| e.to_string())?;
    ops::audit(&state, "user", None, "guard_set_rules", "更新安全规则", "ok");
    Ok(state.guard.info())
}

#[tauri::command]
pub fn mcp_set_enabled(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> Result<crate::guard::GuardInfo, String> {
    state
        .guard
        .set_ai_enabled(&state.store, enabled)
        .map_err(|e| e.to_string())?;
    ops::audit(
        &state,
        "user",
        None,
        "mcp_set_enabled",
        &format!("AI 接入 {}", if enabled { "开启" } else { "关闭" }),
        "ok",
    );
    Ok(state.guard.info())
}

// ---------- 审批 ----------

#[tauri::command]
pub fn approvals_pending(state: State<'_, Arc<AppState>>) -> Vec<crate::approvals::PendingApproval> {
    state.approvals.pending()
}

#[tauri::command]
pub fn approval_decide(
    state: State<'_, Arc<AppState>>,
    id: String,
    approve: bool,
) -> Result<(), String> {
    if !state.approvals.decide(&id, approve) {
        return Err("审批不存在或已过期".into());
    }
    let _ = state.handle.emit("approvals-updated", ());
    Ok(())
}

// ---------- 审计 ----------

#[tauri::command]
pub fn audit_list(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
    source: Option<String>,
) -> Result<Vec<crate::store::AuditEntry>, String> {
    state.store.audit_list(limit.unwrap_or(200), source.as_deref())
}

// ---------- MCP 接入信息 ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    pub port: u16,
    pub token: String,
    pub sidecar_path: String,
    pub config_json: String,
}

#[tauri::command]
pub fn mcp_info(state: State<'_, Arc<AppState>>) -> Result<McpInfo, String> {
    let port = std::fs::read_to_string(state.dir.join("ipc-port"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let token = std::fs::read_to_string(state.dir.join("ipc-token"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let sidecar_path = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let dir = exe.parent()?;
            // 开发：target/debug 同目录；macOS .app：Contents/Resources；Windows：安装目录
            let candidates = [
                dir.join("serverdesk-mcp"),
                dir.join("../Resources/serverdesk-mcp"),
                dir.join("serverdesk-mcp.exe"),
                dir.join("../Resources/serverdesk-mcp.exe"),
            ];
            candidates
                .iter()
                .find(|p| p.exists())
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| "serverdesk-mcp".into());
    let config_json = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "serverdesk": {
                "command": sidecar_path,
                "args": []
            }
        }
    }))
    .map_err(|e| e.to_string())?;
    Ok(McpInfo { port, token, sidecar_path, config_json })
}

// ---------- 备份与恢复 ----------

/// 读取用户通过文件对话框选择的私钥文件内容（仅明文 PEM/openssh 格式回填表单）。
#[tauri::command]
pub fn read_key_file(path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_file() {
        return Err("所选路径不是文件".into());
    }
    let content = std::fs::read_to_string(&p).map_err(|e| format!("读取私钥文件失败: {e}"))?;
    let trimmed = content.trim();
    if !trimmed.contains("PRIVATE KEY") {
        return Err("文件内容不像私钥（未找到 PRIVATE KEY 标记），请确认选择了正确的文件".into());
    }
    // 规范化行尾（CRLF → LF）并保留标准结尾换行，方便直接被 russh 解析
    let normalized = trimmed.replace("\r\n", "\n").to_string();
    Ok(format!("{normalized}\n"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSettings {
    pub enabled: bool,
    pub keep: u32,
    pub interval_hours: u32,
    pub last_run: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub settings: BackupSettings,
    pub files: Vec<crate::backup::BackupFileMeta>,
}

fn read_auto_settings(store: &crate::store::Store) -> BackupSettings {
    let meta = |k: &str| store.get_meta(k).ok().flatten();
    BackupSettings {
        enabled: meta("auto_backup_enabled").map(|s| s == "1").unwrap_or(false),
        keep: meta("auto_backup_keep").and_then(|s| s.parse().ok()).unwrap_or(7),
        interval_hours: meta("auto_backup_interval_hours")
            .and_then(|s| s.parse().ok())
            .unwrap_or(24),
        last_run: meta("auto_backup_last"),
    }
}

#[tauri::command]
pub fn backup_info(state: State<'_, Arc<AppState>>) -> Result<BackupInfo, String> {
    Ok(BackupInfo {
        settings: read_auto_settings(&state.store),
        files: crate::backup::list_auto_backups(&state.dir),
    })
}

#[tauri::command]
pub fn backup_export(
    state: State<'_, Arc<AppState>>,
    path: String,
    password: String,
) -> Result<crate::backup::ExportResult, String> {
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定，请先解锁".into());
    }
    let res = crate::backup::export_encrypted(&state.store, std::path::Path::new(&path), &password)?;
    ops::audit(&state, "user", None, "backup_export", &truncate(&path, 300), &format!("{} 台", res.count));
    Ok(res)
}

#[tauri::command]
pub fn backup_restore(
    state: State<'_, Arc<AppState>>,
    path: String,
    password: String,
    mode: String,
) -> Result<crate::backup::RestoreResult, String> {
    let payload = crate::backup::read_payload(std::path::Path::new(&path), &password)?;
    let res = crate::backup::restore_payload(&state.store, &payload, &mode)?;
    ops::audit(
        &state,
        "user",
        None,
        "backup_restore",
        &format!("{mode}:{}", truncate(&path, 200)),
        &format!("{} 台", res.imported),
    );
    Ok(res)
}

#[tauri::command]
pub fn auto_backup_now(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let s = read_auto_settings(&state.store);
    let path = crate::backup::run_auto_backup(&state.store, &state.vault, &state.dir, s.keep as usize)?;
    state.store.set_meta("auto_backup_last", &now())?;
    ops::audit(&state, "user", None, "auto_backup_now", "手动触发自动备份", "ok");
    Ok(path.display().to_string())
}

#[tauri::command]
pub fn auto_backup_save_settings(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
    keep: u32,
    interval_hours: u32,
) -> Result<BackupInfo, String> {
    state.store.set_meta("auto_backup_enabled", if enabled { "1" } else { "0" })?;
    state.store.set_meta("auto_backup_keep", &keep.clamp(1, 50).to_string())?;
    state.store.set_meta("auto_backup_interval_hours", &interval_hours.clamp(1, 168).to_string())?;
    ops::audit(
        &state,
        "user",
        None,
        "auto_backup_settings",
        &format!("enabled={enabled} keep={keep} interval={interval_hours}h"),
        "ok",
    );
    Ok(BackupInfo {
        settings: read_auto_settings(&state.store),
        files: crate::backup::list_auto_backups(&state.dir),
    })
}

#[tauri::command]
pub fn auto_backup_restore(
    state: State<'_, Arc<AppState>>,
    name: String,
    mode: String,
) -> Result<crate::backup::RestoreResult, String> {
    let res = crate::backup::restore_auto_backup(&state.store, &state.vault, &state.dir, &name, &mode)?;
    ops::audit(&state, "user", None, "auto_backup_restore", &name, &format!("{} 台", res.imported));
    Ok(res)
}

#[tauri::command]
pub fn auto_backup_delete(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    crate::backup::delete_auto_backup(&state.dir, &name)?;
    ops::audit(&state, "user", None, "auto_backup_delete", &name, "ok");
    Ok(())
}
