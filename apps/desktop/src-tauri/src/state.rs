//! 全局应用状态。

use crate::{approvals::Approvals, guard::Guard, ssh::SshPool, store::Store, vault::Vault};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

pub struct AppState {
    pub store: Arc<Store>,
    pub vault: Arc<Vault>,
    pub ssh: SshPool,
    pub guard: Guard,
    /// AI 智能审查器（R8）：配置热更新 + HTTP 连接池；
    /// API Key 不常驻内存，使用时经 vault.dek() 现解（见 review/config.rs）。
    pub reviewer: crate::review::Reviewer,
    pub approvals: Approvals,
    pub ai_rate: crate::guard::AiRate,
    /// AI 审查调用限流（次/分钟）：与工具调用限流分开，保护审查 API 成本
    pub review_rate: crate::guard::AiRate,
    pub handle: tauri::AppHandle,
    pub dir: PathBuf,
    /// 审计落库失败次数：红线要求「逐次审计」，失败必须可感知而不是静默吞掉。
    pub audit_failures: AtomicU64,
}
