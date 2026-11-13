//! 全局应用状态。

use crate::{ssh::SshPool, store::Store, vault::Vault};
use std::sync::Arc;

pub struct AppState {
    pub store: Arc<Store>,
    pub vault: Arc<Vault>,
    pub ssh: SshPool,
}
