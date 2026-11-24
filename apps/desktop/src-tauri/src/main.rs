// ServerDesk 桌面应用 Rust 后端入口。
// Windows release 构建使用 GUI 子系统，避免启动时弹出控制台窗口；
// debug（tauri dev）下保留控制台以便查看日志输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod approvals;
mod backup;
mod commands;
mod crypto;
mod fbackups;
mod guard;
mod ipc;
mod notify;
mod observe;
mod ops;
mod ssh;
mod state;
mod store;
mod text;
mod vault;

#[cfg(target_os = "macos")]
use tauri::menu::SubmenuBuilder;
use tauri::menu::{MenuBuilder, MenuItem, PredefinedMenuItem};
use state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Manager};

/// Windows release（GUI 子系统）下 stdout/stderr 句柄为 NULL，
/// 任何 println!/eprintln! 都会因写失败而 panic 导致应用崩溃。
/// 启动时把标准输出/错误重定向到 NUL 设备，日志安全丢弃。
#[cfg(all(windows, not(debug_assertions)))]
mod win_stdio {
    const STD_OUTPUT_HANDLE: u32 = 1;
    const STD_ERROR_HANDLE: u32 = 2;

    extern "system" {
        fn SetStdHandle(nStdHandle: u32, hHandle: *mut core::ffi::c_void) -> i32;
    }

    pub fn redirect_to_nul() {
        use std::os::windows::io::IntoRawHandle;
        let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") else {
            return;
        };
        // 句柄所有权移交给系统：进程生命周期内保持有效
        let handle = nul.into_raw_handle();
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
}

fn main() {
    #[cfg(all(windows, not(debug_assertions)))]
    win_stdio::redirect_to_nul();

    // Linux（deepin/UOS 等国产系统常见）：WebKitGTK 的 DMABUF 渲染器在部分
    // GPU/驱动组合（含虚拟机）下渲染黑屏。禁用后走备用合成路径，界面恢复正常，
    // 代价仅为少量合成性能，对管理工具界面无感知。用户已显式设置时不覆盖。
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
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
            // AI 下载目录里的明文文件 7 天后自动清理（配置文件常带口令/令牌）
            ops::cleanup_downloads_on_startup(&dir);

            // 审批到达提醒：注册通知点击回调（点击系统通知 → 唤起主窗口）
            notify::init(app.handle());

            // 原生应用菜单：mac/win 菜单里提供「检查更新」「锁定保险库」入口，
            // 触发后以事件形式转给前端（复用 UpdatePrompt 的检查与锁屏流程）
            let check_item = MenuItem::with_id(
                app,
                "menu-check-update",
                "检查更新…",
                true,
                if cfg!(target_os = "macos") { Some("Cmd+U") } else { Some("Ctrl+U") },
            )?;
            let lock_item = MenuItem::with_id(
                app,
                "menu-lock-vault",
                "锁定保险库",
                true,
                if cfg!(target_os = "macos") { Some("Cmd+L") } else { Some("Ctrl+L") },
            )?;
            let quit_item = PredefinedMenuItem::quit(app, None)?;
            let sep_item = PredefinedMenuItem::separator(app)?;
            #[cfg(target_os = "macos")]
            let menu = {
                let sub = SubmenuBuilder::new(app, "ServerDesk")
                    .about(None)
                    .separator()
                    .item(&check_item)
                    .separator()
                    .item(&lock_item)
                    .separator()
                    .quit()
                    .build()?;
                MenuBuilder::new(app).item(&sub).build()?
            };
            #[cfg(not(target_os = "macos"))]
            let menu = MenuBuilder::new(app)
                .items(&[&check_item, &lock_item, &sep_item, &quit_item])
                .build()?;
            app.set_menu(menu.clone())?;
            app.on_menu_event(move |handle, event| {
                let _ = match event.id().as_ref() {
                    "menu-check-update" => handle.emit("menu-check-update", ()),
                    "menu-lock-vault" => handle.emit("menu-lock-vault", ()),
                    _ => Ok(()),
                };
            });

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
                audit_failures: std::sync::atomic::AtomicU64::new(0),
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
            commands::audit_page,
            commands::audit_sessions,
            commands::audit_failures,
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
            // 变更记录
            commands::file_backups_list,
            commands::file_backup_restore,
            // 私钥文件导入
            commands::read_key_file
        ])
        .run(tauri::generate_context!())
        .expect("ServerDesk 启动失败");
}
