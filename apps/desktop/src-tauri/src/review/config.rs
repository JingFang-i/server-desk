//! AI 智能审查（R8）配置与 API Key 存储。
//!
//! 持久化约定（对齐项目安全红线 1：任何代码路径不得输出明文凭据）：
//! - `ai_review_config`：非敏感配置，明文 JSON 存 meta 表；
//! - `ai_review_key_blob`：`base64(AES-256-GCM(保险库 DEK, api_key))`，绝不明文落盘；
//! - 任何 IPC 命令不得把明文 Key 回传前端，只给掩码（[`mask_key`]）；
//! - 保险库锁定时 `dek()` 失败 → Key 不可读。这与 MCP 工具的失败顺序天然一致：
//!   所有工具在此之前已因拿不到服务器凭据而失败。

use crate::crypto::{self, KEY_LEN};
use crate::store::Store;
use crate::vault::Vault;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const META_CONFIG: &str = "ai_review_config";
pub const META_KEY_BLOB: &str = "ai_review_key_blob";

/// 通信协议。V1 只实现 OpenAI 兼容（DeepSeek / Kimi / GLM / 硅基流动 /
/// OpenAI / Ollama / LM Studio 均兼容）；Anthropic 预留 Phase 2。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    OpenAiCompat,
}

impl Default for Protocol {
    fn default() -> Self {
        Self::OpenAiCompat
    }
}

/// 审查调用失败（超时 / 网络 / 解析失败）时的行为。
/// D2 已拍板（2026-11-25）：默认 `Escalate`——降级到人工审批兜底，而不是静默放行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailurePolicy {
    /// 失败 → 转人工审批（默认；审计记 degraded）
    Escalate,
    /// 失败 → 按静态结论放行（等于该层静默失效，UI 必须明示）
    Allow,
    /// 失败 → 拒绝执行（高合规场景）
    Block,
}

impl Default for FailurePolicy {
    fn default() -> Self {
        Self::Escalate
    }
}

impl FailurePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Escalate => "escalate",
            Self::Allow => "allow",
            Self::Block => "block",
        }
    }
}

/// AI 判 deny 时的行为。
/// D1 已拍板（2026-11-25）：默认 `Escalate`——误判成本落在「多点一次审批」，
/// 而不是把合法操作硬堵死；人的最终裁决权保留（红线 3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DenyMode {
    /// deny → 转人工审批（默认，审批卡标红 + AI 理由）
    Escalate,
    /// deny → 直接拒绝，不产生审批卡
    Reject,
}

impl Default for DenyMode {
    fn default() -> Self {
        Self::Escalate
    }
}

impl DenyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Escalate => "escalate",
            Self::Reject => "reject",
        }
    }
}

/// AI 审查配置。默认值即 D1–D3 的拍板结论（见 docs/r8-ai-review-design.md §8）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub protocol: Protocol,
    /// 单次审查请求总超时（毫秒），保存时钳制到 1000–15000
    pub timeout_ms: u32,
    /// 送审文本（脱敏后）长度上限（字节），保存时钳制到 1024–65536
    pub max_input_bytes: usize,
    /// 单次审查的输出 token 预算，保存时钳制到 64–4096。
    /// 推理模型（如 DeepSeek-R1、openbmb MiniCPM）需要更大预算——
    /// 预算太小会截断思维链，content 恒为空，审查永远失败。
    pub max_tokens: u32,
    pub failure_policy: FailurePolicy,
    pub deny_mode: DenyMode,
    /// Phase 2 预留：文件类工具（write/append/upload）是否送审
    pub review_files: bool,
    /// D3 已拍板：是否把服务器分组/名称一并发给模型，默认 false
    pub send_server_context: bool,
    /// 审查结论缓存 TTL（秒），0 = 不缓存（R8.2 接入缓存）
    pub cache_ttl_secs: u64,
    /// 提示词版本：改 prompt 必须一起升版本，缓存 key 与日志都带它
    pub prompt_version: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            protocol: Protocol::default(),
            timeout_ms: 4000,
            max_input_bytes: 8192,
            max_tokens: 120,
            failure_policy: FailurePolicy::default(),
            deny_mode: DenyMode::default(),
            review_files: false,
            send_server_context: false,
            cache_ttl_secs: 1800,
            // v2：few-shot 锚定判定粒度（R8.3）；缓存 key 含此版本，升版本即失效旧结论
            prompt_version: "v2".to_string(),
        }
    }
}

impl ReviewConfig {
    /// 保存前的归一化：去空白、钳制数值范围。
    pub fn normalized(mut self) -> Self {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.model = self.model.trim().to_string();
        self.timeout_ms = self.timeout_ms.clamp(1000, 15000);
        self.max_input_bytes = self.max_input_bytes.clamp(1024, 65536);
        self.max_tokens = self.max_tokens.clamp(64, 4096);
        self.cache_ttl_secs = self.cache_ttl_secs.min(86400);
        self
    }

