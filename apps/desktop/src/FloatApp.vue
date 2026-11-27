<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "./api";
import pelicanCallUrl from "./assets/pelican-call.wav";
import { useActivityStore, type ActivityItem } from "./stores/activity";

/**
 * AI 实时活动悬浮窗（主窗口最小化/关闭都不影响它，托盘可开关）。
 *
 * 平时骑行，有待审批时停下来接电话。
 * - 车轮、牙盘、曲柄、脚踏板、鹈鹕的两只脚一起**顺时针**转（同一周期），
 *   脚在踏板上靠反向旋转保持水平 —— 这样才像在踩车，而不是脚跟着乱甩
 * - 车轮下方是「路面」：滚动显示活动，有待审批时固定显示待处理数量
 * - 点鹈鹕播放叫声；右键打开原生菜单，空白区域可拖动窗口
 * - 右上角红色数字角标 = 待审批数，点它直接进审批中心
 */
const activity = useActivityStore();
const historyOpen = ref(false);
const historyButton = ref<HTMLButtonElement | null>(null);
const backButton = ref<HTMLButtonElement | null>(null);
const pelicanAudio = ref<HTMLAudioElement | null>(null);
const vocalizing = ref(false);

async function toggleHistory() {
  historyOpen.value = !historyOpen.value;
  await nextTick();
  (historyOpen.value ? backButton.value : historyButton.value)?.focus();
}

const approvals = ref(0);
const answering = computed(() => approvals.value > 0);
const unlisteners: UnlistenFn[] = [];
let disposed = false;
let approvalVersion = 0;
let timer: number | undefined;
const APPROVAL_POLL_MS = 2000;

const latest = computed<ActivityItem | null>(() => activity.entries[0] ?? null);

/** 停车期间固定显示待处理数量，骑行期间滚动显示最新调用。 */
const marqueeText = computed(() => {
  if (answering.value) return `🔔 ${approvals.value} 条审批待处理`;
  if (latest.value) return `${latest.value.server || "全局"} · ${latest.value.tool}`;
  return "暂无 AI 调用";
});

async function refreshApprovals() {
  const version = ++approvalVersion;
  try {
    const pending = await api.approvalsPending();
    if (version === approvalVersion) approvals.value = pending.length;
  } catch {
    /* 主应用还没就绪 / 已退出：保持上一次的值 */
  }
}

