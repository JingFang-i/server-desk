//! ServerDesk MCP sidecar：stdio MCP Server（newline-delimited JSON-RPC 2.0）。
//!
//! 职责：协议桥。把 tools/call 转发到主应用的本地 IPC（127.0.0.1 + Bearer token），
//! 全部安全策略（高危拦截/审批/审计/脱敏/凭据保密）由主应用执行。
//! sidecar 不读取数据库、不持有凭据；应用未运行或保险库锁定时工具调用返回明确错误。
//!
//! 工具：list_servers / exec_command / server_status
//!
//! 接入示例（Claude Desktop / Cursor 等客户端配置）：
//! { "mcpServers": { "serverdesk": { "command": "/path/to/serverdesk-mcp", "args": [] } } }

use serde_json::{json, Value};
use shared::{IpcRequest, IpcResponse};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::Duration;

const CALL_TIMEOUT: Duration = Duration::from_secs(150); // 覆盖主应用 120s 审批窗口

#[tokio::main]
async fn main() {
    let dir = match shared::data_dir() {
        Some(d) => d,
        None => {
            eprintln!("无法定位 ServerDesk 数据目录");
            std::process::exit(1);
        }
    };
    eprintln!(
        "serverdesk-mcp v{} 就绪（等待 MCP 客户端初始化）",
        env!("CARGO_PKG_VERSION")
    );

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    loop {
        line.clear();
        let n = match stdin.read_line(&mut line).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break; // stdin 关闭，客户端退出
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // 忽略非 JSON 行
        };
        if let Some(reply) = handle_msg(&dir, msg).await {
            println!("{}", serde_json::to_string(&reply).unwrap_or_default());
        }
    }
}

/// 处理一条 MCP 消息；返回需要回发的 JSON-RPC 应答（通知类返回 None）。
async fn handle_msg(dir: &PathBuf, msg: Value) -> Option<Value> {
    let method = msg.get("method").and_then(|m| m.as_str())?;
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // 无 id 的消息按通知处理（不应答）
    let reply = |result: Result<Value, (i64, String)>| -> Option<Value> {
        let id = id.clone()?;
        Some(match result {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message }
            }),
        })
    };

    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2024-11-05")
                .to_string();
            reply(Ok(json!({
                "protocolVersion": requested,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "serverdesk", "version": env!("CARGO_PKG_VERSION") }
            })))
        }
        "notifications/initialized" | "initialized" => None,
        "tools/list" => reply(Ok(json!({
            "tools": [
                {
                    "name": "list_servers",
                    "description": "列出已录入的服务器（名称/地址/用户名等元信息；绝不包含密码或私钥）",
                    "inputSchema": { "type": "object", "properties": {}, "required": [] }
                },
                {
                    "name": "exec_command",
                    "description": "在指定服务器上执行 shell 命令。经过安全管线：凭据泄露类命令直接拒绝；高危命令需用户在 ServerDesk 应用内人工批准（120 秒超时）；输出自动脱敏。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "command": { "type": "string", "description": "要执行的命令" },
                            "timeout_secs": { "type": "number", "description": "超时秒数（1-300，默认 30）" }
                        },
                        "required": ["server", "command"]
                    }
                },
                {
                    "name": "server_status",
                    "description": "只读探查服务器状态：uptime / CPU 核数 / 内存 / 根分区磁盘 / 负载",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" }
                        },
                        "required": ["server"]
                    }
                },
                {
                    "name": "list_files",
                    "description": "列出远程目录内容（ls -lah，最多 200 条）。命中敏感路径保护规则的目录会被拒绝。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "path": { "type": "string", "description": "远程目录绝对路径" }
                        },
                        "required": ["server", "path"]
                    }
                },
                {
                    "name": "tail_log",
                    "description": "读取远程日志文件尾部（1-2000 行，默认 200），适合排查服务故障而不撑爆上下文。敏感路径保护生效。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "path": { "type": "string", "description": "远程日志文件绝对路径" },
                            "lines": { "type": "number", "description": "尾部行数（默认 200，上限 2000）" }
                        },
                        "required": ["server", "path"]
                    }
                },
                {
                    "name": "log_search",
                    "description": "在远程日志中检索（grep，最多 200 个匹配、上下文 0-5 行）。敏感路径保护生效。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "path": { "type": "string", "description": "远程日志文件绝对路径" },
                            "pattern": { "type": "string", "description": "检索文本（字面量，非正则）" },
                            "ignore_case": { "type": "boolean", "description": "忽略大小写（默认 false）" },
                            "max_matches": { "type": "number", "description": "最大匹配数（默认 50，上限 200）" },
                            "context": { "type": "number", "description": "上下文行数（默认 0，上限 5）" }
                        },
                        "required": ["server", "path", "pattern"]
                    }
                },
                {
                    "name": "list_services",
                    "description": "列出远程服务器的服务状态表（systemd/service），只读。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" }
                        },
                        "required": ["server"]
                    }
                },
                {
                    "name": "port_check",
                    "description": "检查远程服务器指定端口的监听状态（ss/netstat），只读。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "port": { "type": "number", "description": "端口号（1-65535）" }
                        },
                        "required": ["server", "port"]
                    }
                },
                {
                    "name": "read_file",
                    "description": "读取远程服务器上的文本文件（上限 512KB，内容自动脱敏）。含凭据/敏感信息的路径（私钥、/etc/shadow、.env 等）会被直接拒绝。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "path": { "type": "string", "description": "远程文件绝对路径" }
                        },
                        "required": ["server", "path"]
                    }
                },
                {
                    "name": "write_file",
                    "description": "写入远程服务器上的文本文件（上限 1MB，整文件覆盖）。需要用户在 ServerDesk 应用内人工批准（120 秒超时自动拒绝）；系统安全相关路径（sshd_config、authorized_keys、shell 配置等）直接拒绝。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "server": { "type": "string", "description": "服务器 id 或精确名称" },
                            "path": { "type": "string", "description": "远程文件绝对路径" },
                            "content": { "type": "string", "description": "要写入的完整文本内容（覆盖原文件）" }
                        },
                        "required": ["server", "path", "content"]
                    }
                }
            ]
        }))),
        "tools/call" => {
            if id.is_none() {
                return None;
            }
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let text = match call_app(dir, name, &args).await {
                Ok(data) => {
                    let pretty = serde_json::to_string_pretty(&data).unwrap_or_default();
                    json!({ "content": [ { "type": "text", "text": pretty } ] })
                }
                Err(e) => json!({
                    "content": [ { "type": "text", "text": e } ],
                    "isError": true
                }),
            };
            reply(Ok(text))
        }
        "ping" => reply(Ok(json!({}))),
        other => reply(Err((-32601, format!("未知方法: {other}")))),
    }
}

