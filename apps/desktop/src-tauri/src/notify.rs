//! 审批到达提醒：系统原生通知 + 声音 + 程序坞/任务栏注意力提示。
//!
//! 触发点只有一处：`ops` 里新登记一条待审批操作时调用 [`approval_requested`]。
//! 通知正文只包含服务器名与命令摘要（经脱敏、截断），绝不携带凭据；
//! 前端另外叠加应用内提示音与角标（`ApprovalAlert.vue`），两者互不依赖。
//!
//! ## 为什么 macOS 上自己投递通知（而不是只用 tauri-plugin-notification）
//!
//! 插件底层是 `notify-rust` → `NSUserNotificationCenter`（10.14 起弃用）。那套 API
//! **不会**把应用注册进「系统设置 → 通知」，进程若从未申请过授权，投递会被系统直接丢弃，
//! 于是表现为「通知从来没弹过」，而应用里也查不到任何失败。macOS 上因此改为：
//! 启动时用 `UNUserNotificationCenter` 申请授权（仅未决时），投递走 `UNNotificationRequest`。
//! 非 macOS / 开发模式（没有 .app 包身份）仍回退到插件。

use crate::approvals::PendingApproval;
use crate::text::{summary, SUMMARY_MAX};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager, UserAttentionType};
use tauri_plugin_notification::NotificationExt;

/// 系统通知声音名（macOS 系统音；Windows/Linux 无对应资源时由插件忽略）。
const NOTIFICATION_SOUND: &str = "Glass";

/// 上报窗口标题（避免两处硬编码不一致）。
const NOTIFICATION_TITLE: &str = "ServerDesk · AI 操作待审批";

/// 前端 Web Audio 是否已能出声。能出声时不再叠加系统提示音，避免双响；
/// 出不了声（自动播放策略 / 页面被挂起）时由 Rust 侧原生发声兜底。
static WEB_AUDIO_READY: AtomicBool = AtomicBool::new(false);

/// 初始化通知插件：点击通知时把主窗口唤到前台（不抢占焦点创建新窗口）+ 申请授权。
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

    // 首次启动直接申请一次系统通知授权。
    //
    // 两个坑（第一版踩过）：
    // 1. 不要先查状态再决定要不要申请——启动瞬间 `run_on_main_thread`/查询容易超时，
    //    状态拿到 unknown 就把申请整个跳过了，表现为「应用从来没进过通知设置列表」；
    // 2. 申请要等事件循环真正跑起来（窗口已创建）之后再发，否则调度会失败。
    //
    // `requestAuthorization` 在已决定过时会立即带着当前状态回调、**不再弹框**，
    // 所以每次启动都调用是安全的，不会骚扰用户（拒绝后也不会反复弹）。
    let app_for_perm = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let status = approval_notification_permission(app_for_perm.clone(), true).await;
        eprintln!("系统通知授权状态: {status}");
        // 写一条审计：授权结果是「通知不弹」的第一顺位排查依据，
        // 而它发生在启动阶段、不在任何审批流程里，事后没有别的地方能查到。
        if let Some(state) = app_for_perm.try_state::<std::sync::Arc<crate::state::AppState>>() {
            crate::ops::audit(
                &state,
                "user",
                None,
                "notify_permission",
                &format!("启动时申请系统通知授权：{status}"),
                "ok",
            );
        }
    });
}

