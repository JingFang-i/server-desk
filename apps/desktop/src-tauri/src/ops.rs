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

pub fn ai_gate(state: &AppState, cfg: &ServerConfig, action: &str, detail: &str) -> Result<(), String> {
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
    // 写前自动备份：原文件存在且 ≤1MB 时保存一份，供 restore_file 回滚
    let mut backup_note = String::new();
    if source == "ai" {
        if let Ok((orig, orig_size, false)) = state
            .ssh
            .sftp_read_file(&state.store, &cfg.id, &conn_params(&cfg), path, 1024 * 1024)
            .await
        {
            if orig_size <= 1024 * 1024 {
                if let Ok(meta) = crate::fbackups::save(&state.dir, &cfg.id, path, &orig) {
                    backup_note = format!(" backup={}", meta.id);
                }
            }
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
            &format!("bytes={n}{backup_note}"),
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

// ---------- R2：AI 文件能力（上传/下载/追加/回滚） ----------

/// 本地（ServerDesk 所在机器）读取防护：防止 AI 把本机敏感文件推到服务器。
fn gate_local_read(state: &AppState, cfg: &ServerConfig, action: &str, local_path: &str) -> Result<(), String> {
    ai_gate(state, cfg, action, local_path)?;
    if let Decision::Blocked(reason) = state.guard.evaluate_read_path(local_path) {
        audit(state, "ai", Some(&cfg.id), action, local_path, "blocked:local_read");
        return Err(format!("已拦截：{reason}（本地路径）"));
    }
    Ok(())
}

fn remote_write_rules(state: &AppState, cfg: &ServerConfig, action: &str, path: &str) -> Result<(), String> {
    match state.guard.evaluate_write_path(path) {
        Decision::Blocked(reason) => {
            audit(state, "ai", Some(&cfg.id), action, path, "blocked");
            Err(format!("已拦截：{reason}"))
        }
        Decision::NeedsApproval(reason) => {
            audit(state, "ai", Some(&cfg.id), action, path, &reason);
            Ok(())
        }
        Decision::Allow => Ok(()),
    }
}

/// AI 上传本地文件到服务器（本地与远端双重防护 + 审批）。
pub async fn ai_upload_file(
    state: &AppState,
    server_ref: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    gate_local_read(state, &cfg, "mcp_upload_file", local_path)?;
    remote_write_rules(state, &cfg, "mcp_upload_file", remote_path)?;
    let summary = format!(
        "上传文件 {} → {}:{}",
        local_path, cfg.host, remote_path
    );
    wait_file_approval(state, &cfg, &summary).await?;
    let res = state
        .ssh
        .sftp_upload(&state.store, &cfg.id, &conn_params(&cfg), local_path, remote_path)
        .await;
    match &res {
        Ok(n) => audit(state, "ai", Some(&cfg.id), "mcp_upload_file", &format!("{local_path} → {remote_path}"), &format!("bytes={n}")),
        Err(e) => audit(state, "ai", Some(&cfg.id), "mcp_upload_file", &format!("{local_path} → {remote_path}"), &truncate(e, 200)),
    }
    res
}

/// AI 从服务器下载文件到应用受管目录（ai_downloads/），不允许写任意本地路径。
pub async fn ai_download_file(
    state: &AppState,
    server_ref: &str,
    remote_path: &str,
) -> Result<String, String> {
    let cfg = load_server_config(state, server_ref)?;
    ai_gate(state, &cfg, "mcp_download_file", remote_path)?;
    if let Decision::Blocked(reason) = state.guard.evaluate_read_path(remote_path) {
        audit(state, "ai", Some(&cfg.id), "mcp_download_file", remote_path, "blocked");
        return Err(format!("已拦截：{reason}"));
    }
    let downloads = state.dir.join("ai_downloads");
    std::fs::create_dir_all(&downloads).map_err(|e| format!("创建下载目录失败: {e}"))?;
    let base = remote_path.rsplit('/').next().unwrap_or("download");
    let safe_base: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || ".-_".contains(c) { c } else { '_' })
        .collect();
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let save_path = downloads.join(format!("{ts}-{safe_base}"));
    let res = state
        .ssh
        .sftp_download(
            &state.store,
            &cfg.id,
            &conn_params(&cfg),
            remote_path,
            &save_path.display().to_string(),
            100 * 1024 * 1024,
        )
        .await;
    match &res {
        Ok(n) => audit(state, "ai", Some(&cfg.id), "mcp_download_file", remote_path, &format!("bytes={n} → {}", save_path.display())),
        Err(e) => audit(state, "ai", Some(&cfg.id), "mcp_download_file", remote_path, &truncate(e, 200)),
    }
    res?;
    Ok(save_path.display().to_string())
}

/// AI 追加内容到远程已有文件（不存在则拒绝，新建用 write_file）。需审批。
pub async fn ai_append_file(
    state: &AppState,
    server_ref: &str,
    path: &str,
    content: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    ai_gate(state, &cfg, "mcp_append_file", path)?;
    if content.len() > 512 * 1024 {
        return Err("追加内容超过 512KB 上限".into());
    }
    remote_write_rules(state, &cfg, "mcp_append_file", path)?;
    // append 语义：文件必须已存在
    let existing = state
        .ssh
        .sftp_read_file(&state.store, &cfg.id, &conn_params(&cfg), path, 1024 * 1024)
        .await
        .map_err(|_| "远程文件不存在或不可读：append 仅限已有文件，新建请用 write_file".to_string())?;
    let mut merged = existing.0;
    if !merged.ends_with(b"\n") && !merged.is_empty() {
        merged.push(b'\n');
    }
    merged.extend_from_slice(content.as_bytes());
    if merged.len() > 1024 * 1024 {
        return Err("追加后超过 1MB 上限".into());
    }
    let summary = format!("追加内容到 {}（{} 字节）", path, content.len());
    wait_file_approval(state, &cfg, &summary).await?;
    let res = state
        .ssh
        .sftp_write_file(&state.store, &cfg.id, &conn_params(&cfg), path, &merged)
        .await;
    match &res {
        Ok(n) => audit(state, "ai", Some(&cfg.id), "mcp_append_file", path, &format!("bytes={n}")),
        Err(e) => audit(state, "ai", Some(&cfg.id), "mcp_append_file", path, &truncate(e, 200)),
    }
    res
}

/// AI 回滚远程文件到某份写前备份（需审批；不做禁写名单拦截——回滚目标为人工批准过的原始状态）。
pub async fn ai_restore_file(
    state: &AppState,
    server_ref: &str,
    backup_id: &str,
) -> Result<u64, String> {
    let cfg = load_server_config(state, server_ref)?;
    ai_gate(state, &cfg, "mcp_restore_file", backup_id)?;
    let (meta, content) =
        crate::fbackups::load(&state.dir, backup_id).map_err(|e| e.to_string())?;
    if meta.server_id != cfg.id {
        return Err("备份不属于该服务器".into());
    }
    let summary = format!(
        "回滚文件 {} 至备份 {}（{} 字节，备份于 {}）",
        meta.path, meta.id, meta.size, meta.ts
    );
    wait_file_approval(state, &cfg, &summary).await?;
    let res = state
        .ssh
        .sftp_write_file(&state.store, &cfg.id, &conn_params(&cfg), &meta.path, &content)
        .await;
    match &res {
        Ok(n) => audit(state, "ai", Some(&cfg.id), "mcp_restore_file", &meta.path, &format!("backup={} bytes={n}", meta.id)),
        Err(e) => audit(state, "ai", Some(&cfg.id), "mcp_restore_file", &meta.path, &truncate(e, 200)),
    }
    res
}