/// 读取端口/token 并调用主应用 IPC（每次调用都重读，兼容应用重启换端口）。
async fn call_app(dir: &PathBuf, tool: &str, args: &Value) -> Result<Value, String> {
    let port_path = dir.join("ipc-port");
    let token_path = dir.join("ipc-token");
    let port: u16 = std::fs::read_to_string(&port_path)
        .map_err(|_| "ServerDesk 应用未运行（无法读取 ipc-port）；请先启动 ServerDesk".to_string())?
        .trim()
        .parse()
        .map_err(|_| "ipc-port 内容损坏".to_string())?;
    let token = std::fs::read_to_string(&token_path)
        .map_err(|_| "无法读取 ipc-token（请确认由同一用户运行）".to_string())?
        .trim()
        .to_string();

    let body = serde_json::to_vec(&IpcRequest {
        tool: tool.to_string(),
        args: args.clone(),
    })
    .map_err(|e| e.to_string())?;

    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port)))
            .await
            .map_err(|_| "连接主应用超时".to_string())?
            .map_err(|e| format!("连接主应用失败（请确认 ServerDesk 正在运行）：{e}"))?;

    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    tokio::time::timeout(CALL_TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .map_err(|_| "调用超时（可能审批流程仍在进行）".to_string())?
        .map_err(|e| format!("读取响应失败: {e}"))?;

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("响应格式错误")?;
    let body = &raw[split + 4..];
    let resp: IpcResponse = serde_json::from_slice(body).map_err(|e| format!("响应解析失败: {e}"))?;
    if resp.ok {
        Ok(resp.data)
    } else {
        Err(resp
            .error
            .unwrap_or_else(|| "未知错误（主应用未返回错误信息）".into()))
    }
}