/// 新审批到达：任务栏/程序坞提示 + 系统通知（带声音）。
///
/// 传入的 `command` 必须已经是**脱敏后**的摘要（由 `ops::register_approval` 统一产出）；
/// 这里只做单行折叠与长度预算，不再拼装原始命令。
/// 失败不影响审批流程本身（写 stderr），审批仍可在应用内处理。
pub fn approval_requested(app: &AppHandle, pending: &PendingApproval) {
    let focused = match app.get_webview_window("main") {
        Some(win) => {
            // macOS 程序坞跳动 / Windows 任务栏闪烁；窗口已在前台时系统会自动忽略
            let _ = win.request_user_attention(Some(UserAttentionType::Critical));
            win.is_focused().unwrap_or(false)
        }
        None => false,
    };

    let body = format!(
        "「{}」请求执行：{}",
        pending.server_name,
        summary(&pending.command, SUMMARY_MAX)
    );
    #[cfg(target_os = "macos")]
    if !tauri::is_dev() {
        // 前台通知由系统抑制，声音兜底无需等待投递回调。
        let played_fallback = focused && !WEB_AUDIO_READY.load(Ordering::Relaxed);
        if played_fallback {
            play_system_alert(app);
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = macos::deliver(NOTIFICATION_TITLE, &body, None).await {
                eprintln!("原生审批通知投递失败: {error}");
                if !played_fallback && !WEB_AUDIO_READY.load(Ordering::Relaxed) {
                    play_system_alert(&app);
                }
            }
        });
        return;
    }

    let delivered = match deliver_plugin(app, NOTIFICATION_TITLE, &body) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("审批通知发送失败: {error}");
            false
        }
    };
    if (focused || !delivered) && !WEB_AUDIO_READY.load(Ordering::Relaxed) {
        play_system_alert(app);
    }
}

fn deliver_plugin(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .sound(NOTIFICATION_SOUND)
        .show()
        .map_err(|e| e.to_string())
}

