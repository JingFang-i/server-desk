<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { NButton, NCard, NEmpty, NTag, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type PendingApproval } from "../api";
import ApprovalReviewNote from "../components/ApprovalReviewNote.vue";

const message = useMessage();
const items = ref<PendingApproval[]>([]);
let timer: number | undefined;
let unlisten: UnlistenFn | undefined;

async function refresh() {
  try {
    items.value = await api.approvalsPending();
  } catch {
    /* 锁定状态下静默 */
  }
}

async function decide(item: PendingApproval, approve: boolean) {
  try {
    await api.approvalDecide(item.id, approve);
    message.success(approve ? "已批准，命令继续执行" : "已拒绝，命令不会执行");
  } catch (e) {
    message.error(String(e));
  } finally {
    refresh();
  }
}

function secondsLeft(iso: string): number {
  return Math.max(0, Math.round((new Date(iso).getTime() - Date.now()) / 1000));
}

onMounted(async () => {
  refresh();
  timer = window.setInterval(refresh, 2000);
  unlisten = await listen("approvals-updated", () => refresh());
});
onUnmounted(() => {
  if (timer) window.clearInterval(timer);
  unlisten?.();
});
</script>

<template>
  <div class="page">
    <n-card title="待审批的 AI 操作" size="small">
      <template #header-extra>
        <n-tag size="small" :type="items.length ? 'warning' : 'default'" :bordered="false">
          {{ items.length }} 条待处理
        </n-tag>
      </template>
      <n-empty v-if="!items.length" description="暂无待审批操作。AI 提交高危命令时会出现在这里（120 秒超时自动拒绝）" />
      <div v-for="it in items" :key="it.id" class="approval-item">
        <div class="meta">
          <n-tag size="small" type="warning" :bordered="false">AI 请求</n-tag>
          <span class="server">{{ it.serverName }}</span>
          <span class="ttl">剩余 {{ secondsLeft(it.expiresAt) }}s</span>
        </div>
        <pre class="cmd">{{ it.command }}</pre>
        <approval-review-note v-if="it.aiNote" :note="it.aiNote" class="ai-note" />
        <div class="actions">
          <n-button size="small" type="error" ghost @click="decide(it, false)">拒绝</n-button>
          <n-button size="small" type="primary" @click="decide(it, true)">批准执行</n-button>
        </div>
      </div>
    </n-card>
  </div>
</template>

<style scoped>
/* 页面按内容自然增高，交给外层 layout-content 滚动（固定 height:100% 会让长内容滚不动） */
.page {
  height: auto;
  min-height: 100%;
}
.approval-item {
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  padding: 12px;
  margin-top: 12px;
  /* 单个审批项本身也能滚动，避免极端长内容撑爆整页 */
  max-height: 70vh;
  overflow-y: auto;
}
.meta {
  display: flex;
  align-items: center;
  gap: 10px;
}
.server {
  font-weight: 600;
}
.ttl {
  margin-left: auto;
  font-size: 12px;
  opacity: 0.6;
}
.cmd {
  background: rgba(255, 255, 255, 0.04);
  border-radius: 6px;
  padding: 10px 12px;
  font-size: 13px;
  line-height: 1.6;
  margin: 10px 0;
  white-space: pre-wrap;
  word-break: break-all;
  /* 超长命令在自己的框里滚动，审批按钮始终可见 */
  max-height: 28vh;
  overflow-y: auto;
}
.ai-note {
  border-radius: 6px;
  padding: 8px 12px;
  margin-bottom: 10px;
}
.actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  /* 吸底：即使审批项滚动，按钮也钉在可视区底部 */
  position: sticky;
  bottom: 0;
  padding-top: 8px;
  background: rgba(22, 22, 26, 0.92);
  backdrop-filter: blur(4px);
  border-top: 1px dashed rgba(255, 255, 255, 0.08);
}
</style>
