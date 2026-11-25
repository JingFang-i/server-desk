// AI 实时活动流（R8 后置增强）：服务器页小窗的数据源。
// 事件来自主应用：工具调用开始（ipc 分发处）与完成（审计写入处）各发一次
// `tool-activity`，按 session|tool|server 关联起止，形成「正在执行 → 已完成」。

import { defineStore } from "pinia";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ActivityItem {
  /** session|tool|server：起止事件按此关联 */
  key: string;
  ts: string;
  tool: string;
  /** 开始事件取 args.server/group（原始引用）；完成事件用审计解析出的名称覆盖 */
  server: string;
  /** start → running；done 后为 false */
  running: boolean;
  outcome: string;
  /** null = 尚未完成 */
  ok: boolean | null;
}

const MAX_ENTRIES = 50;

let unlisten: UnlistenFn | null = null;
let seq = 0;

export const useActivityStore = defineStore("activity", {
  state: () => ({
    entries: [] as ActivityItem[],
  }),
  getters: {
    runningCount: (s) => s.entries.filter((e) => e.running).length,
  },
  actions: {
    /** 全局只订阅一次（幂等）；在服务器页挂载时调用。 */
    async init() {
      if (unlisten) return;
      unlisten = await listen<Record<string, unknown>>("tool-activity", (ev) => {
        const d = ev.payload as Record<string, unknown>;
        const phase = String(d.phase ?? "");
        const session = String(d.session ?? "");
        const tool = String(d.tool ?? "");
        const serverRaw = String(d.server ?? "");
        const key = `${session}|${tool}|${serverRaw}`;

        if (phase === "start") {
          seq += 1;
          this.entries.unshift({
            key,
            ts: new Date().toISOString(),
            tool,
            server: serverRaw,
            running: true,
            outcome: "",
            ok: null,
          });
          if (this.entries.length > MAX_ENTRIES) this.entries.length = MAX_ENTRIES;
          return;
        }

        // done：更新匹配的 running 项；找不到（如来源非 IPC 或刷新丢失）则直接插入
        const outcome = String(d.outcome ?? "");
        const ok = outcome.startsWith("ok");
        const serverName = String(d.server_name ?? "");
        const ts = String(d.ts ?? new Date().toISOString());
        const idx = this.entries.findIndex((e) => e.running && e.key === key);
        if (idx >= 0) {
          const e = this.entries[idx];
          e.running = false;
          e.outcome = outcome;
          e.ok = ok;
          e.ts = ts;
          if (serverName) e.server = serverName;
        } else {
          seq += 1;
          this.entries.unshift({
            key,
            ts,
            tool,
            server: serverName,
            running: false,
            outcome,
            ok,
          });
          if (this.entries.length > MAX_ENTRIES) this.entries.length = MAX_ENTRIES;
        }
        void seq; // 预留：排序键
      });
    },
    clear() {
      this.entries = [];
    },
  },
});

/** 工具名展示：剥掉 mcp_ 前缀。 */
export function prettyTool(action: string): string {
  return action.replace(/^mcp_/, "");
}

/** 审计 outcome → 结果徽标类型。 */
export function outcomeKind(outcome: string): "ok" | "bad" | "warn" | "idle" {
  if (outcome.startsWith("ok")) return "ok";
  if (outcome.startsWith("approval") || outcome.startsWith("ai_review")) return "warn";
  if (outcome === "" ) return "idle";
  return "bad";
}
