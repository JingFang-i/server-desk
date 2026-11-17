//! 统一命令执行管线：手动（user/manual）与 AI（mcp）共用。
//! AI 路径叠加：allow_ai 检查 → 泄露防护 → 黑名单拦截 → 人工审批 → 输出脱敏。
//! 所有路径写审计（detail 脱敏）。

use crate::guard::Decision;
use crate::ssh::{ConnParams, ExecResult};
use crate::state::AppState;
use shared::{AuthMethod, ServerConfig};
use std::time::Duration;
use tauri::Emitter;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

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

// ---------- SFTP：文件传输与远程文件读写 ----------

const AI_READ_FILE_CAP: u64 = 512 * 1024; // AI 读文件上限 512KB
const AI_WRITE_FILE_CAP: usize = 1024 * 1024; // AI 写文件上限 1MB
const UI_DOWNLOAD_CAP: u64 = 1024 * 1024 * 1024; // 界面下载上限 1GB

fn ai_gate(state: &AppState, cfg: &ServerConfig, action: &str, detail: &str) -> Result<(), String> {
    if !cfg.allow_ai {
        audit(state, "ai", Some(&cfg.id), action, detail, "blocked:not_allow_ai");
        return Err(format!("服务器「{}」未开放 AI 操作（allowAi=false）", cfg.name));
    }
    Ok(())
}

async fn wait_file_approval(state: &AppState, cfg: &ServerConfig, summary: &str) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _pending = state.approvals.create(&cfg.id, &cfg.name, summary, tx);
    let _ = state.handle.emit("approvals-updated", ());
    audit(state, "ai", Some(&cfg.id), "mcp_approval_requested", summary, "file_write");
    let decided = tokio::time::timeout(
        Duration::from_secs(crate::guard::APPROVAL_TIMEOUT_SECS),
        rx,
    )
    .await;
    let approved = matches!(decided, Ok(Ok(true)));
    if !approved {
        audit(
            state,
            "ai",
            Some(&cfg.id),
            "mcp_approval_requested",
            summary,
            "approval_denied_or_timeout",
        );
        return Err("审批被拒绝或已超时（120 秒），写入未执行".into());
    }
    audit(state, "ai", Some(&cfg.id), "mcp_approval_granted", summary, "ok");
    Ok(())
}

/// 读取远程文件（source = "manual" 或 "ai"）。
pub async fn remote_read_file(
    state: &AppState,
    server_ref: &str,
    path: &str,
    source: &str,
) -> Result<(String, u64, bool), String> {
    let cfg = load_server_config(state, server_ref)?;
    if source == "ai" {
        ai_gate(state, &cfg, "mcp_read_file", path)?;
        if let Decision::Blocked(reason) = state.guard.evaluate_read_path(path) {
            audit(state, source, Some(&cfg.id), "mcp_read_file", path, "blocked");
            return Err(format!("已拦截：{reason}"));
        }
    }
    let res = state
        .ssh
        .sftp_read_file(
            &state.store,
            &cfg.id,
            &conn_params(&cfg),
            path,
            if source == "ai" { AI_READ_FILE_CAP } else { AI_READ_FILE_CAP * 4 },
        )
        .await;
    match res {
        Ok((data, full, truncated)) => {
            let content = if source == "ai" {
                state.guard.sanitize(&String::from_utf8_lossy(&data))
            } else {
                String::from_utf8_lossy(&data).to_string()
            };
            audit(
                state,
                source,
                Some(&cfg.id),
                if source == "ai" { "mcp_read_file" } else { "sftp_read" },
                path,
                &format!("size={full} truncated={truncated}"),
            );
            Ok((content, full, truncated))
        }
        Err(e) => {
            audit(state, source, Some(&cfg.id), "mcp_read_file", path, &truncate(&e, 200));
            Err(e)
        }
    }
}

/// 写入远程文件（AI 需审批；手动直连）。
pub async fn remote_write_file(
    state: &AppState,
    server_ref: &str,
    path: &str,
    content: &str,
    source: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    if content.len() > AI_WRITE_FILE_CAP {
        return Err(format!("内容超过 {}MB 上限", AI_WRITE_FILE_CAP / 1024 / 1024));
    }
    if source == "ai" {
        ai_gate(state, &cfg, "mcp_write_file", path)?;
        match state.guard.evaluate_write_path(path) {
            Decision::Blocked(reason) => {
                audit(state, source, Some(&cfg.id), "mcp_write_file", path, "blocked");
                return Err(format!("已拦截：{reason}"));
            }
            Decision::NeedsApproval(_) => {
                wait_file_approval(
                    state,
                    &cfg,
                    &format!("写入文件 {path}（{} 字节）", content.len()),
                )
                .await?;
            }
            Decision::Allow => {}
        }
    }
    let res = state
        .ssh
        .sftp_write_file(&state.store, &cfg.id, &conn_params(&cfg), path, content.as_bytes())
        .await;
    match &res {
        Ok(n) => audit(
            state,
            source,
            Some(&cfg.id),
            if source == "ai" { "mcp_write_file" } else { "sftp_write" },
            path,
            &format!("bytes={n}"),
        ),
        Err(e) => audit(
            state,
            source,
            Some(&cfg.id),
            "mcp_write_file",
            path,
            &truncate(e, 200),
        ),
    }
    res
}

/// 界面：上传本地文件到远程。
pub async fn sftp_upload_manual(
    state: &AppState,
    server_ref: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    let res = state
        .ssh
        .sftp_upload(&state.store, &cfg.id, &conn_params(&cfg), local_path, remote_path)
        .await;
    match &res {
        Ok(n) => audit(
            state,
            "manual",
            Some(&cfg.id),
            "sftp_upload",
            &format!("{local_path} → {remote_path}"),
            &format!("bytes={n}"),
        ),
        Err(e) => audit(
            state,
            "manual",
            Some(&cfg.id),
            "sftp_upload",
            &format!("{local_path} → {remote_path}"),
            &truncate(e, 200),
        ),
    }
    res
}

/// 界面：下载远程文件到本地。
pub async fn sftp_download_manual(
    state: &AppState,
    server_ref: &str,
    remote_path: &str,
    save_path: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    let res = state
        .ssh
        .sftp_download(
            &state.store,
            &cfg.id,
            &conn_params(&cfg),
            remote_path,
            save_path,
            UI_DOWNLOAD_CAP,
        )
        .await;
    match &res {
        Ok(n) => audit(
            state,
            "manual",
            Some(&cfg.id),
            "sftp_download",
            &format!("{remote_path} → {save_path}"),
            &format!("bytes={n}"),
        ),
        Err(e) => audit(
            state,
            "manual",
            Some(&cfg.id),
            "sftp_download",
            &format!("{remote_path} → {save_path}"),
            &truncate(e, 200),
        ),
    }
    res
}
