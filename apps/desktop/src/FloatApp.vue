<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "./api";
import { useActivityStore, type ActivityItem } from "./stores/activity";

/**
 * AI 实时活动悬浮窗（主窗口最小化/关闭都不影响它，托盘可开关）。
 *
 * 只有一种形态：鹈鹕骑自行车。
 * - 车轮、牙盘、曲柄、脚踏板、鹈鹕的两只脚一起**顺时针**转（同一周期），
 *   脚在踏板上靠反向旋转保持水平 —— 这样才像在踩车，而不是脚跟着乱甩
 * - 车轮下方是「路面」：活动文字从右往左滚过（有审批时先滚「🔔 N 条审批待处理」）
 * - 左键 → 打开主界面；右键 → 原生菜单（打开主界面 / 隐藏整个悬浮窗）
 * - 右上角红色数字角标 = 待审批数，点它直接进审批中心
 */
const activity = useActivityStore();

const approvals = ref(0);
let unlisten: UnlistenFn | undefined;
let unlistenUpdated: UnlistenFn | undefined;
let timer: number | undefined;
const APPROVAL_POLL_MS = 2000;

const latest = computed<ActivityItem | null>(() => activity.entries[0] ?? null);

/** 路面（跑马灯）的内容：有审批先报审批，否则报最新一次调用 */
const marqueeText = computed(() => {
  const parts: string[] = [];
  if (approvals.value > 0) parts.push(`🔔 ${approvals.value} 条审批待处理`);
  if (latest.value) {
    parts.push(`${latest.value.server || "全局"} · ${latest.value.tool}`);
  }
  return parts.length ? parts.join("　·　") : "暂无 AI 调用";
});

async function refreshApprovals() {
  try {
    approvals.value = (await api.approvalsPending()).length;
  } catch {
    /* 主应用还没就绪 / 已退出：保持上一次的值 */
  }
}

function openApp() {
  void api.showMainWindow().catch(() => {});
}

/** 审批角标：点它直接落到主界面的审批中心 */
function openApprovals() {
  void api.showMainWindow("approvals").catch(() => {});
}

/** 隐藏整个悬浮窗（持久化）：托盘或设置页开关可再打开 */
function hideFloat() {
  void api.setFloatEnabled(false).catch(() => {});
}

function openMenu() {
  void api.floatContextMenu().catch(() => {});
}

onMounted(async () => {
  void activity.init().catch(() => {});
  await refreshApprovals();
  try {
    unlisten = await listen("approvals-updated", refreshApprovals);
    unlistenUpdated = await listen("float-menu", (event) => {
      switch (event.payload as string) {
        case "open-main":
          openApp();
          break;
        case "hide":
          hideFloat();
          break;
      }
    });
  } catch {
    /* 事件通道不可用：靠轮询兜底 */
  }
  timer = window.setInterval(refreshApprovals, APPROVAL_POLL_MS);
});

onUnmounted(() => {
  if (timer) window.clearInterval(timer);
  unlisten?.();
  unlistenUpdated?.();
});
</script>

