//! 跨进程共享类型：服务器资产模型、工具契约、IPC 协议。
//! 主应用（Tauri）与 MCP sidecar 共同依赖本 crate，保证契约一致。

use serde::{Deserialize, Serialize};

/// 服务器资产（前端与 sidecar 共用的标准形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default = "default_true")]
    pub allow_ai: bool,
    pub created_at: String,
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

/// 认证方式：账密 或 SSH 私钥。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    #[serde(rename_all = "camelCase")]
    Password {
        password: String,
    },
    #[serde(rename_all = "camelCase")]
    Key {
        private_key: String,
        passphrase: Option<String>,
    },
}

impl AuthMethod {
    pub fn kind(&self) -> &'static str {
        match self {
            AuthMethod::Password { .. } => "password",
            AuthMethod::Key { .. } => "key",
        }
    }
}

/// 连接测试结果（M1 为 TCP 层探测，M2 升级为完整 SSH 认证测试）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub message: String,
}

// ---------- MCP sidecar ⇄ 主应用 的本地 IPC 契约 ----------

/// sidecar → 主应用 的工具调用请求（经 127.0.0.1 HTTP + Bearer token）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// sidecar 进程级会话 id（同一次 MCP 客户端会话聚合审计用）
    #[serde(default)]
    pub session: String,
}

/// 主应用 → sidecar 的统一应答。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
}

impl IpcResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self { ok: true, data, error: None }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, data: serde_json::Value::Null, error: Some(msg.into()) }
    }
}

/// 应用数据目录（主应用 Tauri app_data_dir 与 sidecar 推导保持一致）。
pub fn data_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join("Library/Application Support/com.qsmx.serverdesk"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|h| std::path::PathBuf::from(h).join("com.qsmx.serverdesk"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|h| {
            let mut p = match std::env::var_os("XDG_DATA_HOME") {
                Some(x) => std::path::PathBuf::from(x),
                None => {
                    let mut p = std::path::PathBuf::from(h);
                    p.push(".local/share");
                    p
                }
            };
            p.push("com.qsmx.serverdesk");
            p
        })
    }
}

/// MCP 工具名清单（顺序即 sidecar `tools/list` 的返回顺序）。
///
/// AGENTS.md 要求「新增工具时同步更新 sidecar → 共享契约 → guard 管线」；
/// 把清单放在这里，sidecar 的单测会逐一比对，漏改一处就会失败。
pub const MCP_TOOL_NAMES: &[&str] = &[
    "list_servers",
    "exec_command",
    "server_status",
    "list_files",
    "tail_log",
    "log_search",
    "list_services",
    "port_check",
    "upload_file",
    "download_file",
    "append_file",
    "restore_file",
    "list_file_backups",
    "exec_batch",
    "read_file",
    "write_file",
];

/// 审计动作名（单一来源）。Rust 写入、`api/index.ts` 的 `AuditAction`、前端筛选共用这一套。
/// 新增动作时必须同时更新：本清单 → TS 类型 → 前端按动作筛选的地方。
pub mod action {
    pub const MCP_EXEC_COMMAND: &str = "mcp_exec_command";
    pub const MCP_EXEC_BATCH: &str = "mcp_exec_batch";
    pub const MCP_READ_FILE: &str = "mcp_read_file";
    pub const MCP_WRITE_FILE: &str = "mcp_write_file";
    pub const MCP_APPEND_FILE: &str = "mcp_append_file";
    pub const MCP_UPLOAD_FILE: &str = "mcp_upload_file";
    pub const MCP_DOWNLOAD_FILE: &str = "mcp_download_file";
    pub const MCP_RESTORE_FILE: &str = "mcp_restore_file";
    pub const MCP_LIST_FILE_BACKUPS: &str = "mcp_list_file_backups";
    pub const MCP_LIST_FILES: &str = "mcp_list_files";
    pub const MCP_TAIL_LOG: &str = "mcp_tail_log";
    pub const MCP_LOG_SEARCH: &str = "mcp_log_search";
    pub const MCP_LIST_SERVICES: &str = "mcp_list_services";
    pub const MCP_PORT_CHECK: &str = "mcp_port_check";
    pub const MCP_SERVER_STATUS: &str = "mcp_server_status";
    pub const MCP_LIST_SERVERS: &str = "mcp_list_servers";
    pub const MCP_APPROVAL_REQUESTED: &str = "mcp_approval_requested";
    pub const MCP_APPROVAL_GRANTED: &str = "mcp_approval_granted";
    pub const MCP_SET_ENABLED: &str = "mcp_set_enabled";

    /// 全部已登记动作（可用于校验/导出）
    pub const ALL: &[&str] = &[
        MCP_EXEC_COMMAND,
        MCP_EXEC_BATCH,
        MCP_READ_FILE,
        MCP_WRITE_FILE,
        MCP_APPEND_FILE,
        MCP_UPLOAD_FILE,
        MCP_DOWNLOAD_FILE,
        MCP_RESTORE_FILE,
        MCP_LIST_FILE_BACKUPS,
        MCP_LIST_FILES,
        MCP_TAIL_LOG,
        MCP_LOG_SEARCH,
        MCP_LIST_SERVICES,
        MCP_PORT_CHECK,
        MCP_SERVER_STATUS,
        MCP_LIST_SERVERS,
        MCP_APPROVAL_REQUESTED,
        MCP_APPROVAL_GRANTED,
        MCP_SET_ENABLED,
    ];
}
