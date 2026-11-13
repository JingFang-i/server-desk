//! Tauri IPC 命令层：前端唯一入口。
//! 凭据仅在解密后短暂存在于内存；用户本人的增删改查会经由前端表单往返（属预期行为）。
//! AI 侧（M3 起）走 MCP sidecar → guard 安全管线，结果绝不含凭据字段。

use crate::crypto;
use crate::ssh::{self, ConnInfo, ConnParams, ExecResult};
use crate::state::AppState;
use crate::vault::{MigrationOutcome, VaultStatus};
use chrono::Utc;
use shared::{AuthMethod, ServerConfig, TestResult};
use tauri::State;
use uuid::Uuid;

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn default_true() -> bool {
    true
}

fn audit(state: &AppState, source: &str, server_id: Option<&str>, action: &str, detail: &str, outcome: &str) {
    let _ = state.store.insert_audit(&now(), source, server_id, action, detail, outcome);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// ---------- 保险库 ----------

#[tauri::command]
pub fn vault_status(state: State<'_, AppState>) -> Result<VaultStatus, String> {
    Ok(state.vault.status())
}

#[tauri::command]
pub fn vault_setup(state: State<'_, AppState>, password: String) -> Result<Option<String>, String> {
    state.vault.setup(&password).map_err(|e| e.to_string())?;
    let mig = state
        .vault
        .migrate_legacy_if_needed(&state.store)
        .map_err(|e| e.to_string())?;
    audit(&state, "user", None, "vault_setup", "初始化主密码保险库", "ok");
    Ok(migration_summary(&mig))
}

#[tauri::command]
pub fn vault_unlock(state: State<'_, AppState>, password: String) -> Result<Option<String>, String> {
    state.vault.unlock(&password).map_err(|e| e.to_string())?;
    let mig = state
        .vault
        .migrate_legacy_if_needed(&state.store)
        .map_err(|e| e.to_string())?;
    audit(&state, "user", None, "vault_unlock", "解锁保险库", "ok");
    Ok(migration_summary(&mig))
}

fn migration_summary(m: &MigrationOutcome) -> Option<String> {
    m.summary()
}

#[tauri::command]
pub fn vault_lock(state: State<'_, AppState>) -> Result<(), String> {
    state.vault.lock();
    audit(&state, "user", None, "vault_lock", "手动锁定保险库", "ok");
    Ok(())
}

#[tauri::command]
pub fn vault_set_settings(
    state: State<'_, AppState>,
    auto_lock_minutes: u64,
    keychain_auto_unlock: bool,
) -> Result<VaultStatus, String> {
    state
        .vault
        .set_settings(auto_lock_minutes, keychain_auto_unlock)
        .map_err(|e| e.to_string())?;
    audit(
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
pub fn vault_reset(state: State<'_, AppState>) -> Result<(), String> {
    state.store.wipe_all()?;
    state.vault.reset().map_err(|e| e.to_string())?;
    audit(&state, "user", None, "vault_reset", "重置保险库并清空数据", "ok");
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

fn row_to_config(dek: &[u8; crypto::KEY_LEN], r: &crate::store::ServerRow) -> Result<ServerConfig, String> {
    let plain = crypto::decrypt(dek, &r.secret_blob)?;
    let auth: AuthMethod = serde_json::from_slice(&plain).map_err(|e| format!("凭据数据损坏: {e}"))?;
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

/// 加载并解密单台服务器配置（凭据仅存在于调用栈内）。
fn load_server_config(state: &AppState, server_id: &str) -> Result<ServerConfig, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let row = state
        .store
        .get(server_id)?
        .ok_or("服务器不存在或已被删除")?;
    row_to_config(&dek, &row)
}

fn conn_params(cfg: &ServerConfig) -> ConnParams {
    ConnParams {
        host: cfg.host.clone(),
        port: cfg.port,
        username: cfg.username.clone(),
        auth: cfg.auth.clone(),
    }
}

#[tauri::command]
pub fn list_servers(state: State<'_, AppState>) -> Result<Vec<ServerConfig>, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let rows = state.store.list()?;
    rows.iter().map(|r| row_to_config(&dek, r)).collect()
}

#[tauri::command]
pub fn save_server(state: State<'_, AppState>, input: SaveServerInput) -> Result<ServerConfig, String> {
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
    let blob = crypto::encrypt(&dek, &cred_json)?;

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
    audit(
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
pub async fn delete_server(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let deleted = state.store.delete(&id)?;
    if !deleted {
        return Err("服务器不存在或已被删除".into());
    }
    let _ = state.ssh.disconnect(&id).await;
    audit(&state, "user", Some(&id), "delete_server", "删除服务器", "ok");
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
pub async fn ssh_test(state: State<'_, AppState>, server_id: String) -> Result<TestResult, String> {
    let cfg = load_server_config(&state, &server_id)?;
    let started = std::time::Instant::now();
    let res = state
        .ssh
        .establish(&state.store, &server_id, &conn_params(&cfg))
        .await;
    let latency = started.elapsed().as_millis() as u64;
    match res {
        Ok(mut session) => {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "test", "en")
                .await;
            audit(&state, "user", Some(&server_id), "ssh_test", "SSH 认证测试", "ok");
            Ok(TestResult {
                ok: true,
                latency_ms: Some(latency),
                message: format!("SSH 认证成功（{}，{latency}ms）", cfg.host),
            })
        }
        Err(e) => {
            audit(
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
pub async fn ssh_connect(state: State<'_, AppState>, server_id: String) -> Result<(), String> {
    let cfg = load_server_config(&state, &server_id)?;
    state
        .ssh
        .connect(&state.store, &server_id, conn_params(&cfg))
        .await?;
    audit(
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
pub async fn ssh_disconnect(state: State<'_, AppState>, server_id: String) -> Result<(), String> {
    let _ = state.ssh.disconnect(&server_id).await;
    audit(&state, "user", Some(&server_id), "ssh_disconnect", "断开连接", "ok");
    Ok(())
}

#[tauri::command]
pub async fn ssh_status(state: State<'_, AppState>) -> Result<Vec<ConnInfo>, String> {
    Ok(state.ssh.status().await)
}

#[tauri::command]
pub async fn ssh_exec(
    state: State<'_, AppState>,
    server_id: String,
    command: String,
    timeout_secs: Option<u64>,
) -> Result<ExecResult, String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("命令不能为空".into());
    }
    let cfg = load_server_config(&state, &server_id)?;
    let timeout =
        std::time::Duration::from_secs(timeout_secs.unwrap_or(30).clamp(1, 300));
    let res = state
        .ssh
        .exec(&state.store, &server_id, &conn_params(&cfg), &command, timeout)
        .await;
    match &res {
        Ok(r) => audit(
            &state,
            "manual",
            Some(&server_id),
            "ssh_exec",
            &truncate(&command, 500),
            &format!("exit={:?} len={}", r.exit_code, r.stdout.len() + r.stderr.len()),
        ),
        Err(e) => audit(
            &state,
            "manual",
            Some(&server_id),
            "ssh_exec",
            &truncate(&command, 500),
            &truncate(e, 300),
        ),
    }
    res
}

/// 重置某服务器的主机密钥指纹（确认服务器重装/换钥后使用）。
#[tauri::command]
pub fn ssh_reset_hostkey(state: State<'_, AppState>, server_id: String) -> Result<(), String> {
    state.store.clear_host_fingerprint(&server_id)?;
    audit(&state, "user", Some(&server_id), "ssh_reset_hostkey", "重置主机密钥指纹", "ok");
    Ok(())
}
