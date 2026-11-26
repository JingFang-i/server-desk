import { createApp } from "vue";
import { createPinia } from "pinia";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App.vue";
import FloatApp from "./FloatApp.vue";
import "./styles.css";

/**
 * 主窗口与 AI 实时活动悬浮窗共用同一份前端产物，按窗口 label 分流：
 * - `main` → 完整应用
 * - `activity-float` → 只挂紧凑的活动面板，不跑保险库/更新/审批中心等主界面逻辑
 *   （各自独立 webview，事件流由 Tauri 广播，两边都会收到）
 */
function windowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    // 纯浏览器预览没有 Tauri 运行时：按主窗口处理
    return "main";
  }
}

if (windowLabel() === "activity-float") {
  // 透明窗口：全局 styles.css 给 body 上的深色底会挡住圆角与流光边
  document.documentElement.classList.add("is-float");
  createApp(FloatApp).use(createPinia()).mount("#app");
} else {
  createApp(App).use(createPinia()).mount("#app");
}