    /// 结构校验：enabled 时 base_url 与 model 必填；协议必须是 https，
    /// 仅本机回环 http（Ollama / LM Studio 本地部署）放行。
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && (self.base_url.is_empty() || self.model.is_empty()) {
            return Err("启用 AI 审查前必须填写 BaseUrl 与模型名称".into());
        }
        if !self.base_url.is_empty() {
            self.endpoint()?;
        }
        if self.prompt_version.trim().is_empty() {
            return Err("prompt_version 不能为空".into());
        }
        Ok(())
    }

    pub fn endpoint(&self) -> Result<reqwest::Url, String> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| "BaseUrl 格式无效".to_string())?;
        let loopback = is_loopback(&url);
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("BaseUrl 必须是 https://（或本机回环 HTTP）".into());
        }
        if url.host().is_none() || !url.username().is_empty() || url.password().is_some()
            || url.query().is_some() || url.fragment().is_some()
        {
            return Err("BaseUrl 必须是服务地址，不能携带凭据、查询参数或片段".into());
        }
        url.path_segments_mut()
            .map_err(|_| "BaseUrl 路径无效".to_string())?
            .pop_if_empty()
            .extend(["chat", "completions"]);
        Ok(url)
    }

}

pub(super) fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// 读配置（无配置 / 解析失败 → 默认值：审查关闭，行为等同 R7）。
pub fn load_config(store: &Store) -> ReviewConfig {
    store
        .get_meta(META_CONFIG)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 配置与密文同一次提交，避免重启后拿到另一端点的凭据。
pub fn save_config(store: &Store, cfg: &ReviewConfig, key_blob: Option<&str>) -> Result<(), String> {
    let json = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
    match key_blob {
        Some(blob) => store.set_meta_values(&[(META_CONFIG, &json), (META_KEY_BLOB, blob)]),
        None => store.set_meta(META_CONFIG, &json),
    }
}

// ---------- API Key：加密落盘，绝不回传前端 ----------

/// 用 DEK 加密并 base64 编码（纯函数，便于单测）。
pub fn encode_key_blob(dek: &[u8; KEY_LEN], key: &str) -> Result<String, String> {
    let blob = crypto::encrypt(dek, key.trim().as_bytes())?;
    Ok(B64.encode(blob))
}

/// 解码并解密（纯函数，便于单测）。
pub fn decode_key_blob(dek: &[u8; KEY_LEN], blob: &str) -> Result<String, String> {
    let raw = B64
        .decode(blob)
        .map_err(|_| "API Key 数据损坏（base64）".to_string())?;
    let plain = crypto::decrypt(dek, &raw)?;
    String::from_utf8(plain).map_err(|_| "API Key 数据损坏（UTF-8）".to_string())
}

/// 读取 API Key（未配置返回 None；保险库锁定时返回错误）。
pub fn load_api_key(store: &Store, vault: &Vault) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    let Some(blob) = store.get_meta(META_KEY_BLOB)?.filter(|blob| !blob.is_empty()) else {
        return Ok(None);
    };
    let dek = vault.dek().map_err(|e| e.to_string())?;
    decode_key_blob(&dek, &blob).map(zeroize::Zeroizing::new).map(Some)
}

