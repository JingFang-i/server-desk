import { readonly, shallowRef } from "vue";
import { invoke } from "@tauri-apps/api/core";

// 审批到达提示音：用 Web Audio 实时合成，不依赖任何音频资源文件。
// 浏览器自动播放策略要求「先有用户交互」才允许出声：首次指针/键盘操作时解锁音频上下文。
// 提示音风格（R8 后置增强）：多种合成音色可选，偏好存 localStorage，选择即试听。

let ctx: AudioContext | null = null;
const audioState = shallowRef<AudioState>("suspended");
export const alertAudioState = readonly(audioState);
let lastPlayedAt = 0;
/** 一次审批可能同时触发事件与轮询，1.2 秒内合并，避免叠音。 */
const MIN_INTERVAL_MS = 1200;
let reportedReady: boolean | null = null;

/**
 * 把「应用内提示音是否真的能出声」上报给桌面端。
 * Rust 侧据此决定审批到达时要不要用原生系统音兜底：能出声就只响一次，不能就补一声。
 */
function reportAudioReady(ready: boolean) {
  if (reportedReady === ready) return;
  reportedReady = ready;
  void invoke("alert_audio_ready", { ready }).catch(() => {
    /* 纯浏览器预览没有 Tauri IPC：忽略 */
  });
}

function audioContext(): AudioContext | null {
  if (ctx) return ctx;
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) return null;
  try {
    const context = new Ctor();
    ctx = context;
    const syncState = () => {
      const running = context.state === "running";
      audioState.value = running ? "running" : "suspended";
      reportAudioReady(running);
    };
    context.addEventListener("statechange", syncState);
    syncState();
  } catch {
    return null;
  }
  return ctx;
}

/** 首次用户交互时创建/恢复音频上下文（避免自动播放限制导致静音）。 */
export function primeAlertSound() {
  const c = audioContext();
  if (c && c.state === "suspended") void c.resume().catch(() => {});
}

/** 是否已 armed（幂等，多个组件同时调用只挂一套监听） */
let armed = false;

/**
 * 持续监听用户手势，直到音频上下文真正进入 running。
 *
 * 之前用 `{ once: true }` 只尝试解锁一次：一旦那次解锁失败（页面还在加载、
 * WebView 判定手势不合格、系统输出设备刚切换），之后所有审批都永远静音，
 * 而且界面上没有任何补救入口。现在改为「一直重试到成功」。
 */
export function armAlertSound() {
  if (armed) return;
  armed = true;
  const stop = () => {
    window.removeEventListener("pointerdown", attempt, true);
    window.removeEventListener("keydown", attempt, true);
    window.removeEventListener("focus", attempt);
  };
  const attempt = () => {
    primeAlertSound();
    if (audioState.value === "running") stop();
  };
  window.addEventListener("pointerdown", attempt, { capture: true });
  window.addEventListener("keydown", attempt, { capture: true });
  window.addEventListener("focus", attempt);
}

export type AudioState = "running" | "suspended" | "unavailable";

/**
 * 显式启用提示音（用户点击后调用）：恢复上下文并播放一声确认音，
 * 返回是否已可出声。自动播放策略要求这一步必须在用户手势的调用栈里。
 */
export async function ensureAudio(): Promise<boolean> {
  const c = audioContext();
  if (!c) {
    audioState.value = "unavailable";
    return false;
  }
  if (c.state === "suspended") {
    try {
      await c.resume();
    } catch {
      return false;
    }
  }
  if (c.state !== "running") return false;
  audioState.value = "running";
  playApprovalChime(true);
  return true;
}

// ---------- 提示音风格 ----------

/** 单个音符：波形、频率（可滑频）、起始偏移、时长、峰值音量。 */
interface Tone {
  type: OscillatorType;
  freq: number;
  freqTo?: number;
  offset: number;
  dur: number;
  gain: number;
}

export interface ChimeOption {
  value: string;
  label: string;
}

/** 可选提示音风格（全部纯合成，无资源文件；选择存 localStorage）。 */
export const CHIME_OPTIONS: ChimeOption[] = [
  { value: "bell", label: "自行车铃（默认）" },
  { value: "dingdong", label: "经典叮咚" },
  { value: "coin", label: "街机金币" },
  { value: "scifi", label: "科幻通讯" },
  { value: "bubble", label: "水下气泡" },
  { value: "tada", label: "胜利号角" },
];

/** 默认音色：自行车铃 */
export const DEFAULT_CHIME = "bell";

/** 存储键带了版本后缀：默认音色换成车铃后，旧版记下的选择（如 coin）不再覆盖默认，
 *  老用户也能听到新默认音；之后在设置里再选会写到新键上。 */
const CHIME_STORAGE_KEY = "serverdesk.alertChime.v2";

/** 当前选择的提示音风格；非法/未设置回退默认。 */
export function currentChime(): string {
  const v = localStorage.getItem(CHIME_STORAGE_KEY);
  return CHIME_OPTIONS.some((o) => o.value === v) ? (v as string) : DEFAULT_CHIME;
}

