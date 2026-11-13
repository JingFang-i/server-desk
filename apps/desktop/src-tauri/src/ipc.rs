//! 本地 IPC 服务：MCP sidecar ⇄ 主应用。
//! 仅监听 127.0.0.1 随机端口；每次启动生成新 token（0600 文件），
//! sidecar 每次调用时从数据目录读取，应用重启后无需重新配置客户端。
//! 主应用持有全部凭据与 SSH 会话；sidecar 只能通过该通道走安全管线。

use crate::ops;
use crate::state::AppState;
use shared::{IpcRequest, IpcResponse};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Duration;

pub async fn serve(state: Arc<AppState>, dir: PathBuf) {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("IPC 服务监听失败: {e}");
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token: String = {
        use rand::{rngs::OsRng, RngCore};
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    };
    if let Err(e) = std::fs::write(dir.join("ipc-port"), port.to_string()) {
        eprintln!("写入 ipc-port 失败: {e}");
        return;
    }
    if let Err(e) = std::fs::write(dir.join("ipc-token"), &token) {
        eprintln!("写入 ipc-token 失败: {e}");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir.join("ipc-token"), std::fs::Permissions::from_mode(0o600));
    }
    eprintln!("IPC 服务就绪: 127.0.0.1:{port}（token 已更新）");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let st = state.clone();
                let tk = token.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(st, tk, stream).await;
                });
            }
            Err(e) => {
                eprintln!("IPC accept 错误: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

async fn handle_conn(
    state: Arc<AppState>,
    token: String,
    mut stream: TcpStream,
) -> Result<(), String> {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    if !peer.starts_with("127.0.0.1") {
        return Err("仅允许本机回环连接".into());
    }

    // 读取头部（≤16KB，5s 超时）
    let mut buf = vec![0u8; 4096];
    let mut head: Vec<u8> = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .map_err(|_| "读取头部超时".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("连接已关闭".into());
        }
        head.extend_from_slice(&buf[..n]);
        if find_subsequence(&head, b"\r\n\r\n").is_some() {
            break;
        }
        if head.len() > 16 * 1024 {
            return Err("请求头过大".into());
        }
    }
    let split = find_subsequence(&head, b"\r\n\r\n").ok_or("请求格式错误")?;
    let header = String::from_utf8_lossy(&head[..split]).to_lowercase();
    let mut body = head[split + 4..].to_vec();

    if !header.starts_with("post") {
        return Err("仅支持 POST".into());
    }
    if !header.contains(&format!("authorization: bearer {token}")) {
        return Err("鉴权失败".into());
    }
    let content_length: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if content_length > 1024 * 1024 {
        return Err("请求体过大".into());
    }
    while body.len() < content_length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
            .await
            .map_err(|_| "读取请求体超时".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(content_length);

    let req: IpcRequest =
        serde_json::from_slice(&body).map_err(|e| format!("请求体解析失败: {e}"))?;
    let resp = match handle_tool(&state, &req.tool, req.args).await {
        Ok(data) => IpcResponse::ok(data),
        Err(e) => IpcResponse::err(e),
    };
    let out = serde_json::to_vec(&resp).map_err(|e| e.to_string())?;
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        out.len()
    );
    stream.write_all(http.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(&out).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// MCP 工具实现：全部经主应用安全管线（guard + 审批 + 审计 + 脱敏）。
async fn handle_tool(
    state: &AppState,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if !state.guard.ai_enabled() {
        return Err("MCP 工具已被禁用：请在应用「设置」中开启 AI 接入".into());
    }
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定：请在应用中解锁后再使用 MCP 工具".into());
    }

    match tool {
        "list_servers" => {
            let rows = state.store.list()?;
            let servers: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let tags: Vec<String> =
                        serde_json::from_str(&r.tags).unwrap_or_default();
                    serde_json::json!({
                        "id": r.id,
                        "name": r.name,
                        "host": r.host,
                        "port": r.port,
                        "username": r.username,
                        "group": r.group_name,
                        "tags": tags,
                        "allowAi": r.allow_ai,
                    })
                })
                .collect();
            ops::audit(state, "ai", None, "mcp_list_servers", "列出服务器", "ok");
            Ok(serde_json::json!({ "servers": servers }))
        }
        "exec_command" => {
            let server = args
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 server（服务器 id 或名称）")?;
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 command")?;
            let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(30);
            let r = ops::run_command(state, server, command, timeout_secs, "ai").await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "server_status" => {
            let server = args
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 server（服务器 id 或名称）")?;
            let cmd = "echo '=== uptime ==='; uptime; echo; \
                       echo '=== cpu ==='; nproc 2>/dev/null; echo; \
                       echo '=== mem ==='; free -m 2>/dev/null || vm_stat 2>/dev/null; echo; \
                       echo '=== disk(/) ==='; df -h / 2>/dev/null; echo; \
                       echo '=== load ==='; cat /proc/loadavg 2>/dev/null || sysctl -n vm.loadavg 2>/dev/null";
            let r = ops::run_command(state, server, cmd, 30, "ai").await?;
            ops::audit(state, "ai", Some(server), "mcp_server_status", "状态探查", "ok");
            Ok(serde_json::json!({ "server": server, "report": r.stdout, "exitCode": r.exit_code }))
        }
        other => Err(format!("未知工具: {other}（可用: list_servers / exec_command / server_status）")),
    }
}
