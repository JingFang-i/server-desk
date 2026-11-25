//! Tauri IPC 命令层：前端唯一入口。
//! 凭据仅在解密后短暂存在于内存；用户本人的增删改查会经由前端表单往返（属预期行为）。
//! AI 侧走 MCP sidecar → ipc → ops 安全管线，结果不含凭据且已脱敏。

use crate::guard::GuardRules;
use crate::ops;
use crate::state::AppState;
use crate::text::{truncate, AUDIT_DETAIL_MAX, ERROR_MAX};
use crate::vault::VaultStatus;
use chrono::Utc;
use shared::{AuthMethod, ServerConfig, TestResult};
use std::sync::Arc;
use tauri::{Emitter, State};
use uuid::Uuid;

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

/// 异步命令：解锁要做 Argon2 与 macOS 钥匙串同步（Security 框架可能阻塞），
/// 同步命令跑在主线程上——一旦阻塞整个界面假死（症状：输完密码回车后无响应）。
#[tauri::command]
pub async fn vault_unlock(
    state: State<'_, Arc<AppState>>,
    password: String,
) -> Result<Option<String>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.vault.unlock(&password).map_err(|e| e.to_string())?;
        let mig = state
            .vault
            .migrate_legacy_if_needed(&state.store)
            .map_err(|e| e.to_string())?;
        ops::audit(&state, "user", None, "vault_unlock", "解锁保险库", "ok");
        Ok(mig.summary())
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

#[tauri::command]
pub fn vault_lock(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.vault.lock();
    ops::audit(&state, "user", None, "vault_lock", "手动锁定保险库", "ok");
    // 广播锁定事件：设置页/侧栏立即回到锁屏（此前最长要等 20s 轮询才生效）
    let _ = state.handle.emit("vault-locked", ());
    Ok(())
}

#[tauri::command]
pub async fn vault_set_settings(
    state: State<'_, Arc<AppState>>,
    auto_lock_minutes: u64,
    keychain_auto_unlock: bool,
) -> Result<VaultStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListServersResult {
    pub servers: Vec<ServerConfig>,
    /// 解密/解析失败的行（不拖垮整个列表，前端可见提示）
    pub errors: Vec<String>,
}

#[tauri::command]
pub fn list_servers(state: State<'_, Arc<AppState>>) -> Result<ListServersResult, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let rows = state.store.list()?;
    let mut servers = Vec::new();
    let mut errors = Vec::new();
    for r in rows {
        match row_to_config(&dek, &r) {
            Ok(c) => servers.push(c),
            Err(e) => errors.push(format!("「{}」读取失败：{e}", r.name)),
        }
    }
    Ok(ListServersResult { servers, errors })
}

#[tauri::command]
pub async fn save_server(
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
        // 主机/端口/用户/凭据变更后，旧 SSH 会话必须淘汰：连接池按 server id 复用，
        // 不淘汰会把后续命令发往旧主机，或继续以旧身份执行（审计里还显示新名字）。
        let _ = state.ssh.disconnect(&row.id).await;
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
    // 先取名称快照：删除后无法再按 id 回查，审计仍要能显示具体服务器名
    let name = state.store.get(&id)?.map(|r| r.name).unwrap_or_default();
    let deleted = state.store.delete(&id)?;
    if !deleted {
        return Err("服务器不存在或已被删除".into());
    }
    let _ = state.ssh.disconnect(&id).await;
    let detail = if name.is_empty() {
        "删除服务器".to_string()
    } else {
        format!("删除服务器「{name}」")
    };
    ops::audit_named(&state, "user", Some(&id), &name, "delete_server", &detail, "ok");
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
                &truncate(&e, ERROR_MAX),
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
    rules: GuardRules,
) -> Result<crate::guard::GuardInfo, String> {
    // 安全策略是整表覆盖：先取旧值，改完把差异写进审计，否则「谁在什么时候放开了什么」
    // 事后完全查不到（红线 5 要求逐次可追溯）。
    let before = state.guard.info();
    state
        .guard
        .set_rules(&state.store, rules)
        .map_err(|e| e.to_string())?;
    let after = state.guard.info();
    let detail = summarize_rule_change(&before, &after);
    ops::audit(&state, "user", None, "guard_set_rules", &detail, "ok");
    Ok(after)
}