<template>
  <div class="float-root" :class="{ 'is-running': activity.runningCount > 0 }" @contextmenu.prevent>
    <div
      class="rider"
      title="左键打开主界面 · 右键菜单"
      @click="openApp"
      @contextmenu.prevent.stop="openMenu"
    >
      <!-- 悬停时出现在车尾的速度线（纯 CSS，鼠标移出即淡出） -->
      <span class="streaks" aria-hidden="true">
        <i /><i /><i />
      </span>
      <svg class="bike" viewBox="0 0 180 98" fill="none" aria-hidden="true">
        <!-- 后轮 -->
        <g class="wheel" style="transform-box: view-box; transform-origin: 46px 76px">
          <circle cx="46" cy="76" r="20" stroke="#e8e8f0" stroke-width="2.6" />
          <circle cx="46" cy="76" r="2.6" fill="#e8e8f0" />
          <g stroke="#e8e8f0" stroke-width="1.3" opacity=".45">
            <line x1="46" y1="57" x2="46" y2="95" />
            <line x1="27" y1="76" x2="65" y2="76" />
            <line x1="33" y1="63" x2="59" y2="89" />
            <line x1="33" y1="89" x2="59" y2="63" />
          </g>
        </g>
        <!-- 前轮 -->
        <g class="wheel" style="transform-box: view-box; transform-origin: 134px 76px">
          <circle cx="134" cy="76" r="20" stroke="#e8e8f0" stroke-width="2.6" />
          <circle cx="134" cy="76" r="2.6" fill="#e8e8f0" />
          <g stroke="#e8e8f0" stroke-width="1.3" opacity=".45">
            <line x1="134" y1="57" x2="134" y2="95" />
            <line x1="115" y1="76" x2="153" y2="76" />
            <line x1="121" y1="63" x2="147" y2="89" />
            <line x1="121" y1="89" x2="147" y2="63" />
          </g>
        </g>
        <!-- 车架 -->
        <g stroke="#aab6c9" stroke-width="3" stroke-linecap="round">
          <line x1="46" y1="76" x2="90" y2="76" />
          <line x1="46" y1="76" x2="80" y2="54" />
          <line x1="80" y1="54" x2="90" y2="76" />
          <line x1="80" y1="54" x2="118" y2="52" />
          <line x1="90" y1="76" x2="118" y2="52" />
          <line x1="118" y1="52" x2="134" y2="76" />
          <line x1="118" y1="52" x2="127" y2="45" />
        </g>
        <line x1="73" y1="51" x2="87" y2="51" stroke="#8d97a8" stroke-width="4" stroke-linecap="round" />
        <circle cx="127" cy="45" r="3.2" fill="#8d97a8" />

        <!-- 牙盘 + 曲柄 + 脚踏板 + 鹈鹕的脚（整体顺时针，每圈 1.3s） -->
        <g class="crank" style="transform-box: view-box; transform-origin: 90px 76px">
          <circle cx="90" cy="76" r="7" stroke="#8d97a8" stroke-width="2.2" fill="none" />
          <line x1="90" y1="76" x2="90" y2="64" stroke="#8d97a8" stroke-width="2.6" stroke-linecap="round" />
          <line x1="90" y1="76" x2="90" y2="88" stroke="#8d97a8" stroke-width="2.6" stroke-linecap="round" />
          <!-- 上踏板一侧（远期腿，暗一点） -->
          <g class="pedal" style="transform-box: view-box; transform-origin: 90px 64px">
            <path d="M90 63 L90 32" stroke="#cf8517" stroke-width="2.3" stroke-linecap="round" />
            <path d="M85.8 62 Q90 59 94.2 62 L94.2 63.4 L85.8 63.4 Z" fill="#cf8517" />
            <rect x="84.5" y="63.2" width="11" height="3.4" rx="1.4" fill="#8d97a8" />
          </g>
          <!-- 下踏板一侧（近侧腿，亮一点） -->
          <g class="pedal" style="transform-box: view-box; transform-origin: 90px 88px">
            <path d="M90 87 L90 56" stroke="#f5a623" stroke-width="2.5" stroke-linecap="round" />
            <path d="M85.6 86 Q90 83 94.4 86 L94.4 87.4 L85.6 87.4 Z" fill="#f5a623" />
            <rect x="84.5" y="87.2" width="11" height="3.4" rx="1.4" fill="#8d97a8" />
          </g>
        </g>

        <!-- 鹈鹕（车身后画，盖住腿根；整体轻微上下浮动） -->
        <g class="rider-bob">
          <path d="M60 47 C60 33 78 26 92 31 C103 35 107 43 101 50 C95 56 73 56 64 52 Z" fill="#f4f5f9" />
          <path d="M89 34 C97 26 105 21 113 21 L115 30 C107 32 98 37 94 41 Z" fill="#f4f5f9" />
          <circle cx="115" cy="25" r="8" fill="#f4f5f9" />
          <path d="M121 22 L152 30 L121 32 Z" fill="#f5a623" />
          <path d="M121 31 Q137 44 149 31 Z" fill="#f8c15c" />
          <path d="M121 31 Q137 40 149 31" stroke="#e59a1b" stroke-width="1" fill="none" opacity=".7" />
          <circle cx="117" cy="23.5" r="1.8" fill="#2a2f3a" />
          <path d="M70 39 C79 33 92 35 95 43 C87 49 75 48 69 44 Z" fill="#dde2ec" />
        </g>
      </svg>

      <!-- 路面：活动文字从右往左滚 -->
      <div class="road">
        <span>{{ marqueeText }}</span>
      </div>

      <button
        v-if="approvals > 0"
        class="badge"
        title="有 AI 审批待处理，点击进入审批中心"
        @click.stop="openApprovals"
      >
        {{ approvals }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.float-root {
  height: 100%;
  padding: 4px 4px 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.rider {
  position: relative;
  width: 100%;
  flex: 1;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  cursor: pointer;
  /* 动画周期集中在这里：悬停/执行中只改变量，所有元素同步变速 */
  --cycle: 1.3s;
  --road: 9s;
  --bob: 0.9s;
}
/* 鼠标悬停 = 猛蹬 */
.rider:hover {
  --cycle: 0.5s;
  --road: 3.6s;
  --bob: 0.36s;
}
/* AI 正在执行：本来就在加速 */
.is-running .rider {
  --cycle: 0.65s;
  --road: 5s;
  --bob: 0.55s;
}
/* 执行中 + 悬停：最快 */
.is-running .rider:hover {
  --cycle: 0.34s;
  --road: 2.8s;
  --bob: 0.28s;
}
.bike {
  width: 100%;
  height: auto;
  display: block;
  transition: filter 0.18s ease;
}
.rider:hover .bike {
  filter: drop-shadow(0 0 7px rgba(120, 195, 255, 0.45));
}

/* 车尾速度线：只有悬停时出现，往左飞出 */
.streaks {
  position: absolute;
  left: 2px;
  top: 34px;
  width: 64px;
  height: 34px;
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.16s ease;
}
.rider:hover .streaks {
  opacity: 1;
}
.streaks i {
  position: absolute;
  height: 2px;
  border-radius: 2px;
  background: linear-gradient(90deg, rgba(140, 200, 255, 0), rgba(170, 220, 255, 0.95));
  animation: streak 0.5s linear infinite;
}
.rider:hover .streaks i {
  animation-duration: calc(var(--cycle) * 0.42);
}
.streaks i:nth-child(1) {
  top: 3px;
  width: 30px;
}
.streaks i:nth-child(2) {
  top: 14px;
  width: 50px;
  animation-delay: 0.12s;
}
.streaks i:nth-child(3) {
  top: 25px;
  width: 24px;
  animation-delay: 0.24s;
}
@keyframes streak {
  0% {
    transform: translateX(48px);
    opacity: 0;
  }
  25% {
    opacity: 0.95;
  }
  100% {
    transform: translateX(-26px);
    opacity: 0;
  }
}

/* 车轮与曲柄同一周期；有调用在执行时踩得更快 */
.wheel,
.crank,
.pedal {
  animation-duration: var(--cycle);
  animation-timing-function: linear;
  animation-iteration-count: infinite;
}
.wheel,
.crank {
  animation-name: spin;
}
/* 踏板反向转同样角度 → 脚始终水平，只是跟着曲柄绕圈 */
.pedal {
  animation-name: spin;
  animation-direction: reverse;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

/* 整体轻微上下起伏，像在发力 */
.rider-bob {
  animation: rider-bob var(--bob) ease-in-out infinite alternate;
}
@keyframes rider-bob {
  from {
    transform: translateY(0);
  }
  to {
    transform: translateY(-1.2px);
  }
}

/* 路面：活动文字从右往左滚，正好从车轮下方经过 */
.road {
  position: relative;
  height: 20px;
  overflow: hidden;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  flex-shrink: 0;
}
.road span {
  position: absolute;
  top: 50%;
  left: 0;
  white-space: nowrap;
  font-size: 11px;
  font-weight: 600;
  color: #dfe3ec;
  opacity: 0.9;
  animation: road-run var(--road) linear infinite;
}
@keyframes road-run {
  0% {
    transform: translate(100vw, -50%);
  }
  100% {
    transform: translate(-100%, -50%);
  }
}

.badge {
  position: absolute;
  top: 0;
  right: 0;
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border: none;
  border-radius: 9px;
  background: #ff3b64;
  color: #fff;
  font-size: 11px;
  font-weight: 700;
  line-height: 18px;
  text-align: center;
  cursor: pointer;
  box-shadow: 0 0 10px rgba(255, 59, 100, 0.7);
}

@media (prefers-reduced-motion: reduce) {
  .wheel,
  .crank,
  .pedal,
  .rider-bob,
  .road span,
  .streaks i {
    animation: none;
  }
  .road span {
    transform: translate(6px, -50%);
  }
}
</style>
