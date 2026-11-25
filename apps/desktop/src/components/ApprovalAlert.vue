<script setup lang="ts">
import { computed, h, onMounted, onUnmounted, shallowRef } from "vue";
import { NButton, NTag, useMessage, useNotification } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type ApprovalReview, type PendingApproval } from "../api";
import ApprovalReviewNote from "./ApprovalReviewNote.vue";
import { alertAudioState, ensureAudio, playApprovalChime, primeAlertSound } from "../alert";
import { notifyPermission } from "../notifyStatus";

/**
 * 全局审批提醒：新审批到达 → 提示音 + 应用内通知 + 菜单角标；
 * 待审批未处理期间每 30 秒再响一次（审批 120 秒后自动拒绝，避免静默过期）。
 * 系统级通知与程序坞/任务栏提示由 Rust 侧（notify.rs）发送，两者互为补充。
 * 顶部横幅（R8 后置增强）：提示音未启用 / 系统通知未授权时显式提示，一键修复——
 * 「提示音一定会响」的两个前提（音频上下文已解锁、系统通知已授权）不再靠运气。
 */
const props = defineProps<{ /** 当前页面：已在审批中心时不再弹卡，只保留声音提醒 */ currentPage?: string }>();
const emit = defineEmits<{ (e: "count", n: number): void; (e: "open"): void }>();

const notification = useNotification();
const message = useMessage();
const POLL_MS = 3000;
const REMIND_MS = 30000;

/** 提示音是否已可出声（自动播放策略要求用户手势后才能响） */
const audioReady = computed(() => alertAudioState.value === "running");

/** 横幅只提示「提示音未启用」；系统通知授权状态在设置页查看，不做全局提醒 */
const bannerVisible = computed(() => !audioReady.value);

let pollTimer: number | undefined;
let remindTimer: number | undefined;
let unlisten: UnlistenFn | undefined;
let unlistenUpdates: UnlistenFn | undefined;
let refreshVersion = 0;
const pending = shallowRef(new Map<string, PendingApproval>());
let pendingCount = 0;

/** 用户点击「开启提示音」：显式 resume + 试听一声 */
async function enableAudio() {
  const ok = await ensureAudio();
  if (ok) {
    message.success("审批提示音已启用（刚才那声就是试听）");
  } else {
    message.error("启用失败：请检查系统音量与输出设备后重试");
  }
}

async function checkPerm() {
  await notifyPermission();
}

async function refresh() {
  const version = ++refreshVersion;
  try {
    const list = await api.approvalsPending();
    if (version !== refreshVersion) return;
    pending.value = new Map(list.map((item) => [item.id, item]));
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
  // 声音永远播放（三音 + 后续 30 秒重复）；已在审批中心时人正盯着列表，
  // 不再弹应用内通知卡遮挡视线（系统通知不受影响，后台场景仍有）。
  playApprovalChime(true);
  if (props.currentPage === "approvals") return;
  // 接住通知句柄：点击「去审批中心」时立即销毁，而不是等 10s duration 自然过期
  const renderNote = (note: ApprovalReview | null) => note
    ? h(ApprovalReviewNote, { note })
    : null;
  const n = notification.warning({
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
        renderNote(pending.value.get(item.id)?.aiNote ?? item.aiNote),
        h(
          "div",
          { style: "font-size:12px;opacity:.6" },
          "120 秒内未处理将自动拒绝"
        ),
        h(
          NButton,
          {
            size: "tiny",
            type: "primary",
            onClick: () => {
              n.destroy();
              emit("open");
            },
          },
          { default: () => "去审批中心" }
        ),
      ]),
  });
}

onMounted(async () => {
  window.addEventListener("pointerdown", primeAlertSound, { once: true, capture: true });
  window.addEventListener("keydown", primeAlertSound, { once: true, capture: true });
  window.addEventListener("focus", checkPerm);
  void checkPerm();

  await refresh();
  try {
    unlisten = await listen<PendingApproval>("approval-requested", (event) => {
      alertOnce(event.payload);
      void refresh();
    });
    unlistenUpdates = await listen("approvals-updated", refresh);
  } catch {
    /* 事件通道不可用（如纯浏览器预览）时仍靠轮询更新角标 */
  }
  pollTimer = window.setInterval(refresh, POLL_MS);
  remindTimer = window.setInterval(() => {
    if (pendingCount > 0) playApprovalChime(true);
  }, REMIND_MS);
});

onUnmounted(() => {
  window.removeEventListener("pointerdown", primeAlertSound, true);
  window.removeEventListener("keydown", primeAlertSound, true);
  window.removeEventListener("focus", checkPerm);
  if (pollTimer) window.clearInterval(pollTimer);
  if (remindTimer) window.clearInterval(remindTimer);
  unlisten?.();
  unlistenUpdates?.();
});
</script>

<template>
  <!-- 提示音未启用时的显式提示条：一键修复，修完自动消失 -->
  <div v-if="bannerVisible" class="alert-banner" role="alert">
    <div v-if="!audioReady" class="banner-row">
      <n-tag size="small" type="warning" :bordered="false">提示音未启用</n-tag>
      <span class="banner-text">审批到达时将没有声音（浏览器自动播放策略）</span>
      <n-button size="tiny" type="primary" @click="enableAudio">开启提示音并试听</n-button>
    </div>
  </div>
  <span style="display: none" />
</template>

<style scoped>
.alert-banner {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  z-index: 2000;
  background: rgba(99, 226, 183, 0.06);
  backdrop-filter: blur(6px);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  padding: 8px 16px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.banner-row {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12.5px;
}
.banner-text {
  opacity: 0.85;
  flex: 1;
}
</style>
