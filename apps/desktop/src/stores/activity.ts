// IPC 以请求 ID 关联活动的开始和结束，审批和审计走各自的事件流。
import { defineStore } from "pinia";
import { listen } from "@tauri-apps/api/event";

export interface ActivityItem {
  id: string;
  ts: string;
  tool: string;
  server: string;
  /** 这条调用做了什么（脱敏后的命令/路径，点开记录时展示） */
  detail: string;
  running: boolean;
  ok: boolean | null;
}

interface ActivityEvent {
  id: string;
  phase: "start" | "done";
  tool: string;
  server: string;
  /** 已脱敏的调用内容；done 事件也会带（与 start 相同） */
  detail?: string;
  ok?: boolean;
}

const MAX_ENTRIES = 50;
let subscribed = false;

export const useActivityStore = defineStore("activity", {
  state: () => ({
    entries: [] as ActivityItem[],
  }),
  getters: {
    runningCount: (s) => s.entries.filter((e) => e.running).length,
  },
  actions: {
    async init(isVisible: () => boolean = () => true) {
      if (subscribed) return;
      subscribed = true;
      try {
        await listen<ActivityEvent>("tool-activity", ({ payload }) => {
          if (!isVisible()) return;
          const { id, phase, tool, server, detail, ok } = payload;
          const ts = new Date().toISOString();
          const item = this.entries.find((entry) => entry.id === id);
          if (item) {
            if (phase === "done") {
              item.running = false;
              item.ok = ok === true;
              item.ts = ts;
            }
            return;
          }
          this.entries.unshift({
            id, ts, tool, server,
            detail: detail ?? "",
            running: phase === "start",
            ok: phase === "start" ? null : ok === true,
          });
          if (this.entries.length > MAX_ENTRIES) this.entries.length = MAX_ENTRIES;
        });
      } catch (error) {
        subscribed = false;
        throw error;
      }
    },
    clear() {
      this.entries = [];
    },
  },
});
