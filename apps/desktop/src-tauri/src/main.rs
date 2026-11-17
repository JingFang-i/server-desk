// ServerDesk 桌面应用 Rust 后端入口。
mod approvals;
mod backup;
mod commands;
mod crypto;
mod fbackups;
mod guard;
mod ipc;
mod observe;
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
        .plugin(tauri_plugin_dialog::init())
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
            let vault_for_lock = vault.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    if vault_for_lock.check_auto_lock() {
                        eprintln!("保险库：空闲超时已自动锁定");
                        let _ = handle.emit("vault-locked", ());
                    }
                }
            });

            // AI 调用日志保留策略：启动时清理一次，之后每 6 小时巡检
            // （audit_log 中 source=ai 的记录仅保留最近 7 天）
            let store_for_audit = store.clone();
            tauri::async_runtime::spawn(async move {
                const RETENTION_DAYS: i64 = 7;
                let purge = |store: &store::Store| {
                    let cutoff = (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS))
                        .to_rfc3339();
                    match store.purge_audit_before(&cutoff, Some("ai")) {
                        Ok(0) => {}
                        Ok(n) => eprintln!("审计清理：已删除 {n} 条超过 {RETENTION_DAYS} 天的 AI 调用记录"),
                        Err(e) => eprintln!("审计清理失败: {e}"),
                    }
                };
                purge(&store_for_audit);
                loop {
                    tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
                    purge(&store_for_audit);
                }
            });

            // 自动备份巡检：每 10 分钟检查是否到期（到期且保险库解锁才执行）
            let store_for_bk = store.clone();
            let vault_for_bk = vault.clone();
            let dir_for_bk = dir.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(600)).await;
                    let enabled = store_for_bk
                        .get_meta("auto_backup_enabled")
                        .ok()
                        .flatten()
                        .map(|s| s == "1")
                        .unwrap_or(false);
                    if !enabled {
                        continue;
                    }
                    let interval: i64 = store_for_bk
                        .get_meta("auto_backup_interval_hours")
                        .ok()
                        .flatten()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(24);
                    let due = match store_for_bk.get_meta("auto_backup_last").ok().flatten() {
                        None => true,
                        Some(t) => chrono::DateTime::parse_from_rfc3339(&t)
                            .map(|d| {
                                d < chrono::Utc::now() - chrono::Duration::hours(interval)
                            })
                            .unwrap_or(true),
                    };
                    if !due {
                        continue;
                    }
                    let keep: usize = store_for_bk
                        .get_meta("auto_backup_keep")
                        .ok()
                        .flatten()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(7);
                    match backup::run_auto_backup(&store_for_bk, &vault_for_bk, &dir_for_bk, keep) {
                        Ok(p) => {
                            let _ = store_for_bk.set_meta("auto_backup_last", &chrono::Utc::now().to_rfc3339());
                            eprintln!("自动备份完成: {}", p.display());
                        }
                        Err(e) => eprintln!("自动备份跳过/失败: {e}"),
                    }
                }
            });

            let state = Arc::new(AppState {
                store,
                vault,
                ssh: ssh::SshPool::new(),
                guard,
                approvals: approvals::Approvals::default(),
                ai_rate: guard::AiRate::new(),
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
            commands::mcp_info,
            // 备份与恢复
            commands::backup_info,
            commands::backup_export,
            commands::backup_restore,
            commands::auto_backup_now,
            commands::auto_backup_save_settings,
            commands::auto_backup_restore,
            commands::auto_backup_delete,
            // SFTP 文件传输
            commands::sftp_upload,
            commands::sftp_download,
            // 私钥文件导入
            commands::read_key_file
        ])
        .run(tauri::generate_context!())
        .expect("ServerDesk 启动失败");
}