/// 把安全规则的前后差异压成一条可审计摘要（新增/删除逐条列出，其余按开关变化）。
fn summarize_rule_change(before: &crate::guard::GuardInfo, after: &crate::guard::GuardInfo) -> String {
    fn diff(name: &str, a: &[String], b: &[String], out: &mut Vec<String>) {
        let added: Vec<&str> = b.iter().map(|s| s.as_str()).filter(|x| !a.iter().any(|y| y == x)).collect();
        let removed: Vec<&str> = a.iter().map(|s| s.as_str()).filter(|x| !b.iter().any(|y| y == x)).collect();
        if !added.is_empty() {
            out.push(format!("{name}+[{}]", added.join(",")));
        }
        if !removed.is_empty() {
            out.push(format!("{name}-[{}]", removed.join(",")));
        }
    }
    let mut parts: Vec<String> = Vec::new();
    diff("deny", &before.deny, &after.deny, &mut parts);
    diff("approval", &before.approval, &after.approval, &mut parts);
    diff("whitelist", &before.whitelist, &after.whitelist, &mut parts);
    if before.whitelist_mode != after.whitelist_mode {
        parts.push(format!("whitelist_mode={}", after.whitelist_mode));
    }
    if before.readonly != after.readonly {
        parts.push(format!("readonly={}", after.readonly));
    }
    if before.rate_per_min != after.rate_per_min {
        parts.push(format!("rate={}→{}", before.rate_per_min, after.rate_per_min));
    }
    if before.strict_groups != after.strict_groups {
        parts.push(format!("strict_groups=[{}]", after.strict_groups.join(",")));
    }
    if parts.is_empty() {
        "更新安全规则（无变化）".to_string()
    } else {
        truncate(&format!("更新安全规则：{}", parts.join(" ")), 480)
    }
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
        shared::action::MCP_SET_ENABLED,
        &format!("AI 接入 {}", if enabled { "开启" } else { "关闭" }),
        "ok",
    );
    Ok(state.guard.info())
}

// ---------- AI 智能审查（R8） ----------

/// 审查配置 + Key 掩码。绝无明文 Key（红线 1）。
#[tauri::command]
pub fn ai_review_info(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::review::ReviewInfo, String> {
    Ok(state.reviewer.info(&state.store, &state.vault))
}

/// 保存审查配置并热生效；`api_key` 为空 / None 表示不修改已存 Key。
/// Key 加密落盘；审计只记配置事实与「key 是否更新」，绝不记 Key 本体。
#[tauri::command]
pub fn ai_review_save(
    state: State<'_, Arc<AppState>>,
    config: crate::review::ReviewConfig,
    api_key: Option<String>,
) -> Result<crate::review::ReviewInfo, String> {
    let cfg = config.normalized();
    cfg.validate()?;
    let key_changed = api_key
        .as_deref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    if key_changed {
        crate::review::config::save_api_key(
            &state.store,
            &state.vault,
            api_key.as_deref().unwrap_or_default(),
        )?;
    }
    state.reviewer.set_config(&state.store, cfg)?;
    let saved = state.reviewer.config();
    ops::audit(
        &state,
        "user",
        None,
        "ai_review_save",
        &truncate(
            &format!(
                "AI 审查配置：enabled={} model={} timeout={}ms 失败策略={} 拒绝策略={} key={}",
                saved.enabled,
                saved.model,
                saved.timeout_ms,
                saved.failure_policy.as_str(),
                saved.deny_mode.as_str(),
                if key_changed { "已更新" } else { "未变更" },
            ),
            AUDIT_DETAIL_MAX,
        ),
        "ok",
    );
    Ok(state.reviewer.info(&state.store, &state.vault))
}

/// 连通性测试结果：`ok=false` 时 error 带原因，前端直接展示。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub model: String,
    /// 模型原始输出（截断展示，仅诊断用；R8.3 起由正式解析器消费）
    pub content: String,
    pub error: String,
}

/// 连通性测试：用一条固定的安全命令真实调用一次模型。
/// 不产生审查日志（R8.5），但写审计；错误信息已单行化截断。
#[tauri::command]
pub async fn ai_review_test(state: State<'_, Arc<AppState>>) -> Result<ReviewTestResult, String> {
    let state = state.inner().clone();
    let cfg = state.reviewer.config();
    if !cfg.enabled {
        return Err("AI 审查未启用：请先开启并保存配置".into());
    }
    let key = crate::review::config::load_api_key(&state.store, &state.vault)?
        .filter(|k| !k.trim().is_empty());
    let Some(key) = key else {
        return Err("未配置 API Key".into());
    };
    let outcome = crate::review::client::chat_json(
        state.reviewer.http(),
        &cfg,
        Some(&key),
        "你是服务器运维命令的安全审查员。只输出 JSON，不要解释。",
        r#"{"tool":"exec_command","command":"uptime"}"#,
    )
    .await;
    match outcome {
        Ok(o) => {
            ops::audit(
                &state,
                "user",
                None,
                "ai_review_test",
                &truncate(
                    &format!("AI 审查连通性测试：model={} {}ms", o.model, o.latency_ms),
                    AUDIT_DETAIL_MAX,
                ),
                "ok",
            );
            Ok(ReviewTestResult {
                ok: true,
                latency_ms: o.latency_ms,
                model: o.model,
                content: truncate(&o.content, 120),
                error: String::new(),
            })
        }
        Err(e) => {
            ops::audit(
                &state,
                "user",
                None,
                "ai_review_test",
                &truncate(&format!("AI 审查连通性测试失败：{e}"), AUDIT_DETAIL_MAX),
                "error",
            );
            Ok(ReviewTestResult {
                ok: false,
                latency_ms: 0,
                model: cfg.model,
                content: String::new(),
                error: truncate(&e, ERROR_MAX),
            })
        }
    }
}

