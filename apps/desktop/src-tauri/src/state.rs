//! 全局应用状态。

use crate::{approvals::Approvals, guard::Guard, ssh::SshPool, store::Store, vault::Vault};
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppState {
    pub store: Arc<Store>,
    pub vault: Arc<Vault>,
    pub ssh: SshPool,
    pub guard: Guard,
    pub approvals: Approvals,
    pub handle: tauri::AppHandle,
    pub dir: PathBuf,
}
