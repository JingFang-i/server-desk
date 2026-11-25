//! 审批到达提醒：系统原生通知 + 声音 + 程序坞/任务栏注意力提示。
//!
//! 触发点只有一处：`ops` 里新登记一条待审批操作时调用 [`approval_requested`]。
//! 通知正文只包含服务器名与命令摘要（经脱敏、截断），绝不携带凭据；
//! 前端另外叠加应用内提示音与角标（`ApprovalAlert.vue`），两者互不依赖。

use crate::approvals::PendingApproval;
use crate::text::{summary, SUMMARY_MAX};
use tauri::{AppHandle, Manager, UserAttentionType};
use tauri_plugin_notification::NotificationExt;

/// 系统通知声音名（macOS 系统音；Windows/Linux 无对应资源时由插件忽略）。
const NOTIFICATION_SOUND: &str = "Glass";

/// 初始化通知插件：点击通知时把主窗口唤到前台（不抢占焦点创建新窗口）。
pub fn init(app: &AppHandle) {
    let handle = app.clone();
    if let Err(e) = app.notification().on_action(move |_| {
        if let Some(win) = handle.get_webview_window("main") {
            let _ = win.unminimize();
            let _ = win.show();
            let _ = win.set_focus();
        }
    }) {
        eprintln!("通知点击回调注册失败: {e}");
    }
}

/// 新审批到达：任务栏/程序坞提示 + 系统通知（带声音）。
///
/// 传入的 `command` 必须已经是**脱敏后**的摘要（由 `ops::register_approval` 统一产出）；
/// 这里只做单行折叠与长度预算，不再拼装原始命令。
/// 失败不影响审批流程本身（提前返回，写 stderr），审批仍可在应用内处理。
pub fn approval_requested(app: &AppHandle, pending: &PendingApproval) {
    if let Some(win) = app.get_webview_window("main") {
        // macOS 程序坞跳动 / Windows 任务栏闪烁；窗口已在前台时系统会自动忽略
        let _ = win.request_user_attention(Some(UserAttentionType::Critical));
    }

    let body = format!(
        "「{}」请求执行：{}",
        pending.server_name,
        summary(&pending.command, SUMMARY_MAX)
    );
    if let Err(e) = app
        .notification()
        .builder()
        .title("ServerDesk · AI 操作待审批")
        .body(&body)
        .sound(NOTIFICATION_SOUND)
        .show()
    {
        eprintln!("审批通知发送失败: {e}");
    }
}

/// 查询或申请系统权限。没有可用平台查询时保留「未知」状态。
#[tauri::command]
pub async fn approval_notification_permission(app: AppHandle, request: bool) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        if tauri::is_dev() {
            return "unknown";
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        if app
            .run_on_main_thread(move || macos_permission(request, tx))
            .is_ok()
        {
            if let Ok(Ok(status)) = tokio::time::timeout(
                std::time::Duration::from_secs(if request { 120 } else { 3 }),
                rx,
            )
            .await
            {
                return status;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, request);
    "unknown"
}

#[cfg(target_os = "macos")]
fn macos_permission(request: bool, tx: tokio::sync::oneshot::Sender<&'static str>) {
    use block2::RcBlock;
    use objc2::{
        class, msg_send,
        runtime::{AnyObject, Bool},
    };
    use std::sync::Mutex;

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    // 系统通知中心需要 .app 包身份。命令行调试进程没有身份时直接返回未知。
    let bundle: *mut AnyObject = unsafe { msg_send![class!(NSBundle), mainBundle] };
    let identifier: *mut AnyObject = unsafe { msg_send![bundle, bundleIdentifier] };
    if identifier.is_null() {
        let _ = tx.send("unknown");
        return;
    }
    let center: *mut AnyObject =
        unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] };
    let tx = Mutex::new(Some(tx));
    let send = move |status| {
        if let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(status);
        }
    };
    if request {
        let callback = RcBlock::new(move |granted: Bool, error: *mut AnyObject| {
            send(if !error.is_null() {
                "unknown"
            } else if granted.as_bool() {
                "granted"
            } else {
                "denied"
            });
        });
        // UserNotifications 的 badge、sound、alert 权限位。
        let options = 0b111usize;
        let _: () = unsafe {
            msg_send![center, requestAuthorizationWithOptions: options, completionHandler: &*callback]
        };
    } else {
        let callback = RcBlock::new(move |settings: *mut AnyObject| {
            if settings.is_null() {
                send("unknown");
                return;
            }
            let status: isize = unsafe { msg_send![settings, authorizationStatus] };
            send(match status {
                0 => "prompt",
                1 => "denied",
                2 | 3 => "granted",
                _ => "unknown",
            });
        });
        let _: () =
            unsafe { msg_send![center, getNotificationSettingsWithCompletionHandler: &*callback] };
    }
}
