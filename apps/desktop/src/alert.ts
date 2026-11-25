// 审批到达提示音：用 Web Audio 实时合成，不依赖任何音频资源文件。
// 浏览器自动播放策略要求「先有用户交互」才允许出声：首次指针/键盘操作时解锁音频上下文。

let ctx: AudioContext | null = null;
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
    ctx = new Ctor();
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

/** 当前音频上下文状态：审批提示音能否立即出声。 */
export function audioState(): AudioState {
  const c = audioContext();
  if (!c) return "unavailable";
  return c.state === "running" ? "running" : "suspended";
}

/**
 * 显式启用提示音（用户点击后调用）：恢复上下文并播放一声确认音，
 * 返回是否已可出声。自动播放策略要求这一步必须在用户手势的调用栈里。
 */
export async function ensureAudio(): Promise<boolean> {
  const c = audioContext();
  if (!c) return false;
  if (c.state === "suspended") {
    try {
      await c.resume();
    } catch {
      return false;
    }
  }
  if (c.state !== "running") return false;
  playApprovalChime(true);
  return true;
}

/** 播放三声短促提示音（上行两音 + 回落），force 用于到点提醒。 */
export function playApprovalChime(force = false) {
  const now = Date.now();
  if (!force && now - lastPlayedAt < MIN_INTERVAL_MS) return;
  lastPlayedAt = now;

  const c = audioContext();
  if (!c) return;
  if (c.state === "suspended") void c.resume().catch(() => {});

  const start = c.currentTime;
  const notes = [
    { offset: 0, freq: 1174.66 },
    { offset: 0.18, freq: 1567.98 },
    { offset: 0.36, freq: 1174.66 },
  ];
  for (const { offset, freq } of notes) {
    const osc = c.createOscillator();
    const gain = c.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.0001, start + offset);
    gain.gain.exponentialRampToValueAtTime(0.22, start + offset + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, start + offset + 0.16);
    osc.connect(gain);
    gain.connect(c.destination);
    osc.start(start + offset);
    osc.stop(start + offset + 0.18);
  }
}
