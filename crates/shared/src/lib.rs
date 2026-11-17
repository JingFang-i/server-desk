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
