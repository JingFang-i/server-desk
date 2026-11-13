<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { NButton, NCard, NEmpty, NTag, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type PendingApproval } from "../api";

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
        <div class="actions">
          <n-button size="small" type="error" ghost @click="decide(it, false)">拒绝</n-button>
          <n-button size="small" type="primary" @click="decide(it, true)">批准执行</n-button>
        </div>
      </div>
    </n-card>
  </div>
</template>

<style scoped>
.approval-item {
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  padding: 12px;
  margin-top: 12px;
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
}
.actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
</style>
