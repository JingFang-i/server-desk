<script setup lang="ts">
import { computed, h, onMounted, onUnmounted, ref } from "vue";
import { NButton, NTag, useMessage, useNotification } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type PendingApproval } from "../api";
import { audioState, ensureAudio, playApprovalChime, primeAlertSound } from "../alert";
import { notifyPermission, requestNotifyPermission, type NotifyPermission } from "../notifyStatus";

/**
 * 全局审批提醒：新审批到达 → 提示音 + 应用内通知 + 菜单角标；
 * 待审批未处理期间每 30 秒再响一次（审批 120 秒后自动拒绝，避免静默过期）。
 * 系统级通知与程序坞/任务栏提示由 Rust 侧（notify.rs）发送，两者互为补充。
 * 顶部横幅（R8 后置增强）：提示音未启用 / 系统通知未授权时显式提示，一键修复——
 * 「提示音一定会响」的两个前提（音频上下文已解锁、系统通知已授权）不再靠运气。
 */
const emit = defineEmits<{ (e: "count", n: number): void; (e: "open"): void; (e: "settings"): void }>();

const notification = useNotification();
const message = useMessage();
const POLL_MS = 3000;
const REMIND_MS = 30000;

/** 提示音是否已可出声（自动播放策略要求用户手势后才能响） */
const audioReady = ref(audioState() === "running");
/** 系统通知授权状态：granted / denied / unknown */
const notifyPerm = ref<NotifyPermission>("unknown");

/** 横幅是否显示：提示音未就绪，或系统通知明确被拒（unknown 不算缺失，避免误报） */
const bannerVisible = computed(
  () => !audioReady.value || notifyPerm.value === "denied"
);

let pollTimer: number | undefined;
let remindTimer: number | undefined;
let unlisten: UnlistenFn | undefined;
let pendingCount = 0;

/** 手势后复查：被动解锁成功即收起横幅 */
function checkAudio() {
  audioReady.value = audioState() === "running";
}

/** 用户点击「开启提示音」：显式 resume + 试听一声 */
async function enableAudio() {
  const ok = await ensureAudio();
  audioReady.value = ok;
  if (ok) {
    message.success("审批提示音已启用（刚才那声就是试听）");
  } else {
    message.error("启用失败：请检查系统音量与输出设备后重试");
  }
}

async function checkPerm() {
  notifyPerm.value = await notifyPermission();
}

async function requestPerm() {
  const r = await requestNotifyPermission();
  notifyPerm.value = r;
  if (r === "granted") {
    message.success("系统通知已授权：应用在后台也会弹出审批提醒并带提示音");
  } else if (r === "denied") {
    message.warning(
      "授权被拒绝：请在系统设置 → 通知 → ServerDesk 中手动允许，否则应用在后台时收不到审批提醒"
    );
  }
}

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
        ...(item.aiNote
          ? [
              h(
                "div",
                { style: "font-size:12px;line-height:1.6;color:#e88080;word-break:break-all" },
                `AI 意见：${item.aiNote}`
              ),
            ]
          : []),
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
  checkAudio();
}
function onKeyDown() {
  primeAlertSound();
  checkAudio();
}

onMounted(async () => {
  // 首次用户交互时解锁音频（自动播放策略），并复查横幅状态
  window.addEventListener("pointerdown", onPointerDown, { once: true, capture: true });
  window.addEventListener("keydown", onKeyDown, { once: true, capture: true });
  checkPerm();

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
  <!-- 提示音 / 系统通知未就绪时的显式提示条：一键修复，修完自动消失 -->
  <div v-if="bannerVisible" class="alert-banner" role="alert">
    <div v-if="!audioReady" class="banner-row">
      <n-tag size="small" type="warning" :bordered="false">提示音未启用</n-tag>
      <span class="banner-text">审批到达时将没有声音（浏览器自动播放策略）</span>
      <n-button size="tiny" type="primary" @click="enableAudio">开启提示音并试听</n-button>
    </div>
    <div v-if="notifyPerm === 'denied'" class="banner-row">
      <n-tag size="small" type="error" :bordered="false">系统通知未授权</n-tag>
      <span class="banner-text">应用在后台时收不到审批提醒与提示音</span>
      <n-button size="tiny" type="error" ghost @click="requestPerm">申请授权</n-button>
      <n-button size="tiny" quaternary @click="emit('settings')">去设置</n-button>
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
