//! 本地 IPC 服务：MCP sidecar ⇄ 主应用。
//! 仅监听 127.0.0.1 随机端口；每次启动生成新 token（0600 文件），
//! sidecar 每次调用时从数据目录读取，应用重启后无需重新配置客户端。
//! 主应用持有全部凭据与 SSH 会话；sidecar 只能通过该通道走安全管线。

use crate::ops;
use crate::state::AppState;
use shared::{IpcRequest, IpcResponse};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Emitter;
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

    let resp = ops::with_session(req.session, async {
        let gate = check_tool_access(&state, &req.tool);
        // 被入口拒绝的调用只保留状态，避免查询资产或广播原始参数。
        let (server, detail) = if gate.is_ok() {
            (
                activity_server_label(&state, &req.tool, &req.args),
                activity_detail(&state, &req.tool, &req.args),
            )
        } else {
            (String::new(), String::new())
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let tool = crate::text::summary(&state.guard.sanitize(&req.tool), 64);
        let _ = state.handle.emit("tool-activity", serde_json::json!({
            "id": request_id, "phase": "start", "tool": tool, "server": server, "detail": detail,
        }));
        let resp = match gate {
            Ok(()) => match handle_tool(&state, &req.tool, req.args).await {
                Ok(data) => IpcResponse::ok(data),
                Err(e) => IpcResponse::err(e),
            },
            Err(e) => IpcResponse::err(e),
        };
        let _ = state.handle.emit("tool-activity", serde_json::json!({
            "id": request_id, "phase": "done", "tool": tool, "server": server, "detail": detail,
            "ok": activity_succeeded(&req.tool, &resp),
        }));
        resp
    }).await;
    let out = serde_json::to_vec(&resp).map_err(|e| e.to_string())?;
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        out.len()
    );
    stream.write_all(http.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(&out).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// 实时活动窗口里显示的「服务器」标签。
///
/// AI 传的 `server` 既可能是服务器 id（UUID）也可能是名称；原样回显会让活动窗口里
/// 一半显示 UUID、一半显示名字（看起来就像「没显示服务器名」）。这里统一解析：
/// - 命中资产库（按 id 或名称）→ 回显**服务器名**
/// - `exec_batch` 按 servers 列表 / group 汇总成人类可读的一行
/// - 解析不到（服务器已删除等）→ 脱敏后原样回显，不留空
/// 全程只读资产表、只用于展示，不参与任何安全判定。
fn activity_server_label(state: &AppState, tool: &str, args: &serde_json::Value) -> String {
    let clean = |raw: &str| crate::text::summary(&state.guard.sanitize(raw), 120);
    let resolve = |raw: &str| -> String {
        let raw = raw.trim();
        if raw.is_empty() {
            return String::new();
        }
        match state.store.get(raw) {
            Ok(Some(row)) => clean(&row.name),
            _ => match state.store.find_by_name(raw) {
                Ok(Some(row)) => clean(&row.name),
                _ => clean(raw),
            },
        }
    };
    match tool {
        // 唯一不针对具体服务器的工具：活动窗口显示「全局」
        "list_servers" => String::new(),
        "exec_batch" => {
            if let Some(label) = args
                .get("servers")
                .and_then(|v| v.as_array())
                .and_then(|list| batch_activity_label(list, resolve))
            {
                return label;
            }
            match args.get("group").and_then(|v| v.as_str()) {
                Some(group) => clean(&format!("分组「{group}」")),
                None => String::new(),
            }
        }
        _ => args
            .get("server")
            .or_else(|| args.get("group"))
            .and_then(|v| v.as_str())
            .map(resolve)
            .unwrap_or_default(),
    }
}

fn batch_activity_label(
    list: &[serde_json::Value],
    resolve: impl FnMut(&str) -> String,
) -> Option<String> {
    let targets = list.iter().filter_map(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    let count = targets.clone().count();
    if count == 0 {
        return None;
    }
    let names = targets.take(3).map(resolve).collect::<Vec<_>>().join("、");
    Some(if count <= 3 { names } else { format!("{names} 等 {count} 台") })
}

#[test]
fn batch_activity_resolves_only_displayed_targets() {
    let input = serde_json::json!(["a", " ", 1, "b", "c", "d", "e"]);
    let mut resolved = Vec::new();
    let label = batch_activity_label(input.as_array().unwrap(), |name| {
        resolved.push(name.to_string());
        name.to_uppercase()
    });
    assert_eq!(resolved, ["a", "b", "c"]);
    assert_eq!(label.as_deref(), Some("A、B、C 等 5 台"));
}

/// 活动记录展开时显示的内容：这条调用**到底做了什么**。
///
/// 只看工具名（`exec_command`）等于什么都没说，用户点开记录要的是那条命令/那个路径。
/// 统一走 `guard.sanitize`（口令、令牌、私钥片段一律 `[REDACTED]`）+ 字符截断，
/// 事件只发往本机两个 webview，不落盘、不出网。
fn activity_detail(state: &AppState, tool: &str, args: &serde_json::Value) -> String {
    use crate::text::{truncate, ACTIVITY_DETAIL_MAX};
    let clean = |raw: &str| truncate(&state.guard.sanitize(raw), ACTIVITY_DETAIL_MAX);
    let arg = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    match tool {
        "list_servers" => "列出全部服务器资产".to_string(),
        "exec_command" => clean(arg("command")),
        "exec_batch" => {
            let targets = args
                .get("servers")
                .and_then(|v| v.as_array())
                .map(|list| list.len())
                .unwrap_or(0);
            let group = arg("group");
            let scope = if !group.is_empty() {
                format!("分组「{group}」")
            } else if targets > 0 {
                format!("{targets} 台")
            } else {
                "批量目标".to_string()
            };
            clean(&format!("[{scope}] {}", arg("command")))
        }
        "server_status" => "固定模板：uptime / CPU / 内存 / 磁盘 / 负载".to_string(),
        "read_file" | "list_files" => clean(&format!("{}", arg("path"))),
        "write_file" | "append_file" => clean(&format!(
            "{}（内容 {} 字符）",
            arg("path"),
            arg("content").chars().count()
        )),
        "upload_file" => clean(&format!("{} → {}", arg("local_path"), arg("remote_path"))),
        "download_file" => clean(arg("path")),
        "restore_file" => clean(&format!("备份回滚 {}", arg("backup_id"))),
        "list_file_backups" => {
            let server = arg("server");
            if server.is_empty() {
                "列出全部写前备份".to_string()
            } else {
                clean(&format!("「{server}」的写前备份"))
            }
        }
        "tail_log" => clean(&format!(
            "{}（末 {} 行）",
            arg("path"),
            args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200)
        )),
        "log_search" => clean(&format!("{} 中搜索「{}」", arg("path"), arg("pattern"))),
        "list_services" => "列出 systemd 服务状态".to_string(),
        "port_check" => format!(
            "检查端口 {}",
            args.get("port").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        other => clean(other),
    }
}

fn activity_succeeded(tool: &str, resp: &IpcResponse) -> bool {
    fn exited_cleanly(data: &serde_json::Value) -> bool {
        data["timedOut"].as_bool() != Some(true) && data["exitCode"].as_u64() == Some(0)
    }
    resp.ok && match tool {
        "exec_command" | "server_status" => exited_cleanly(&resp.data),
        "exec_batch" => resp.data["results"].as_array().is_some_and(|entries| {
            entries.iter().all(|entry| entry["ok"].as_bool() == Some(true) && exited_cleanly(entry))
        }),
        _ => true,
    }
}

#[cfg(test)]
#[test]
fn activity_result_distinguishes_exit_status_and_partial_batch() {
    let success = IpcResponse::ok(serde_json::json!({"exitCode": 0, "timedOut": false}));
    let timeout = IpcResponse::ok(serde_json::json!({"exitCode": 0, "timedOut": true}));
    let batch = IpcResponse::ok(serde_json::json!({
        "results": [{"ok": true, "exitCode": 0}, {"ok": false, "exitCode": 1}],
    }));
    assert!(activity_succeeded("exec_command", &success));
    assert!(!activity_succeeded("exec_command", &timeout));
    assert!(!activity_succeeded("exec_batch", &batch));
    assert!(!activity_succeeded("list_servers", &IpcResponse::err("未执行")));
}

fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn check_tool_access(state: &AppState, tool: &str) -> Result<(), String> {
    if !state.guard.ai_enabled() {
        return Err("MCP 工具已被禁用：请在应用「设置」中开启 AI 接入".into());
    }
    state
        .ai_rate
        .check(state.guard.rate_per_min())
        .map_err(|e| {
            ops::audit(state, "ai", None, "rate_limited", tool, "blocked");
            e
        })?;
    if !state.vault.is_unlocked() {
        return Err("保险库已锁定：请在应用中解锁后再使用 MCP 工具".into());
    }
    Ok(())
}

/// 已通过入口检查；各操作继续走自己的审批、审计与脱敏管线。
async fn handle_tool(
    state: &Arc<AppState>,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
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
            ops::audit(state, "ai", None, shared::action::MCP_LIST_SERVERS, "列出服务器", "ok");
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
            let r = ops::run_fixed_template(state, server, shared::action::MCP_SERVER_STATUS, cmd, 30).await?;
            Ok(serde_json::json!({
                "server": server,
                "report": r.stdout,
                "exitCode": r.exit_code,
                "truncated": r.truncated,
                "timedOut": r.timed_out,
            }))
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
            // 红线 4：每台服务器独立 allow_ai。备份注册表里有远端路径与大小，
            // 未开放 AI 的服务器不得被枚举。
            let allowed: std::collections::HashSet<String> = state
                .store
                .list()?
                .into_iter()
                .filter(|r| r.allow_ai)
                .map(|r| r.id)
                .collect();
            let filter = match args.get("server").and_then(|v| v.as_str()) {
                Some(reference) => {
                    let cfg = ops::load_server_config(state, reference)?;
                    if !cfg.allow_ai {
                        ops::audit(
                            state,
                            "ai",
                            Some(&cfg.id),
                            shared::action::MCP_LIST_FILE_BACKUPS,
                            reference,
                            "blocked:not_allow_ai",
                        );
                        return Err(format!("服务器「{}」未开放 AI 操作（allowAi=false）", cfg.name));
                    }
                    Some(cfg.id)
                }
                None => None,
            };
            let items: Vec<_> = crate::fbackups::list(&state.dir, filter.as_deref())
                .into_iter()
                .filter(|m| allowed.contains(&m.server_id))
                .collect();
            ops::audit(state, "ai", None, shared::action::MCP_LIST_FILE_BACKUPS, "列出写前备份", "ok");
            Ok(serde_json::to_value(&items).map_err(|e| e.to_string())?)
        }
        "exec_batch" => {
            let command = tool_arg(&args, "command")?;
            let servers: Option<Vec<String>> = args.get("servers").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
            });
            let group = args.get("group").and_then(|v| v.as_str()).map(String::from);
            let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(30);
            if servers.is_none() && group.is_none() {
                return Err("缺少参数 servers（数组）或 group（分组名）".into());
            }
            ops::exec_batch(state, servers.as_deref(), group.as_deref(), command, timeout_secs).await
        }
        other => Err(format!(
            "未知工具: {other}（可用: list_servers / exec_command / server_status / read_file / write_file / list_files / tail_log / log_search / list_services / port_check / upload_file / download_file / append_file / restore_file / list_file_backups / exec_batch）"
        )),
    }
}
