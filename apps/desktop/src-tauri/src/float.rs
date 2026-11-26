//! AI 实时活动悬浮窗。
//!
//! 与主窗口**同生共死**的对等窗口：启动时一起出现，主窗口最小化 / 关闭都不影响它；
//! 真正退出只有一条路径——菜单栏托盘图标的「退出」。
//!
//! 三种形态（前端 `FloatApp.vue` 决定，后端只负责改窗口尺寸）：
//! - **球**（默认最小形态）：流光圆球，里面跑马灯显示最新一条；左键点开 → 活动条，
//!   右键出原生菜单（打开主面板 / 展开 / 关闭）
//! - **条**：340×78，标题栏 + 最新一条活动
//! - **列表**：340×300，标题栏 + 审批提示条 + 活动列表（点记录展开命令内容）
//!
//! macOS 细节：窗口操作必须回主线程（否则 WebKit 判定
//! `crashDueToApplicationCallingMainThreadOnlyWebKitAPIFromBackgroundThread` 直接 SIGKILL）；
//! 显示用 `orderFrontRegardless`，不用 `show()`（后者会激活应用并抢键盘焦点）。

use tauri::menu::{ContextMenu, MenuBuilder};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "activity-float";

/// 悬浮窗尺寸（无边框；鹈鹕插画与路面文字全部由前端画）
const SIZE_ORB: (f64, f64) = (168.0, 118.0);

/// 距屏幕右边缘的边距
const MARGIN: f64 = 18.0;
/// 收起态底边距：鹈鹕抬到 Dock 上方，同时给下方展开的小窗留出空间。
/// 展开时**顶边不动、窗口向下长**，所以两种形态共用这个顶边锚点。
const ANCHOR_BOTTOM: f64 = 96.0;
/// 启动后多久亮出悬浮窗：等主窗口先出来，避免和首帧、主密码输入抢时序
const SHOW_DELAY_MS: u64 = 900;

/// 持久化开关：0 = 关闭悬浮窗（启动不显示），缺省 = 开启
pub const META_ENABLED: &str = "float_window_enabled";

/// 创建悬浮窗（初始隐藏，[`show_on_start`] 随后亮出）。
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("ServerDesk · AI 实时活动")
        .inner_size(SIZE_ORB.0, SIZE_ORB.1)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .focused(false)
        .visible(false)
        .shadow(false)
        .build()?;
    // 台前调度 / 多桌面：加入所有 Space，切舞台时不跟着被收走
    let _ = win.set_visible_on_all_workspaces(true);
    place(app, &win);

    // 右键菜单的选项回到前端处理（前端掌握形态与持久化状态）
    let handle = app.clone();
    win.on_menu_event(move |_window, event| {
        let action = match event.id().as_ref() {
            "float-open-main" => "open-main",
            "float-hide" => "hide",
            _ => return,
        };
        let _ = handle.emit("float-menu", action);
    });
    Ok(win)
}

/// 贴到主屏右下角。
fn place(app: &AppHandle, win: &WebviewWindow) {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return;
    };
    let scale = monitor.scale_factor();
    let screen = monitor.size().to_logical::<f64>(scale);
    let origin = monitor.position().to_logical::<f64>(scale);
    let width = win
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale).width)
        .unwrap_or(SIZE_ORB.0);
    let x = origin.x + screen.width - width - MARGIN;
    let y = origin.y + screen.height - SIZE_ORB.1 - ANCHOR_BOTTOM;
    let _ = win.set_position(tauri::LogicalPosition::new(x, y));
}

/// 在主线程序列化执行。AppKit / WebKit 的窗口操作只能碰主线程。
fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| format!("调度主线程失败: {e}"))?;
    rx.recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| format!("等待主线程超时: {e}"))
}

/// 显示悬浮窗：**不抢焦点**。
fn show_float(win: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::{msg_send, runtime::AnyObject};
        let raw = win.ns_window().map_err(|e| e.to_string())?;
        if raw.is_null() {
            return Err("取不到 NSWindow".into());
        }
        let ns_window = raw as *mut AnyObject;
        unsafe {
            let _: () = msg_send![ns_window, orderFrontRegardless];
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        win.show().map_err(|e| e.to_string())
    }
}

