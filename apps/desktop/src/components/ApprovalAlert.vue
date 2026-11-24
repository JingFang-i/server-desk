<script setup lang="ts">
import { h, onMounted, onUnmounted } from "vue";
import { NButton, useNotification } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type PendingApproval } from "../api";
import { playApprovalChime, primeAlertSound } from "../alert";

/**
 * 全局审批提醒：新审批到达 → 提示音 + 应用内通知 + 菜单角标；
 * 待审批未处理期间每 30 秒再响一次（审批 120 秒后自动拒绝，避免静默过期）。
 * 系统级通知与程序坞/任务栏提示由 Rust 侧（notify.rs）发送，两者互为补充。
 */
const emit = defineEmits<{ (e: "count", n: number): void; (e: "open"): void }>();

const notification = useNotification();
const POLL_MS = 3000;
const REMIND_MS = 30000;

let pollTimer: number | undefined;
let remindTimer: number | undefined;
let unlisten: UnlistenFn | undefined;
let pendingCount = 0;

async function refresh() {
  try {
    const list = await api.approvalsPending();
    pendingCount = list.length;
    emit("count", pendingCount);
  } catch {
    /* 锁定状态 / 后端未就绪：静默，等待下次轮询 */
  }
}

function summarize(item: PendingApproval): string {
  const command = item.command.replace(/\s+/g, " ").trim();
  return command.length > 110 ? `${command.slice(0, 110)}…` : command;
}

function alertOnce(item: PendingApproval) {
  playApprovalChime(true);
  notification.warning({
    title: "AI 操作待审批",
    duration: 10000,
    keepAliveOnHover: true,
    content: () =>
      h("div", { style: "display:flex;flex-direction:column;gap:8px" }, [
        h(
          "div",
          { style: "font-size:13px;line-height:1.6;word-break:break-all" },
          `「${item.serverName}」请求执行：${summarize(item)}`
        ),
        h(
          "div",
          { style: "font-size:12px;opacity:.6" },
          "120 秒内未处理将自动拒绝"
        ),
        h(
          NButton,
          { size: "tiny", type: "primary", onClick: () => emit("open") },
          { default: () => "去审批中心" }
        ),
      ]),
  });
}

function onPointerDown() {
  primeAlertSound();
}
function onKeyDown() {
  primeAlertSound();
}

onMounted(async () => {
  // 首次用户交互时解锁音频（自动播放策略）
  window.addEventListener("pointerdown", onPointerDown, { once: true, capture: true });
  window.addEventListener("keydown", onKeyDown, { once: true, capture: true });

  await refresh();
  try {
    unlisten = await listen<PendingApproval>("approval-requested", (event) => {
      alertOnce(event.payload);
      refresh();
    });
  } catch {
    /* 事件通道不可用（如纯浏览器预览）时仍靠轮询更新角标 */
  }
  pollTimer = window.setInterval(refresh, POLL_MS);
  remindTimer = window.setInterval(() => {
    if (pendingCount > 0) playApprovalChime(true);
  }, REMIND_MS);
});

onUnmounted(() => {
  window.removeEventListener("pointerdown", onPointerDown, true);
  window.removeEventListener("keydown", onKeyDown, true);
  if (pollTimer) window.clearInterval(pollTimer);
  if (remindTimer) window.clearInterval(remindTimer);
  unlisten?.();
});
</script>

<template>
  <span style="display: none" />
</template>
