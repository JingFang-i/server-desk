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

fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("缺少参数 {key}"))
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
        "read_file" => {
            let server = args
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 server（服务器 id 或名称）")?;
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 path（远程文件绝对路径）")?;
            let (content, size, truncated) =
                ops::remote_read_file(state, server, path, "ai").await?;
            Ok(serde_json::json!({
                "server": server, "path": path,
                "size": size, "truncated": truncated,
                "content": content,
                "note": "内容已脱敏；命中敏感文件保护规则的路径会被直接拒绝"
            }))
        }
        "write_file" => {
            let server = args
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 server（服务器 id 或名称）")?;
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 path（远程文件绝对路径）")?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("缺少参数 content（要写入的文本内容）")?;
            let bytes = ops::remote_write_file(state, server, path, content, "ai").await?;
            Ok(serde_json::json!({
                "server": server, "path": path, "bytesWritten": bytes,
                "note": "写入需用户在 ServerDesk 应用内人工批准（120 秒超时）；系统安全相关路径直接拒绝"
            }))
        }
        "list_files" => {
            let server = tool_arg(&args, "server")?;
            let path = tool_arg(&args, "path")?;
            let r = crate::observe::list_files(state, server, path).await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "tail_log" => {
            let server = tool_arg(&args, "server")?;
            let path = tool_arg(&args, "path")?;
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as u32;
            let r = crate::observe::tail_log(state, server, path, lines).await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "log_search" => {
            let server = tool_arg(&args, "server")?;
            let path = tool_arg(&args, "path")?;
            let pattern = tool_arg(&args, "pattern")?;
            let ignore_case = args.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
            let max = args.get("max_matches").and_then(|v| v.as_u64()).unwrap_or(50) as u32;
            let ctx = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let r = crate::observe::log_search(state, server, path, pattern, ignore_case, max, ctx).await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "list_services" => {
            let server = tool_arg(&args, "server")?;
            let r = crate::observe::list_services(state, server).await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "port_check" => {
            let server = tool_arg(&args, "server")?;
            let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            if port == 0 {
                return Err("缺少参数 port（1-65535）".into());
            }
            let r = crate::observe::port_check(state, server, port).await?;
            Ok(serde_json::to_value(&r).map_err(|e| e.to_string())?)
        }
        "upload_file" => {
            let server = tool_arg(&args, "server")?;
            let local_path = tool_arg(&args, "local_path")?;
            let remote_path = tool_arg(&args, "remote_path")?;
            let bytes = ops::ai_upload_file(state, server, local_path, remote_path).await?;
            Ok(serde_json::json!({
                "bytesUploaded": bytes,
                "note": "本地敏感路径（私钥/.pem/.env 等）直接拒绝；远端禁写路径拒绝；其余需人工批准"
            }))
        }
        "download_file" => {
            let server = tool_arg(&args, "server")?;
            let remote_path = tool_arg(&args, "path")?;
            let saved = ops::ai_download_file(state, server, remote_path).await?;
            Ok(serde_json::json!({ "savedTo": saved }))
        }
        "append_file" => {
            let server = tool_arg(&args, "server")?;
            let path = tool_arg(&args, "path")?;
            let content = tool_arg(&args, "content")?;
            let bytes = ops::ai_append_file(state, server, path, content).await?;
            Ok(serde_json::json!({
                "bytesAfterAppend": bytes,
                "note": "仅限已有文件（追加语义）；远端禁写路径拒绝；需人工批准"
            }))
        }
        "restore_file" => {
            let server = tool_arg(&args, "server")?;
            let backup_id = tool_arg(&args, "backup_id")?;
            let bytes = ops::ai_restore_file(state, server, backup_id).await?;
            Ok(serde_json::json!({
                "bytesRestored": bytes,
                "note": "已回滚到写前备份状态（需人工批准）"
            }))
        }
        "list_file_backups" => {
            let server = args.get("server").and_then(|v| v.as_str());
            let items = crate::fbackups::list(&state.dir, server);
            Ok(serde_json::to_value(&items).map_err(|e| e.to_string())?)
        }
        other => Err(format!(
            "未知工具: {other}（可用: list_servers / exec_command / server_status / read_file / write_file / list_files / tail_log / log_search / list_services / port_check / upload_file / download_file / append_file / restore_file / list_file_backups）"
        )),
    }
}