/// 原生系统提示音（不依赖 WebView 的音频上下文）。
///
/// 审批到达时如果提示音通道可能失效（窗口前台 / 系统通知没弹 / Web Audio 被挂起），
/// 由 Rust 直接播放系统音：声音走系统音频通道，不受自动播放策略与页面生命周期影响。
pub fn play_system_alert(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        // NSSound 必须在主线程播放
        if let Err(e) = app.run_on_main_thread(|| {
            if let Err(e) = macos::play_sound() {
                eprintln!("系统提示音播放失败: {e}");
            }
        }) {
            eprintln!("系统提示音调度失败: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        non_macos_sound();
    }
}

/// Windows / Linux：尽力而为的系统提示音（无对应命令时静默跳过）。
#[cfg(not(target_os = "macos"))]
fn non_macos_sound() {
    #[cfg(target_os = "windows")]
    {
        // 无需额外依赖，直接触发系统默认提示音
        let _ = std::process::Command::new("rundll32")
            .arg("user32.dll,MessageBeep")
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        for (cmd, args) in [
            ("canberra-gtk-play", vec!["-i", "dialog-warning"]),
            ("paplay", vec!["/usr/share/sounds/freedesktop/stereo/dialog-warning.oga"]),
        ] {
            if std::process::Command::new(cmd).args(args).spawn().is_ok() {
                return;
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {}
}

/// 前端上报「Web Audio 是否已能出声」，供 [`approval_requested`] 决定是否原生兜底。
#[tauri::command]
pub fn alert_audio_ready(ready: bool) {
    WEB_AUDIO_READY.store(ready, Ordering::Relaxed);
}

/// 设置页「试听系统音」：直接播放原生提示音，验证不依赖 WebView 的声音通道。
#[tauri::command]
pub fn play_alert_sound(app: AppHandle) {
    play_system_alert(&app);
}

/// 设置页「发送测试通知」：走正式投递路径，当场验证系统通知通不通。
///
/// 为什么需要它：系统通知失败是**静默**的（没授权 / 请求被拒 → 直接丢弃），
/// 等到某次审批才发现没弹就太晚了。这里：
/// - 把系统的真实报错取回来（`UNNotificationRequest` 的完成回调），不再猜；
/// - 窗口在前台时**延迟 5 秒投递**——macOS 不给前台应用显示横幅（除非实现
///   `UNUserNotificationCenterDelegate`），延迟给用户切到别的应用的时间；
/// - 结果写审计（`notify_test`），事后可查，不依赖界面 toast。
#[tauri::command]
pub async fn approval_notification_test(app: AppHandle) -> Result<String, String> {
    let status = approval_notification_permission(app.clone(), false).await;
    if status == "denied" {
        return Err(
            "系统通知已被拒绝：请到「系统设置 → 通知 → ServerDesk」允许通知后再试".into(),
        );
    }
    if status == "prompt" {
        // 先弹系统授权框；此时立刻投递会被丢掉，让用户允许后再点一次
        approval_notification_permission(app, true).await;
        return Err("已弹出系统授权框：允许通知后再点一次「发送测试通知」".into());
    }

    let focused = app
        .get_webview_window("main")
        .and_then(|win| win.is_focused().ok())
        .unwrap_or(false);
    let delay = (focused && cfg!(target_os = "macos") && !tauri::is_dev()).then_some(5.0);
    let body = "如果你看到这条通知，说明系统通知通道正常——AI 请求审批时也会这样提醒";
    let (accepted, error, channel) =
        deliver_test(&app, "ServerDesk · 通知测试", body, delay).await;

    let detail = format!(
        "auth={status} focused={focused} channel={channel} delay={:?} accepted={accepted} error={}",
        delay,
        error.as_deref().unwrap_or("none")
    );
    if let Some(state) = app.try_state::<std::sync::Arc<crate::state::AppState>>() {
        crate::ops::audit(
            &state,
            "user",
            None,
            "notify_test",
            &detail,
            if accepted && error.is_none() { "ok" } else { "failed" },
        );
    }

    if let Some(err) = error {
        return Err(format!("系统拒绝了这条通知：{err}"));
    }
    if !accepted {
        return Err("通知投递失败：请检查系统通知权限与勿扰/专注模式".into());
    }
    if delay.is_some() {
        // 这条提示要能看完再切走，前端用更长的停留时间展示
        Ok("已安排在 5 秒后投递——现在请立刻切到别的应用（或桌面），横幅会出现在那里。前台时 macOS 不给应用显示横幅，这不是故障。"
            .into())
    } else {
        Ok("已投递，请看屏幕右上角的通知".into())
    }
}

/// 复用正式投递通道，返回系统受理结果与通道名。
async fn deliver_test(
    app: &AppHandle,
    title: &str,
    body: &str,
    delay: Option<f64>,
) -> (bool, Option<String>, &'static str) {
    #[cfg(target_os = "macos")]
    {
        if !tauri::is_dev() {
            let result = macos::deliver(title, body, delay).await;
            return (result.is_ok(), result.err(), "native");
        }
    }
    let _ = delay;
    let result = deliver_plugin(app, title, body);
    (result.is_ok(), result.err(), "plugin")
}

/// 查询或申请系统权限。没有可用平台查询时保留「未知」状态。
///
/// 返回：granted / denied / prompt（未申请）/ dev（开发模式无法查询）/ unknown。
#[tauri::command]
pub async fn approval_notification_permission(app: AppHandle, request: bool) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        // 开发模式（pnpm tauri dev）没有 .app 包身份，UNUserNotificationCenter
        // 查询必然失败——返回专门状态让界面如实提示，而不是笼统的「无法确认」
        if tauri::is_dev() {
            return "dev";
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Err(e) = app.run_on_main_thread(move || macos_permission(request, tx)) {
            eprintln!("通知权限查询调度失败: {e}");
            return "unknown";
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(if request { 120 } else { 5 }),
            rx,
        )
        .await
        {
            Ok(Ok(status)) => return status,
            Ok(Err(_)) => eprintln!("通知权限查询通道中断"),
            Err(_) => eprintln!("通知权限查询超时（{}）", if request { "申请" } else { "查询" }),
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

    // 系统通知中心需要 .app 包身份。命令行调试进程没有身份时直接返回未知。
    let bundle: *mut AnyObject = unsafe { msg_send![class!(NSBundle), mainBundle] };
    let identifier: *mut AnyObject = unsafe { msg_send![bundle, bundleIdentifier] };
    if identifier.is_null() {
        let _ = tx.send("unknown");
        return;
    }
    let center: *mut AnyObject = macos::notification_center();
    if center.is_null() {
        let _ = tx.send("unknown");
        return;
    }
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

/// macOS 原生通知投递与系统音（UserNotifications / AppKit 的极薄封装）。
#[cfg(target_os = "macos")]
mod macos {
    use objc2::{
        class, msg_send,
        rc::{autoreleasepool, Retained},
        runtime::{AnyObject, Bool},
    };
    use std::ffi::CString;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    pub fn notification_center() -> *mut AnyObject {
        unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] }
    }

    fn nsstring(value: &str) -> *mut AnyObject {
        let Ok(text) = CString::new(value) else {
            return std::ptr::null_mut();
        };
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    /// NSString → Rust 字符串（用于取系统的错误描述）。
    unsafe fn ns_to_string(value: *mut AnyObject) -> String {
        if value.is_null() {
            return String::new();
        }
        let utf8: *const std::os::raw::c_char = unsafe { msg_send![value, UTF8String] };
        if utf8.is_null() {
            return String::new();
        }
        unsafe { std::ffi::CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned()
    }

    /// 等待系统确认受理，超时或回调丢失也作为投递失败。
    pub async fn deliver(title: &str, body: &str, delay_secs: Option<f64>) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        autoreleasepool(|_| submit(title, body, delay_secs, tx))?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "等待系统通知投递回调超时".to_string())?
            .map_err(|_| "系统通知投递未返回结果".to_string())?
    }

    fn submit(
        title: &str,
        body: &str,
        delay_secs: Option<f64>,
        tx: oneshot::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        unsafe {
            let center = notification_center();
            if center.is_null() {
                return Err("通知中心不可用".into());
            }
            let ns_title = nsstring(title);
            let ns_body = nsstring(body);
            if ns_title.is_null() || ns_body.is_null() {
                return Err("通知文本转换失败".into());
            }

            let content: Option<Retained<AnyObject>> = msg_send![class!(UNMutableNotificationContent), new];
            let content = content.ok_or("UNMutableNotificationContent 创建失败")?;
            let content = &*content;
            let _: () = msg_send![content, setTitle: ns_title];
            let _: () = msg_send![content, setBody: ns_body];
            let sound: *mut AnyObject = msg_send![class!(UNNotificationSound), defaultSound];
            if !sound.is_null() {
                let _: () = msg_send![content, setSound: sound];
            }

            let delay_secs = delay_secs.filter(|secs| *secs > 0.0);
            let trigger: *mut AnyObject = match delay_secs {
                Some(secs) => msg_send![
                    class!(UNTimeIntervalNotificationTrigger),
                    triggerWithTimeInterval: secs,
                    repeats: Bool::NO
                ],
                None => std::ptr::null_mut(),
            };
            if delay_secs.is_some() && trigger.is_null() {
                return Err("延迟触发器创建失败".into());
            }

            let uuid: *mut AnyObject = msg_send![class!(NSUUID), UUID];
            if uuid.is_null() {
                return Err("通知标识创建失败".into());
            }
            let identifier: *mut AnyObject = msg_send![uuid, UUIDString];
            if identifier.is_null() {
                return Err("通知标识创建失败".into());
            }
            let request: *mut AnyObject = msg_send![
                class!(UNNotificationRequest),
                requestWithIdentifier: identifier,
                content: content,
                trigger: trigger
            ];
            if request.is_null() {
                return Err("UNNotificationRequest 创建失败".into());
            }

            let tx = Mutex::new(Some(tx));
            let block = block2::RcBlock::new(move |error: *mut AnyObject| {
                let result = if error.is_null() {
                    Ok(())
                } else {
                    Err(ns_to_string(msg_send![error, localizedDescription]))
                };
                if let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = tx.send(result);
                }
            });
            let _: () = msg_send![
                center,
                addNotificationRequest: request,
                withCompletionHandler: &*block
            ];
        }
        Ok(())
    }

    /// 播放系统提示音（Glass，即通知音）。必须在主线程调用。
    pub fn play_sound() -> Result<(), String> {
        let name = CString::new("Glass").map_err(|_| "系统音名称非法".to_string())?;
        unsafe {
            let ns_name: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
            if ns_name.is_null() {
                return Err("系统音名称转换失败".into());
            }
            let sound: *mut AnyObject = msg_send![class!(NSSound), soundNamed: ns_name];
            if sound.is_null() {
                return Err("系统音 Glass 不存在".into());
            }
            let played: Bool = msg_send![sound, play];
            if !played.as_bool() {
                return Err("系统音播放被拒绝（可能没有输出设备）".into());
            }
        }
        Ok(())
    }
}
