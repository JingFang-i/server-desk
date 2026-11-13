<script setup lang="ts">
import { computed, h, onMounted, ref } from "vue";
import { NButton, NCard, NSelect, NTag, useMessage, type DataTableColumns } from "naive-ui";
import { api, type AuditEntry } from "../api";

const message = useMessage();
const rows = ref<AuditEntry[]>([]);
const loading = ref(false);
const sourceFilter = ref<string>("all");

const sourceOptions = [
  { label: "全部来源", value: "all" },
  { label: "AI（MCP）", value: "ai" },
  { label: "用户手动", value: "manual" },
  { label: "用户（界面操作）", value: "user" },
];

const rowKey = (r: AuditEntry) => r.id;

function fmtTs(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString("zh-CN", { hour12: false });
}

function outcomeTag(outcome: string) {
  const lower = outcome.toLowerCase();
  const type = lower.startsWith("ok")
    ? "success"
    : lower.includes("block") || lower.includes("denied")
      ? "error"
      : "default";
  return h(
    NTag,
    { size: "small", type: type as "success" | "error" | "default", bordered: false },
    { default: () => outcome }
  );
}

const columns = computed<DataTableColumns<AuditEntry>>(() => [
  { title: "时间", key: "ts", width: 165, render: (r) => fmtTs(r.ts) },
  {
    title: "来源",
    key: "source",
    width: 90,
    render: (r) =>
      h(
        NTag,
        {
          size: "small",
          bordered: false,
          type: r.source === "ai" ? "warning" : r.source === "user" ? "info" : "default",
        },
        { default: () => r.source }
      ),
  },
  { title: "操作", key: "action", width: 180 },
  { title: "服务器", key: "serverId", width: 200, render: (r) => r.serverId ?? "—" },
  {
    title: "内容",
    key: "detail",
    minWidth: 260,
    render: (r) =>
      h("span", { style: "font-size:12px;white-space:pre-wrap;word-break:break-all" }, r.detail || "—"),
  },
  { title: "结果", key: "outcome", width: 190, render: (r) => outcomeTag(r.outcome) },
]);

async function refresh() {
  loading.value = true;
  try {
    rows.value = await api.auditList(300, sourceFilter.value);
  } catch (e) {
    message.error(String(e));
  } finally {
    loading.value = false;
  }
}

onMounted(refresh);
</script>

<template>
  <div class="page">
    <n-card title="审计日志" size="small">
      <template #header-extra>
        <div style="display: flex; gap: 8px; align-items: center">
          <n-select
            v-model:value="sourceFilter"
            :options="sourceOptions"
            style="width: 160px"
            size="small"
            @update:value="refresh"
          />
          <n-button size="small" :loading="loading" @click="refresh">刷新</n-button>
        </div>
      </template>
      <n-data-table
        :columns="columns"
        :data="rows"
        :loading="loading"
        :bordered="false"
        size="small"
        :row-key="rowKey"
        :max-height="520"
      />
      <p class="hint" style="margin-top: 10px">
        所有 AI 操作、人工命令与安全事件（拦截 / 审批 / 授权变更）均记录在案；内容已自动脱敏。
      </p>
    </n-card>
  </div>
</template>