/// 读取持久化开关（缺省开启）。
pub fn enabled(store: &crate::store::Store) -> bool {
    store
        .get_meta(META_ENABLED)
        .ok()
        .flatten()
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// 启动后亮出悬浮窗（主窗口与它同时启动，各自独立显示）。
pub fn show_on_start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(SHOW_DELAY_MS)).await;
        let app_for_main = app.clone();
        let result = on_main(&app, move || {
            let Some(float) = app_for_main.get_webview_window(LABEL) else {
                return Err("悬浮窗不存在".to_string());
            };
            place(&app_for_main, &float);
            show_float(&float)
        })
        .unwrap_or_else(|e| Err(e));
        if let Err(e) = &result {
            eprintln!("悬浮窗显示失败: {e}");
        }
        // 显示结果落审计：打包版看不到 stderr，「悬浮窗没出现」时这是唯一可查的证据
        if let Some(state) = app.try_state::<std::sync::Arc<crate::state::AppState>>() {
            let (detail, outcome) = match &result {
                Ok(()) => ("AI 实时活动悬浮窗已显示".to_string(), "ok"),
                Err(e) => (format!("AI 实时活动悬浮窗显示失败：{e}"), "failed"),
            };
            crate::ops::audit(&state, "user", None, "float_window", &detail, outcome);
        }
    });
}

/// 右键菜单：在悬浮窗上弹出原生菜单（选项通过 `float-menu` 事件回到前端）。
#[tauri::command]
pub fn float_context_menu(app: AppHandle) -> Result<(), String> {
    let menu = MenuBuilder::new(&app)
        .text("float-open-main", "打开主界面")
        .separator()
        .text("float-hide", "隐藏整个悬浮窗")
        .build()
        .map_err(|e| format!("构建菜单失败：{e}"))?;
    let win = app
        .get_window(LABEL)
        .ok_or_else(|| "悬浮窗不存在".to_string())?;
    // popup 在 macOS 上是模态的（阻塞到用户选择/关闭），menu 在这期间一直存活
    menu.popup(win).map_err(|e| format!("弹出菜单失败：{e}"))
}

/// 读取悬浮窗开关状态（设置页用）。
#[tauri::command]
pub fn get_float_enabled(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> bool {
    enabled(&state.store)
}

/// 开关悬浮窗并持久化（设置页开关 / 托盘菜单共用）：关掉后下次启动不再自动出现。
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let state = app
        .try_state::<std::sync::Arc<crate::state::AppState>>()
        .ok_or_else(|| "应用状态未就绪".to_string())?;
    state
        .store
        .set_meta(META_ENABLED, if enabled { "1" } else { "0" })?;
    if enabled {
        let app_for_main = app.clone();
        on_main(app, move || {
            let float = app_for_main
                .get_webview_window(LABEL)
                .ok_or_else(|| "悬浮窗尚未创建".to_string())?;
            place(&app_for_main, &float);
            show_float(&float)
        })?
    } else {
        if let Some(float) = app.get_webview_window(LABEL) {
            let _ = float.hide();
        }
        Ok(())
    }
}

#[tauri::command]
pub fn set_float_enabled(
    app: AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    enabled: bool,
) -> Result<(), String> {
    let _ = &state; // AppState 在 set_enabled 内部取用
    set_enabled(&app, enabled)
}

/// 显示悬浮窗（托盘菜单 / 设置页）。
#[tauri::command]
pub fn show_float_window(app: AppHandle) -> Result<(), String> {
    let app_for_main = app.clone();
    on_main(&app, move || {
        let float = app_for_main
            .get_webview_window(LABEL)
            .ok_or_else(|| "悬浮窗尚未创建（检查启动时的审计记录 float_window）".to_string())?;
        place(&app_for_main, &float);
        show_float(&float)
    })?
}

/// 隐藏悬浮窗。
#[tauri::command]
pub fn hide_float_window(app: AppHandle) {
    if let Some(float) = app.get_webview_window(LABEL) {
        let _ = float.hide();
    }
}

/// 唤起主窗口（悬浮窗 ↗ / 托盘「打开主界面」），可指定落地页。
///
/// 主窗口可能被关闭（只是 hide，进程还在）或最小化：
/// `unminimize` 必须在 `show` 之前，否则 macOS 上只调 `show` 不会还原最小化的窗口。
#[tauri::command]
pub fn show_main_window(app: AppHandle, page: Option<String>) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    let _ = main.unminimize();
    let _ = main.show();
    let _ = main.set_focus();
    if let Some(page) = page.filter(|p| !p.is_empty()) {
        let _ = app.emit("navigate", page);
    }
    Ok(())
}