/// 最近审查记录（R8.5 接入 `ai_review_log` 表；当前返回空集供前端联调）。
#[tauri::command]
pub fn ai_review_recent(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
) -> Result<Vec<crate::review::ReviewEntry>, String> {
    let _ = (state, limit);
    Ok(Vec::new())
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
    session: Option<String>,
) -> Result<Vec<crate::store::AuditEntry>, String> {
    state
        .store
        .audit_list(limit.unwrap_or(200), source.as_deref(), session.as_deref())
}

/// 审计页服务端分页：SQL 层 LIMIT/OFFSET，几百上千条不再一次拉全。
#[tauri::command]
pub fn audit_page(
    state: State<'_, Arc<AppState>>,
    page: Option<i64>,
    page_size: Option<i64>,
    source: Option<String>,
    session: Option<String>,
) -> Result<crate::store::AuditPage, String> {
    state.store.audit_page(
        page.unwrap_or(1),
        page_size.unwrap_or(50),
        source.as_deref(),
        session.as_deref(),
    )
}

/// 审计页「会话」筛选的下拉数据（按最近活动倒序）。
#[tauri::command]
pub fn audit_sessions(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.store.audit_sessions(100)
}

/// 审计落库失败次数：> 0 时界面必须提示（红线 5「逐次审计」不允许静默丢失）。
#[tauri::command]
pub fn audit_failures(state: State<'_, Arc<AppState>>) -> u64 {
    state
        .audit_failures
        .load(std::sync::atomic::Ordering::Relaxed)
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
            // 顺序：开发（target/[debug|release] 同目录）→ 打包（资源子目录 resources/，
            // Windows/Linux 安装目录与 macOS Contents/Resources 同构）→ macOS 旧布局（资源根）
            // sidecar 以 bundle.externalBin 一等公民方式分发：
            // 安装后与主程序同目录，文件名带 target triple 后缀
            // （serverdesk-mcp-aarch64-apple-darwin / -x86_64-pc-windows-msvc.exe 等）。
            // 扫描主程序同目录下 serverdesk-mcp* 开头的文件，兼容带/不带 triple、.exe。
            let mut candidates: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("serverdesk-mcp") {
                        candidates.push(e.path());
                    }
                }
            }
            candidates.sort();
            if candidates.is_empty() {
                candidates.push(dir.join("serverdesk-mcp"));
            }
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

// ---------- SFTP：界面文件传输 ----------

#[tauri::command]
pub async fn sftp_upload(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    local_path: String,
    remote_path: String,
) -> Result<u64, String> {
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定，请先解锁".into());
    }
    ops::sftp_upload_manual(&state, &server_id, &local_path, &remote_path).await
}

#[tauri::command]
pub async fn sftp_download(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    remote_path: String,
    save_path: String,
) -> Result<u64, String> {
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定，请先解锁".into());
    }
    ops::sftp_download_manual(&state, &server_id, &remote_path, &save_path).await
}

// ---------- R5：变更记录（写前备份列表 + 手动回滚） ----------

#[tauri::command]
pub fn file_backups_list(
    state: State<'_, Arc<AppState>>,
    server: Option<String>,
) -> Result<Vec<crate::fbackups::BackupMeta>, String> {
    Ok(crate::fbackups::list(&state.dir, server.as_deref()))
}

#[tauri::command]
pub async fn file_backup_restore(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    backup_id: String,
) -> Result<u64, String> {
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定，请先解锁".into());
    }
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let (meta, content) =
        crate::fbackups::load_encrypted(&state.dir, &dek, &backup_id).map_err(|e| e.to_string())?;
    if meta.server_id != server_id {
        return Err("备份不属于该服务器".into());
    }
    let cfg = ops::load_server_config(&state, &server_id)?;
    let res = state
        .ssh
        .sftp_write_file(&state.store, &cfg.id, &ops::conn_params(&cfg), &meta.path, &content)
        .await;
    match &res {
        Ok(n) => ops::audit(&state, "user", Some(&server_id), "file_backup_restore", &meta.path, &format!("backup={backup_id} bytes={n}")),
        Err(e) => ops::audit(&state, "user", Some(&server_id), "file_backup_restore", &meta.path, &truncate(e, 200)),
    }
    res
}

// ---------- 备份与恢复 ----------

/// 读取用户通过文件对话框选择的私钥文件内容（仅明文 PEM/openssh 格式回填表单）。
#[tauri::command]
pub fn read_key_file(path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_file() {
        return Err("所选路径不是文件".into());
    }
    // 私钥文件都很小：限制体积，避免这条命令被当成任意大文件读取通道
    const MAX_KEY_FILE: u64 = 64 * 1024;
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX);
    if size > MAX_KEY_FILE {
        return Err(format!("私钥文件超过 {}KB，请确认选择了正确的文件", MAX_KEY_FILE / 1024));
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
    // 恢复会写资产表：必须处于解锁状态，才能拿到当前 DEK 校验备份凭据
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定，请先解锁再恢复备份".into());
    }
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let payload = crate::backup::read_payload(std::path::Path::new(&path), &password)?;
    let res = crate::backup::restore_payload(&state.store, &payload, &mode, &dek)?;
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