function playPelicanCall() {
  const sound = pelicanAudio.value;
  if (!sound) return;
  sound.currentTime = 0;
  void sound.play().catch(() => { vocalizing.value = false; });
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

onMounted(() => {
  void activity.init().catch(() => {});
  void Promise.allSettled([
    listen("approvals-updated", refreshApprovals),
    listen<string>("float-menu", ({ payload }) => {
      switch (payload) {
        case "open-main":
          openApp();
          break;
        case "hide":
          hideFloat();
          break;
        case "history":
          if (!historyOpen.value) void toggleHistory();
          break;
      }
    }),
  ]).then((results) => {
    for (const result of results) {
      if (result.status !== "fulfilled") continue;
      if (disposed) result.value();
      else unlisteners.push(result.value);
    }
    if (!disposed) void refreshApprovals();
  });
  void refreshApprovals();
  timer = window.setInterval(refreshApprovals, APPROVAL_POLL_MS);
});

onUnmounted(() => {
  disposed = true;
  approvalVersion++;
  if (timer) window.clearInterval(timer);
  unlisteners.forEach((stop) => stop());
});
</script>

<template>
  <div class="float-root"
    :class="{ 'is-running': activity.runningCount > 0, 'is-answering': answering, 'is-vocalizing': vocalizing }"
    @contextmenu.prevent
  >
    <audio ref="pelicanAudio" :src="pelicanCallUrl" preload="auto" hidden
      @playing="vocalizing = true" @ended="vocalizing = false" @pause="vocalizing = false"
    />
    <section
      v-if="historyOpen"
      class="history"
      aria-label="近期 AI 活动"
      @keydown.esc.stop="toggleHistory"
      @contextmenu.prevent.stop="openMenu"
    >
      <header>
        <button ref="backButton" type="button" class="history-button" @click="toggleHistory">‹ 返回</button>
        <button v-if="approvals > 0" type="button" class="history-button" @click="openApprovals">
          {{ approvals }} 条待审批
        </button>
      </header>
      <div class="history-list">
        <p v-if="!activity.entries.length" class="history-empty">暂无 AI 调用</p>
        <details v-for="item in activity.entries" :key="item.id">
          <summary>
            <span class="history-name" :title="`${item.server || '全局'} · ${item.tool}`">
              {{ item.server || "全局" }} · {{ item.tool }}
            </span>
            <span class="history-state">{{ item.running ? "执行中" : item.ok ? "完成" : "失败／拦截" }}</span>
          </summary>
          <pre v-if="item.detail" tabindex="0">{{ item.detail }}</pre>
          <p v-else class="history-empty">无调用详情</p>
        </details>
      </div>
    </section>
    <div
      v-else
      class="rider"
      title="拖动可移动位置 · 右键菜单"
      data-tauri-drag-region
      @contextmenu.prevent.stop="openMenu"
    >
      <!-- 悬停时出现在车尾的速度线（纯 CSS，鼠标移出即淡出） -->
      <span class="streaks" aria-hidden="true">
        <i /><i /><i />
      </span>
      <div class="bike-stage">
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
              <path class="riding-leg" d="M90 63 L90 32" stroke="#cf8517" stroke-width="2.3" stroke-linecap="round" />
              <path class="riding-leg" d="M85.8 62 Q90 59 94.2 62 L94.2 63.4 L85.8 63.4 Z" fill="#cf8517" />
              <rect x="84.5" y="63.2" width="11" height="3.4" rx="1.4" fill="#8d97a8" />
            </g>
            <!-- 下踏板一侧（近侧腿，亮一点） -->
            <g class="pedal" style="transform-box: view-box; transform-origin: 90px 88px">
              <path class="riding-leg" d="M90 87 L90 56" stroke="#f5a623" stroke-width="2.5" stroke-linecap="round" />
              <path class="riding-leg" d="M85.6 86 Q90 83 94.4 86 L94.4 87.4 L85.6 87.4 Z" fill="#f5a623" />
              <rect x="84.5" y="87.2" width="11" height="3.4" rx="1.4" fill="#8d97a8" />
            </g>
          </g>

          <g v-if="answering" class="parked-leg">
            <path d="M84 50 Q82 64 75 77 L72 93" stroke="#f5a623" stroke-width="2.8" stroke-linecap="round" />
            <path d="M70 93 Q76 91 80 94 L80 96 H70 Z" fill="#f5a623" />
          </g>

          <!-- 鹈鹕（车身后画，盖住腿根；整体轻微上下浮动） -->
          <g class="rider-bob">
            <path d="M60 47 C60 33 78 26 92 31 C103 35 107 43 101 50 C95 56 73 56 64 52 Z" fill="#f4f5f9" />
            <path d="M89 34 C97 26 105 21 113 21 L115 30 C107 32 98 37 94 41 Z" fill="#f4f5f9" />
            <circle cx="115" cy="25" r="8" fill="#f4f5f9" />
            <path d="M121 22 L152 30 L121 32 Z" fill="#f5a623" />
            <g class="lower-bill">
              <path d="M121 31 Q137 44 149 31 Z" fill="#f8c15c" />
              <path d="M121 31 Q137 40 149 31" stroke="#e59a1b" stroke-width="1" fill="none" opacity=".7" />
            </g>
            <circle cx="117" cy="23.5" r="1.8" fill="#2a2f3a" />
            <g v-if="answering" class="phone-pose">
              <path d="M70 39 C83 34 94 42 101 33 L106 28 Q112 28 111 33 C108 45 94 52 80 49 Q73 48 69 44 Z"
                fill="#dde2ec" />
              <path d="M102 12 L107 14 Q109 15 108 17 L106 21 Q105 22 103 21 L101 20
                Q100 24 103 28 L105 26 Q107 25 108 27 L111 30 Q113 32 111 34 L107 36
                Q104 37 102 35 Q94 28 96 19 Q97 15 99 13 Q100 11 102 12 Z"
                fill="#73c7ff" stroke="#203449" stroke-width="1.25" stroke-linejoin="round" />
              <path d="M109 8 Q114 11 114 16 M113 5 Q120 9 120 15" stroke="#f5c279"
                stroke-width="1.5" stroke-linecap="round" />
            </g>
            <path v-else d="M70 39 C79 33 92 35 95 43 C87 49 75 48 69 44 Z" fill="#dde2ec" />
            <path v-if="vocalizing" d="M155 27 L161 24 M157 32 H164 M155 37 L161 40"
              stroke="#f5c279" stroke-width="1.5" stroke-linecap="round" />
          </g>
        </svg>
        <button type="button" class="pelican-call" aria-label="让鹈鹕叫一声"
          title="点击听鹈鹕叫声" @click.stop="playPelicanCall"
        />
      </div>

      <!-- 骑行时滚动，接电话时固定显示审批数量。 -->
      <div class="road">
        <span>{{ marqueeText }}</span>
      </div>

      <button
        v-if="answering"
        type="button"
        class="badge"
        :aria-label="`${approvals} 条待审批，打开审批中心`"
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

.history {
  min-height: 0;
  flex: 1;
  display: flex;
  flex-direction: column;
  background: #171c27;
  border: 1px solid #394357;
  border-radius: 8px;
  font-size: 11px;
}
.history header {
  display: flex;
  justify-content: space-between;
  padding: 3px 4px;
  border-bottom: 1px solid #394357;
}
.history-button {
  border: 0;
  border-radius: 4px;
  padding: 2px 5px;
  background: #222938;
  color: #f1f3f9;
  font: inherit;
  cursor: pointer;
}
.history-open {
  position: absolute;
  z-index: 1;
  top: 0;
  left: 0;
  font-size: 11px;
}
.history-list {
  overflow: auto;
  overscroll-behavior: contain;
}
.history-list summary {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px;
  border-bottom: 1px solid #293244;
  cursor: pointer;
}
.history-list summary::before { content: "▸"; }
.history-list summary::-webkit-details-marker { display: none; }
.history-list details[open] > summary::before { content: "▾"; }
.history-name {
  flex: 1;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.history-state { color: #b9c8de; }
.history-list pre {
  margin: 0;
  padding: 4px 6px;
  max-height: 54px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  user-select: text;
}
.history-empty { margin: 4px 6px; }

.rider {
  position: relative;
  width: 100%;
  flex: 1;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  /* 整块是窗口拖动区：光标给「可抓取」的暗示 */
  cursor: grab;
  /* 动画周期集中在这里：悬停/执行中只改变量，所有元素同步变速 */
  --cycle: 1.3s;
  --road: 9s;
  --bob: 0.9s;
}
/* 鼠标悬停 = 猛蹬 */
.float-root:not(.is-answering) .rider:hover {
  --cycle: 0.5s;
  --road: 3.6s;
  --bob: 0.36s;
}
/* AI 正在执行：本来就在加速 */
.is-running:not(.is-answering) .rider {
  --cycle: 0.65s;
  --road: 5s;
  --bob: 0.55s;
}
/* 执行中 + 悬停：最快 */
.is-running:not(.is-answering) .rider:hover {
  --cycle: 0.34s;
  --road: 2.8s;
  --bob: 0.28s;
}
/* 插画与路面可穿透鼠标事件，鹈鹕按钮与审批角标单独响应。 */
.bike-stage,
.rider .road,
.rider .streaks {
  pointer-events: none;
}

.bike-stage {
  position: relative;
  flex: none;
}

.pelican-call {
  position: absolute;
  top: 10%;
  left: 32%;
  width: 54%;
  height: 48%;
  border: 0;
  border-radius: 40%;
  background: transparent;
  color: inherit;
  cursor: pointer;
  pointer-events: auto;
}

.bike {
  width: 100%;
  height: auto;
  display: block;
  transition: filter 0.18s ease;
}
.float-root:not(.is-answering) .rider:hover .bike {
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

.phone-pose {
  animation: lift-phone 180ms ease-out;
}
@keyframes lift-phone {
  from { opacity: 0; transform: translateY(5px); }
}

.lower-bill {
  transform-box: view-box;
  transform-origin: 121px 31px;
  transition: transform 110ms ease-out;
}
.is-vocalizing .lower-bill {
  transform: rotate(12deg);
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
  pointer-events: auto;
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

.is-answering .wheel,
.is-answering .crank,
.is-answering .pedal,
.is-answering .rider-bob,
.is-answering .streaks i {
  animation-play-state: paused;
}

.is-answering .riding-leg,
.is-answering .streaks {
  visibility: hidden;
}

.is-answering .road span {
  width: 100%;
  padding: 0 4px;
  color: #f5c279;
  text-align: center;
  text-overflow: ellipsis;
  overflow: hidden;
  animation: none;
  transform: translateY(-50%);
}

@media (prefers-reduced-motion: reduce) {
  .wheel,
  .crank,
  .pedal,
  .rider-bob,
  .phone-pose,
  .road span,
  .streaks i {
    animation: none;
  }
  .road span {
    transform: translate(6px, -50%);
  }
  .lower-bill {
    transition: none;
  }
}
</style>
