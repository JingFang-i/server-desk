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
    pub approvals: Approvals,
    pub ai_rate: crate::guard::AiRate,
    pub handle: tauri::AppHandle,
    pub dir: PathBuf,
    /// 审计落库失败次数：红线要求「逐次审计」，失败必须可感知而不是静默吞掉。
    pub audit_failures: AtomicU64,
}
