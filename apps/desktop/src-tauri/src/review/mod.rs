//! AI 智能审查（R8）模块入口。
//!
//! 分期落地（见 docs/r8-ai-review-design.md）：
//! - R8.1（本次）：配置/Key 存储、Reviewer 运行时、设置用 IPC 命令；
//! - R8.2：`verdict` 合并（AI 只收紧不放宽）+ `ops.rs` 管线接入；
//! - R8.3：审查 prompt 与输出解析（`prompt.rs` / `verdict.rs`）；
//! - R8.5：`ai_review_log` 审查日志（`log.rs`）。
//!
//! 安全不变式（方案 §1，R8.2 起由 property test 守护）：
//! 最终判定 = max(静态规则, AI 判定)，序关系 Allow < NeedsApproval < Blocked；
//! 静态 Blocked 的命令**不送审**——模型没有任何机会否决硬规则。

pub mod client;
pub mod config;

use crate::review::config::{load_config, save_config};
pub use crate::review::config::{ReviewConfig, ReviewInfo};
use crate::store::Store;
use std::sync::RwLock;

/// 审查器运行时：配置热更新 + 复用的 HTTP 连接池。
/// API Key 不在此常驻——每次使用时经 `vault.dek()` 现解（见 config.rs 模块注释）。
pub struct Reviewer {
    cfg: RwLock<ReviewConfig>,
    http: reqwest::Client,
}

impl Reviewer {
    /// 启动时从 meta 加载；未配置 = 审查关闭（行为与 R7 完全一致）。
    pub fn load(store: &Store) -> Self {
        let cfg = load_config(store);
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(1500))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            cfg: RwLock::new(cfg),
            http,
        }
    }

    pub fn config(&self) -> ReviewConfig {
        self.cfg.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// 保存并热生效（无需重启）。
    pub fn set_config(&self, store: &Store, cfg: ReviewConfig) -> Result<(), String> {
        save_config(store, &cfg)?;
        *self.cfg.write().unwrap_or_else(|p| p.into_inner()) = cfg;
        // R8.2：此处将同步清空审查结论缓存——模型/策略变更后，旧结论不可复用
        Ok(())
    }

    pub fn info(&self, store: &Store, vault: &crate::vault::Vault) -> ReviewInfo {
        ReviewInfo::of(store, vault, &self.config())
    }

    /// 复用的 HTTP 连接池（连接超时 1.5s；总超时按每次请求的配置）。
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

/// 审查日志条目。R8.5 由 `ai_review_log` 表填充；R8.1 先定契约供前端联调。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEntry {
    pub id: i64,
    pub ts: String,
    pub tool: String,
    pub server_name: String,
    /// AI 原始判定：allow | review | deny | error | skipped
    pub ai_verdict: String,
    /// 合并后的最终判定：allow | approval | blocked
    pub final_verdict: String,
    pub risk: String,
    pub reason: String,
    pub model: String,
    pub latency_ms: i64,
    pub cached: bool,
    pub error: String,
}
