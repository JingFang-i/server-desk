<script setup lang="ts">
import { ref } from "vue";
import { NButton, NEmpty, NTag } from "naive-ui";
import { useActivityStore } from "../stores/activity";

/**
 * AI 实时活动小窗（全局）：右下角悬浮，展示哪个服务器正在被调用什么工具。
 * 事件流由 stores/activity 订阅（App.vue 启动时 init），本组件只负责渲染；
 * 收起状态记在 localStorage，换页不重置。
 */
const activity = useActivityStore();

const COLLAPSE_KEY = "serverdesk.activityCollapsed";
const showActivity = ref(localStorage.getItem(COLLAPSE_KEY) !== "1");

function toggle() {
  showActivity.value = !showActivity.value;
  localStorage.setItem(COLLAPSE_KEY, showActivity.value ? "0" : "1");
}
</script>

<template>
  <div class="activity-widget">
    <div class="activity-head" @click="toggle">
      <span class="activity-title">
        ⚡ AI 实时活动
        <n-tag v-if="activity.runningCount" size="small" type="info" :bordered="false">
          {{ activity.runningCount }} 个执行中
        </n-tag>
      </span>
      <n-button size="tiny" quaternary>{{ showActivity ? "收起" : "展开" }}</n-button>
    </div>
    <div v-if="showActivity" class="activity-list">
      <n-empty
        v-if="!activity.entries.length"
        size="small"
        description="暂无 AI 调用（AI 通过 MCP 操作服务器时会实时显示在这里）"
      />
      <div v-for="e in activity.entries.slice(0, 12)" :key="e.id" class="activity-row">
        <n-tag
          size="small"
          :bordered="false"
          :type="e.running ? 'info' : e.ok === true ? 'success' : e.ok === false ? 'error' : 'default'"
        >
          {{ e.running ? "执行中…" : e.ok ? "完成" : "失败/拦截" }}
        </n-tag>
        <span class="act-server">{{ e.server || "全局" }}</span>
        <span class="act-tool">{{ e.tool }}</span>
        <span class="act-time">{{ new Date(e.ts).toLocaleTimeString("zh-CN", { hour12: false }) }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.activity-widget {
  position: fixed;
  right: 16px;
  bottom: 16px;
  z-index: 100;
  width: 340px;
  max-height: 300px;
  display: flex;
  flex-direction: column;
  background: rgba(20, 20, 24, 0.92);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 10px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
  backdrop-filter: blur(8px);
  overflow: hidden;
}
.activity-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  cursor: pointer;
  user-select: none;
}
.activity-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  font-weight: 600;
}
.activity-list {
  overflow-y: auto;
  padding: 4px 12px 10px;
}
.activity-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 0;
  border-bottom: 1px dashed rgba(255, 255, 255, 0.06);
  font-size: 12px;
}
.activity-row:last-child {
  border-bottom: none;
}
.act-server {
  font-weight: 600;
  max-width: 90px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.act-tool {
  flex: 1;
  font-family: ui-monospace, Menlo, monospace;
  opacity: 0.8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.act-time {
  opacity: 0.45;
  flex-shrink: 0;
}
</style>
