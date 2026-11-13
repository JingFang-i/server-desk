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
