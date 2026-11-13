//! 统一命令执行管线：手动（user/manual）与 AI（mcp）共用。
//! AI 路径叠加：allow_ai 检查 → 泄露防护 → 黑名单拦截 → 人工审批 → 输出脱敏。
//! 所有路径写审计（detail 脱敏）。

use crate::guard::Decision;
use crate::ssh::{ConnParams, ExecResult};
use crate::state::AppState;
use shared::{AuthMethod, ServerConfig};
use std::time::Duration;
use tauri::Emitter;

/// 解密并加载单台服务器配置（按 id 或精确名称解析）。
pub fn load_server_config(state: &AppState, id_or_name: &str) -> Result<ServerConfig, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let row = state
        .store
        .get(id_or_name)?
        .or_else(|| {
            state
                .store
                .find_by_name(id_or_name)
                .ok()
                .flatten()
        })
        .ok_or_else(|| format!("找不到服务器「{id_or_name}」"))?;
    let plain = crate::crypto::decrypt(&dek, &row.secret_blob)?;
    let auth: AuthMethod =
        serde_json::from_slice(&plain).map_err(|e| format!("凭据数据损坏: {e}"))?;
    let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
    Ok(ServerConfig {
        id: row.id.clone(),
        name: row.name.clone(),
        host: row.host.clone(),
        port: row.port,
        username: row.username.clone(),
        auth,
        group: row.group_name.clone(),
        tags,
        notes: row.notes.clone(),
        allow_ai: row.allow_ai,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    })
}

pub fn conn_params(cfg: &ServerConfig) -> ConnParams {
    ConnParams {
        host: cfg.host.clone(),
        port: cfg.port,
        username: cfg.username.clone(),
        auth: cfg.auth.clone(),
    }
}

pub fn audit(
    state: &AppState,
    source: &str,
    server_id: Option<&str>,
    action: &str,
    detail: &str,
    outcome: &str,
) {
    let sanitized = state.guard.sanitize(detail);
    let _ = state.store.insert_audit(
        &chrono::Utc::now().to_rfc3339(),
        source,
        server_id,
        action,
        &crate::commands::truncate(&sanitized, 500),
        outcome,
    );
}

/// 执行一条命令。source: "manual"（用户）或 "ai"（MCP 管线）。
pub async fn run_command(
    state: &AppState,
    server_ref: &str,
    command: &str,
    timeout_secs: u64,
    source: &str,
) -> Result<ExecResult, String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("命令不能为空".into());
    }
    let cfg = load_server_config(state, server_ref)?;
    let timeout = Duration::from_secs(timeout_secs.clamp(1, 300));

    // ---- AI 专属安全管线 ----
    if source == "ai" {
        if !cfg.allow_ai {
            audit(
                state,
                source,
                Some(&cfg.id),
                "mcp_exec_command",
                &command,
                "blocked:not_allow_ai",
            );
            return Err(format!("服务器「{}」未开放 AI 操作（allowAi=false）", cfg.name));
        }
        match state.guard.evaluate(&command) {
            Decision::Blocked(reason) => {
                audit(state, source, Some(&cfg.id), "mcp_exec_command", &command, "blocked");
                return Err(format!("已拦截：{reason}"));
            }
            Decision::NeedsApproval(reason) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _pending = state.approvals.create(&cfg.id, &cfg.name, &command, tx);
                let _ = state.handle.emit("approvals-updated", ());
                audit(
                    state,
                    source,
                    Some(&cfg.id),
                    "mcp_approval_requested",
                    &command,
                    &reason,
                );
                let decided =
                    tokio::time::timeout(Duration::from_secs(crate::guard::APPROVAL_TIMEOUT_SECS), rx).await;
                let approved = matches!(decided, Ok(Ok(true)));
                if !approved {
                    audit(
                        state,
                        source,
                        Some(&cfg.id),
                        "mcp_exec_command",
                        &command,
                        "approval_denied_or_timeout",
                    );
                    return Err("审批被拒绝或已超时（120 秒），命令未执行".into());
                }
                audit(state, source, Some(&cfg.id), "mcp_approval_granted", &command, "ok");
            }
            Decision::Allow => {}
        }
    }

    // ---- 执行 ----
    let res = state
        .ssh
        .exec(&state.store, &cfg.id, &conn_params(&cfg), &command, timeout)
        .await;

    let out = match res {
        Ok(r) if source == "ai" => ExecResult {
            stdout: state.guard.sanitize(&r.stdout),
            stderr: state.guard.sanitize(&r.stderr),
            exit_code: r.exit_code,
            truncated: r.truncated,
        },
        other => other?,
    };

    audit(
        state,
        source,
        Some(&cfg.id),
        if source == "ai" { "mcp_exec_command" } else { "ssh_exec" },
        &command,
        &match &out {
            ExecResult { exit_code, .. } => format!("exit={exit_code:?}"),
        },
    );
    Ok(out)
}