/** 保存提示音风格（立即生效，下次审批即用新音色）。 */
export function setChime(value: string) {
  if (CHIME_OPTIONS.some((o) => o.value === value)) {
    localStorage.setItem(CHIME_STORAGE_KEY, value);
  }
}

/** 各风格的音符编排（时长都控制在 ~1s 内，30 秒重复提醒也不烦人）。 */
function chimeTones(name: string): Tone[] {
  switch (name) {
    case "coin":
      // 街机金币：短促 B5 跳到明亮 E6 长音（复古游戏「吃金币」）
      return [
        { type: "square", freq: 987.77, offset: 0, dur: 0.09, gain: 0.16 },
        { type: "square", freq: 1318.51, offset: 0.09, dur: 0.4, gain: 0.16 },
      ];
    case "scifi":
      // 科幻通讯：两声锯齿波滑音上行，像星际舰桥的电传
      return [
        { type: "sawtooth", freq: 320, freqTo: 1250, offset: 0, dur: 0.3, gain: 0.14 },
        { type: "sawtooth", freq: 420, freqTo: 1680, offset: 0.28, dur: 0.34, gain: 0.14 },
      ];
    case "bubble":
      // 水下气泡：三个快速上滑的正弦短泡，咕噜咕噜
      return [
        { type: "sine", freq: 580, freqTo: 940, offset: 0, dur: 0.1, gain: 0.22 },
        { type: "sine", freq: 700, freqTo: 1120, offset: 0.13, dur: 0.1, gain: 0.22 },
        { type: "sine", freq: 520, freqTo: 860, offset: 0.27, dur: 0.12, gain: 0.22 },
      ];
    case "tada":
      // 胜利号角：C6/E6 和弦铺底 + 滑向 G6 的亮音
      return [
        { type: "triangle", freq: 1046.5, offset: 0, dur: 0.45, gain: 0.14 },
        { type: "triangle", freq: 1318.5, offset: 0, dur: 0.45, gain: 0.12 },
        { type: "sine", freq: 1567.98, freqTo: 2093, offset: 0.12, dur: 0.4, gain: 0.16 },
      ];
    case "bell":
      // 自行车铃：两声清脆的「叮叮」。基频约 B6，叠 2×/3× 泛音模拟金属声，
      // 泛音衰减更快 → 主音余韵长一点，听起来像车铃而不是电子音。
      return [
        { type: "sine", freq: 1975.53, offset: 0, dur: 0.42, gain: 0.2 },
        { type: "sine", freq: 3951.07, offset: 0, dur: 0.28, gain: 0.1 },
        { type: "sine", freq: 5926.6, offset: 0, dur: 0.18, gain: 0.05 },
        { type: "sine", freq: 1975.53, offset: 0.23, dur: 0.52, gain: 0.22 },
        { type: "sine", freq: 3951.07, offset: 0.23, dur: 0.34, gain: 0.11 },
        { type: "sine", freq: 5926.6, offset: 0.23, dur: 0.2, gain: 0.055 },
      ];
    case "dingdong":
    default:
      // 经典叮咚：上行两音 + 回落（D6 → G6 → D6）
      return [
        { type: "sine", freq: 1174.66, offset: 0, dur: 0.18, gain: 0.22 },
        { type: "sine", freq: 1567.98, offset: 0.18, dur: 0.18, gain: 0.22 },
        { type: "sine", freq: 1174.66, offset: 0.36, dur: 0.18, gain: 0.22 },
      ];
  }
}

/** 播放三声短促提示音（风格可选，force 用于到点提醒与试听）。 */
export function playApprovalChime(force = false) {
  const now = Date.now();
  if (!force && now - lastPlayedAt < MIN_INTERVAL_MS) return;
  lastPlayedAt = now;

  const c = audioContext();
  if (!c) return;
  // 上下文还没解锁时不能直接排音：先 resume，确认 running 后再排，
  // 否则振荡器被排进一个永远不前进的时间轴，表现就是「没声音」。
  if (c.state !== "running") {
    void c
      .resume()
      .then(() => {
        if (c.state === "running") {
          audioState.value = "running";
          reportAudioReady(true);
          scheduleChime(c);
        }
      })
      .catch(() => {
        /* 解锁失败：Rust 侧会用原生系统音兜底 */
      });
    return;
  }
  scheduleChime(c);
}

/** 在已解锁的上下文上排一段提示音。 */
function scheduleChime(c: AudioContext) {
  const start = c.currentTime;
  for (const t of chimeTones(currentChime())) {
    const osc = c.createOscillator();
    const gain = c.createGain();
    osc.type = t.type;
    osc.frequency.setValueAtTime(t.freq, start + t.offset);
    if (t.freqTo !== undefined) {
      osc.frequency.exponentialRampToValueAtTime(t.freqTo, start + t.offset + t.dur);
    }
    gain.gain.setValueAtTime(0.0001, start + t.offset);
    gain.gain.exponentialRampToValueAtTime(t.gain, start + t.offset + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, start + t.offset + t.dur);
    osc.connect(gain);
    gain.connect(c.destination);
    osc.start(start + t.offset);
    osc.stop(start + t.offset + t.dur + 0.02);
  }
}
