// ServerDesk 桌面应用 Rust 后端入口。
mod approvals;
mod commands;
mod crypto;
mod guard;
mod ipc;
mod ops;
mod ssh;
mod state;
mod store;
mod vault;

use state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Manager};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法解析应用数据目录: {e}"))?;
            std::fs::create_dir_all(&dir)?;
            let store = Arc::new(store::Store::open(&dir.join("servers.db"))?);
            let vault = Arc::new(vault::Vault::open(&dir, &dir.join("servers.db"))?);
            match vault.try_keychain_auto_unlock() {
                Ok(true) => eprintln!("保险库：已通过系统钥匙串自动解锁"),
                Ok(false) => {}
                Err(e) => eprintln!("保险库：钥匙串自动解锁失败（{e}），需要手动解锁"),
            }
            let guard = guard::Guard::load(&store);

            // 空闲自动锁巡检：每 30s 检查一次，锁上时广播事件给前端
            let vault_for_task = vault.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    if vault_for_task.check_auto_lock() {
                        eprintln!("保险库：空闲超时已自动锁定");
                        let _ = handle.emit("vault-locked", ());
                    }
                }
            });

            let state = Arc::new(AppState {
                store,
                vault,
                ssh: ssh::SshPool::new(),
                guard,
                approvals: approvals::Approvals::default(),
                handle: app.handle().clone(),
                dir: dir.clone(),
            });
            app.manage(state.clone());

            // 本地 IPC（MCP sidecar 桥）：token 文件 0600，端口随机
            let state_for_ipc = state.clone();
            let dir_for_ipc = dir.clone();
            tauri::async_runtime::spawn(async move {
                ipc::serve(state_for_ipc, dir_for_ipc).await;
            });

            eprintln!("ServerDesk 数据目录: {}", dir.display());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 保险库
            commands::vault_status,
            commands::vault_setup,
            commands::vault_unlock,
            commands::vault_lock,
            commands::vault_set_settings,
            commands::vault_reset,
            // 服务器资产
            commands::list_servers,
            commands::save_server,
            commands::delete_server,
            // 连接测试
            commands::test_connection,
            commands::ssh_test,
            // SSH 连接池
            commands::ssh_connect,
            commands::ssh_disconnect,
            commands::ssh_status,
            commands::ssh_exec,
            commands::ssh_reset_hostkey,
            // AI 安全管线
            commands::guard_info,
            commands::guard_set_rules,
            commands::mcp_set_enabled,
            // 审批
            commands::approvals_pending,
            commands::approval_decide,
            // 审计
            commands::audit_list,
            // MCP 接入信息
            commands::mcp_info
        ])
        .run(tauri::generate_context!())
        .expect("ServerDesk 启动失败");
}
