// 通过桌面端查询系统权限。平台无法确认时避免误报已授权。
import { readonly, shallowRef } from "vue";
import { invoke } from "@tauri-apps/api/core";

type NotifyPermission = "granted" | "denied" | "prompt" | "unknown";
const permission = shallowRef<NotifyPermission>("unknown");
export const notifyPermissionState = readonly(permission);
let requesting = false;

export async function notifyPermission(request = false): Promise<NotifyPermission> {
  if (requesting) return permission.value;
  requesting = request;
  try {
    const status = await invoke<NotifyPermission>("approval_notification_permission", { request });
    if (request || !requesting) permission.value = status;
  } catch {
    if (request || !requesting) permission.value = "unknown";
  } finally {
    if (request) requesting = false;
  }
  return permission.value;
}
