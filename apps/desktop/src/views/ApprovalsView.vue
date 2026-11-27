<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { NAlert, NButton, NCard, NEmpty, NSpin, NTag, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type PendingApproval } from "../api";
import ApprovalReviewNote from "../components/ApprovalReviewNote.vue";

const emit = defineEmits<{ updated: [] }>();
const message = useMessage();
const items = ref<PendingApproval[]>([]);
const loading = ref(true);
const loadError = ref(false);
const now = ref(Date.now());
const deciding = reactive(new Map<string, boolean>());
const errors = reactive(new Map<string, string>());
const handled = new Set<string>();
let timer: number | undefined;
let unlisten: UnlistenFn | undefined;
let version = 0;
let disposed = false;

function secondsLeft(expiresAt: string) {
  return Math.max(0, Math.ceil((Date.parse(expiresAt) - now.value) / 1000));
}

const sorted = computed(() => [...items.value].sort(
  (a, b) => Date.parse(a.expiresAt) - Date.parse(b.expiresAt),
));

async function refresh() {
  const request = ++version;
  try {
    const next = await api.approvalsPending();
    if (request !== version) return;
    const pendingIds = new Set(next.map(({ id }) => id));
    for (const id of handled) if (!pendingIds.has(id)) handled.delete(id);
    items.value = next.filter(({ id }) => !handled.has(id));
    for (const id of errors.keys()) if (!pendingIds.has(id)) errors.delete(id);
    loadError.value = false;
  } catch {
    if (request === version) loadError.value = true;
  } finally {
    if (request === version) loading.value = false;
  }
}

async function decide(item: PendingApproval, approved: boolean) {
  if (deciding.has(item.id) || Date.parse(item.expiresAt) <= Date.now()) return;
  deciding.set(item.id, approved);
  errors.delete(item.id);
  try {
    await api.approvalDecide(item.id, approved);
    handled.add(item.id);
    items.value = items.value.filter(({ id }) => id !== item.id);
    message.success(approved ? "已批准执行，可在审计日志查看结果" : "已拒绝");
  } catch (e) {
    if (String(e).includes("审批不存在或已过期") || Date.parse(item.expiresAt) <= Date.now()) {
      handled.add(item.id);
      items.value = items.value.filter(({ id }) => id !== item.id);
      message.warning("该请求已过期或已被处理，请查看审计日志");
    } else if (items.value.some(({ id }) => id === item.id)) {
      errors.set(item.id, "未能提交审批决定，请重试");
    } else {
      message.warning("该请求已过期或已被处理，请查看审计日志");
    }
  } finally {
    deciding.delete(item.id);
    emit("updated");
    void refresh();
  }
}

onMounted(() => {
  void refresh();
  let ticks = 0;
  timer = window.setInterval(() => {
    now.value = Date.now();
    if (++ticks % 2 === 0) void refresh();
  }, 1000);
  void listen("approvals-updated", refresh).then((stop) => {
    if (disposed) stop();
    else unlisten = stop;
  }).catch(() => {});
});

onUnmounted(() => {
  disposed = true;
  version++;
  window.clearInterval(timer);
  unlisten?.();
});
</script>

<template>
  <div class="page approvals-page">
    <div class="toolbar approval-toolbar">
      <h2>待处理请求 <span class="approval-total">{{ items.length }}</span></h2>
      <n-button size="small" secondary @click="refresh">刷新</n-button>
    </div>
    <n-alert v-if="loadError" type="error" title="暂时无法刷新审批">
      请重试。请求是否仍可处理以服务器返回的状态为准。
    </n-alert>
    <n-card v-if="loading || (!items.length && !loadError)" class="approval-empty">
      <n-spin v-if="loading" description="正在读取审批…" />
      <n-empty v-else description="暂无待审批请求" />
    </n-card>
    <n-card v-for="it in sorted" :key="it.id" class="approval-item" size="small">
      <template #header>
        <div class="approval-target">
          <span>{{ it.serverName }}</span>
          <n-tag size="small" type="warning" :bordered="false">AI 请求</n-tag>
        </div>
      </template>
      <template #header-extra>
        <span class="approval-clock" :class="{ urgent: secondsLeft(it.expiresAt) <= 20 }">
          {{ secondsLeft(it.expiresAt) ? `剩余 ${secondsLeft(it.expiresAt)} 秒` : "已过期" }}
        </span>
      </template>
      <pre class="approval-command">{{ it.command }}</pre>
      <ApprovalReviewNote v-if="it.aiNote" :note="it.aiNote" />
      <n-alert v-if="errors.has(it.id)" type="error" class="approval-error">{{ errors.get(it.id) }}</n-alert>
      <template #footer>
        <div class="approval-actions">
          <span class="hint">120 秒未处理将自动拒绝</span>
          <n-button size="small" :disabled="deciding.has(it.id) || !secondsLeft(it.expiresAt)"
            :loading="deciding.get(it.id) === false" @click="decide(it, false)">拒绝</n-button>
          <n-button size="small" type="warning" secondary :disabled="deciding.has(it.id) || !secondsLeft(it.expiresAt)"
            :loading="deciding.get(it.id) === true" @click="decide(it, true)">批准执行</n-button>
        </div>
      </template>
    </n-card>
  </div>
</template>

<style scoped>
.approvals-page { max-width: 900px; margin-inline: auto; }
.approval-toolbar, .approval-target, .approval-actions { justify-content: space-between; }
.approval-toolbar h2 { font-size: 15px; margin: 0; }
.approval-total { margin-inline-start: 6px; color: var(--muted); font: 13px var(--mono); }
.approval-target, .approval-actions { display: flex; align-items: center; gap: 9px; flex-wrap: wrap; }
.approval-target { justify-content: flex-start; overflow-wrap: anywhere; }
.approval-clock { font: 12px var(--mono); color: var(--muted); white-space: nowrap; }
.approval-clock.urgent { color: var(--warning); }
.approval-command {
  margin: 0 0 12px; padding: 12px; border-radius: var(--radius-control); background: var(--canvas);
  font-size: 13px; white-space: pre-wrap; overflow-wrap: anywhere; max-height: 28vh; overflow: auto;
}
.approval-actions .hint { margin: 0 auto 0 0; }
.approval-error { margin-top: 10px; }
.approval-empty { text-align: center; padding: 25px; }
</style>
