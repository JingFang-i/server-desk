// 系统通知权限状态：审批提醒的「后台可达」通道依赖它（R8.4 后置增强）。
// macOS 需要用户显式授权；Windows/Linux 的 WebView 通知通常默认放行。
// 查询/请求失败（如纯浏览器预览）返回 unknown，UI 按「无法确认」提示而不是误报。

import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";

export type NotifyPermission = "granted" | "denied" | "unknown";

export async function notifyPermission(): Promise<NotifyPermission> {
  try {
    return (await isPermissionGranted()) ? "granted" : "denied";
  } catch {
    return "unknown";
  }
}

/** 触发系统授权弹窗；用户拒绝或弹窗失败返回 denied / unknown。 */
export async function requestNotifyPermission(): Promise<NotifyPermission> {
  try {
    const r = await requestPermission();
    return r === "granted" ? "granted" : "denied";
  } catch {
    return "unknown";
  }
}