/// 是否已配置 Key（不读取内容）。
pub fn has_key(store: &Store) -> bool {
    store
        .get_meta(META_KEY_BLOB)
        .ok()
        .flatten()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Key 掩码：`sk-****wxyz`；过短或为空时全打码。
/// 这是唯一允许离开 Rust 层的 Key 形态。
pub fn mask_key(key: &str) -> String {
    let t = key.trim();
    if t.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    format!(
        "{}****{}",
        chars[..3].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

/// 回传前端的完整信息：配置 + Key 掩码（绝无明文）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInfo {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub protocol: Protocol,
    pub timeout_ms: u32,
    pub max_input_bytes: usize,
    pub failure_policy: FailurePolicy,
    pub deny_mode: DenyMode,
    pub review_files: bool,
    pub send_server_context: bool,
    pub cache_ttl_secs: u64,
    pub prompt_version: String,
    pub has_key: bool,
    pub key_mask: String,
}

impl ReviewInfo {
    pub fn of(store: &Store, vault: &Vault, cfg: &ReviewConfig) -> Self {
        let key_mask = if has_key(store) {
            // 掩码需要明文：保险库解锁时可得；锁定时不透露任何内容细节
            match load_api_key(store, vault) {
                Ok(Some(k)) => mask_key(&k),
                _ => "••••".to_string(),
            }
        } else {
            String::new()
        };
        Self {
            enabled: cfg.enabled,
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            protocol: cfg.protocol,
            timeout_ms: cfg.timeout_ms,
            max_input_bytes: cfg.max_input_bytes,
            failure_policy: cfg.failure_policy,
            deny_mode: cfg.deny_mode,
            review_files: cfg.review_files,
            send_server_context: cfg.send_server_context,
            cache_ttl_secs: cfg.cache_ttl_secs,
            prompt_version: cfg.prompt_version.clone(),
            has_key: has_key(store),
            key_mask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_decisions() {
        let c = ReviewConfig::default();
        assert!(!c.enabled, "默认关闭：未配置时行为与 R7 一致");
        assert_eq!(c.deny_mode, DenyMode::Escalate, "D1：deny 转人工审批");
        assert_eq!(
            c.failure_policy,
            FailurePolicy::Escalate,
            "D2：失败转人工审批"
        );
        assert!(!c.send_server_context, "D3：默认不发服务器上下文");
    }

    #[test]
    fn config_json_roundtrip_preserves_enums() {
        let mut c = ReviewConfig::default();
        c.failure_policy = FailurePolicy::Block;
        c.deny_mode = DenyMode::Reject;
        c.protocol = Protocol::OpenAiCompat;
        let json = serde_json::to_string(&c).unwrap();
        let back: ReviewConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let back: ReviewConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(back.enabled);
        assert_eq!(back.timeout_ms, 4000);
        assert_eq!(back.deny_mode, DenyMode::Escalate);
    }

    #[test]
    fn normalized_clamps_and_trims() {
        let mut c = ReviewConfig::default();
        c.base_url = " https://api.deepseek.com/v1/ ".into();
        c.model = " deepseek-chat ".into();
        c.timeout_ms = 50;
        c.max_input_bytes = 1;
        c.max_tokens = 1;
        c.cache_ttl_secs = 999_999;
        let n = c.normalized();
        assert_eq!(n.base_url, "https://api.deepseek.com/v1");
        assert_eq!(n.model, "deepseek-chat");
        assert_eq!(n.timeout_ms, 1000);
        assert_eq!(n.max_input_bytes, 1024);
        assert_eq!(n.max_tokens, 64);
        assert_eq!(n.cache_ttl_secs, 86400);
    }

    #[test]
    fn validate_requires_url_and_model_when_enabled() {
        let mut c = ReviewConfig::default();
        c.enabled = true;
        assert!(c.validate().is_err(), "缺 BaseUrl/model 必须拒绝");
        c.base_url = "ftp://x".into();
        c.model = "m".into();
        assert!(c.validate().is_err());
        c.base_url = "http://evil.example.com".into();
        assert!(c.validate().is_err(), "非回环 http 必须拒绝");
        c.base_url = "http://127.0.0.1:11434/v1".into();
        assert!(c.validate().is_ok(), "本地 Ollama 回环放行");
        c.base_url = "https://api.moonshot.cn/v1".into();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn endpoint_only_allows_loopback_http_and_plain_base_paths() {
        let mut c = ReviewConfig::default();
        for base in [
            "http://localhost.invalid/v1",
            "http://127.0.0.1.invalid/v1",
            "https://example.invalid/v1?debug=true",
        ] {
            c.base_url = base.into();
            assert!(c.endpoint().is_err());
        }
        c.base_url = "http://[::1]:11434/v1/".into();
        assert_eq!(c.endpoint().unwrap().path(), "/v1/chat/completions");
    }

    #[test]
    fn key_blob_roundtrip() {
        let dek = [7u8; KEY_LEN];
        let blob = encode_key_blob(&dek, "sk-abc123secret").unwrap();
        assert!(!blob.contains("secret"), "密文不得含明文片段");
        assert_eq!(decode_key_blob(&dek, &blob).unwrap(), "sk-abc123secret");
    }

    #[test]
    fn key_blob_rejects_wrong_dek_and_garbage() {
        let blob = encode_key_blob(&[1u8; KEY_LEN], "sk-x").unwrap();
        assert!(decode_key_blob(&[2u8; KEY_LEN], &blob).is_err(), "换 DEK 必须解不开");
        assert!(decode_key_blob(&[1u8; KEY_LEN], "not-base64!!").is_err());
    }

    #[test]
    fn mask_never_reveals_full_key() {
        assert_eq!(mask_key("sk-abcdef1234567890xyz"), "sk-****0xyz");
        assert_eq!(mask_key("short"), "*****");
        assert_eq!(mask_key(""), "");
        assert_eq!(mask_key("   "), "");
        let m = mask_key("sk-1234567890");
        assert!(!m.contains("1234567890"), "掩码不得残留可还原片段");
    }
}
