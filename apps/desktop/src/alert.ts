import { readonly, shallowRef } from "vue";

// 审批到达提示音：用 Web Audio 实时合成，不依赖任何音频资源文件。
// 浏览器自动播放策略要求「先有用户交互」才允许出声：首次指针/键盘操作时解锁音频上下文。
// 提示音风格（R8 后置增强）：多种合成音色可选，偏好存 localStorage，选择即试听。

let ctx: AudioContext | null = null;
const audioState = shallowRef<AudioState>("suspended");
export const alertAudioState = readonly(audioState);
let lastPlayedAt = 0;
/** 一次审批可能同时触发事件与轮询，1.2 秒内合并，避免叠音。 */
const MIN_INTERVAL_MS = 1200;

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
      audioState.value = context.state === "running" ? "running" : "suspended";
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
  { value: "dingdong", label: "经典叮咚（默认）" },
  { value: "coin", label: "街机金币" },
  { value: "scifi", label: "科幻通讯" },
  { value: "bubble", label: "水下气泡" },
  { value: "tada", label: "胜利号角" },
];

const CHIME_STORAGE_KEY = "serverdesk.alertChime";

/** 当前选择的提示音风格；非法/未设置回退默认。 */
export function currentChime(): string {
  const v = localStorage.getItem(CHIME_STORAGE_KEY);
  return CHIME_OPTIONS.some((o) => o.value === v) ? (v as string) : "dingdong";
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
  if (c.state === "suspended") void c.resume().catch(() => {});

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
