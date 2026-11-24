<script setup lang="ts">
import { computed, h, onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NInput,
  NModal,
  NSelect,
  NTag,
  useMessage,
  type DataTableColumns,
} from "naive-ui";
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

/** 会话筛选在 SQL 层完成（不再只筛前端已加载的最近 300 行） */
const sessionFilter = ref<string>("all");
const sessionList = ref<string[]>([]);
const sessionOptions = computed(() =>
  [{ label: "全部会话", value: "all" }].concat(
    sessionList.value.map((s) => ({ label: s, value: s }))
  )
);
/** 审计落库失败次数：>0 时必须让用户看到，不能静默 */
const auditFailures = ref(0);

function fmtTs(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString("zh-CN", { hour12: false });
}

/** 列表里只展示内容摘要（折叠空白并截断） */
const DETAIL_PREVIEW_LEN = 42;
function preview(text: string): string {
  if (!text) return "—";
  const oneLine = text.replace(/\s+/g, " ").trim();
  return oneLine.length > DETAIL_PREVIEW_LEN
    ? oneLine.slice(0, DETAIL_PREVIEW_LEN) + "…"
    : oneLine;
}

function outcomeTag(row: AuditEntry) {
  const lower = row.outcome.toLowerCase();
  const type = row.ok
    ? "success"
    : lower.includes("block") || lower.includes("denied") || lower.includes("timed_out")
      ? "error"
      : "default";
  return h(
    NTag,
    { size: "small", type: type as "success" | "error" | "default", bordered: false },
    { default: () => row.outcome }
  );
}

// ---------- 详情弹窗 ----------
const detailModal = ref(false);
const detailEntry = ref<AuditEntry | null>(null);

function openDetail(row: AuditEntry) {
  detailEntry.value = row;
  detailModal.value = true;
}

async function copyDetail() {
  if (!detailEntry.value) return;
  const e = detailEntry.value;
  const serverLabel = e.serverName || e.serverId || "—";
  const serverExtra = e.serverName && e.serverId ? `（ID: ${e.serverId}）` : "";
  const text = [
    `时间: ${fmtTs(e.ts)}`,
    `来源: ${e.source}`,
    `操作: ${e.action}`,
    `服务器: ${serverLabel}${serverExtra}`,
    `结果: ${e.outcome}`,
    `内容:\n${e.detail || "—"}`,
  ].join("\n");
  try {
    await navigator.clipboard.writeText(text);
    message.success("已复制详情");
  } catch {
    message.warning("复制失败，请手动选择文本复制");
  }
}

const columns = computed<DataTableColumns<AuditEntry>>(() => [
  { title: "时间", key: "ts", width: 160, render: (r) => fmtTs(r.ts) },
  {
    title: "来源",
    key: "source",
    width: 88,
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
  { title: "操作", key: "action", width: 170 },
  {
    title: "服务器",
    key: "serverName",
    width: 200,
    render: (r) => {
      if (!r.serverName && !r.serverId) return "—";
      const lines = [];
      if (r.serverName) {
        lines.push(h("div", { style: "font-size:13px;font-weight:600" }, r.serverName));
      }
      if (r.serverId) {
        lines.push(
          h(
            "div",
            {
              style:
                "font-size:11px;opacity:.5;font-family:ui-monospace,Menlo,monospace;overflow:hidden;text-overflow:ellipsis;white-space:nowrap",
            },
            r.serverId
          )
        );
      }
      return h("div", {}, lines);
    },
  },
  {
    title: "内容",
    key: "detail",
    minWidth: 240,
    render: (r) =>
      h(
        "span",
        {
          style:
            "font-size:12px;opacity:.85;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;display:block",
        },
        preview(r.detail)
      ),
  },
  { title: "结果", key: "outcome", width: 170, render: (r) => outcomeTag(r) },
  {
    title: "会话",
    key: "session",
    width: 110,
    render: (r) =>
      r.session
        ? h("span", { style: "font-size:11px;font-family:ui-monospace,Menlo,monospace;opacity:.7" }, r.session)
        : "—",
  },
  {
    title: "",
    key: "actions",
    width: 76,
    render: (r) =>
      h(
        NButton,
        { size: "tiny", onClick: () => openDetail(r) },
        { default: () => "详情" }
      ),
  },
]);

async function refresh() {
  loading.value = true;
  try {
    rows.value = await api.auditList(300, sourceFilter.value, sessionFilter.value);
    auditFailures.value = await api.auditFailures();
    sessionList.value = await api.auditSessions();
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
          <n-select
            v-model:value="sessionFilter"
            :options="sessionOptions"
            size="small"
            style="width: 200px"
            placeholder="按会话筛选"
            @update:value="refresh"
          />
          <n-button size="small" :loading="loading" @click="refresh">刷新</n-button>
        </div>
      </template>
      <n-alert
        v-if="auditFailures > 0"
        type="error"
        :bordered="false"
        style="margin-bottom: 10px"
        title="审计写入失败"
      >
        有 {{ auditFailures }} 条操作未能写入审计表（数据库异常或磁盘问题）。这些操作本身可能已经执行，
        请检查数据目录权限与剩余空间。
      </n-alert>
      <n-data-table
        :columns="columns"
        :data="rows"
        :loading="loading"
        :bordered="false"
        size="small"
        :row-key="rowKey"
        :max-height="520"
        :scroll-x="1080"
      />
      <p class="hint" style="margin-top: 10px">
        所有 AI 操作、人工命令与安全事件（拦截 / 审批 / 授权变更）均记录在案；列表仅显示内容摘要，点「详情」查看全文。服务器列显示操作发生时的名称快照（服务器改名或删除后仍可追溯）。内容已自动脱敏。
      </p>
    </n-card>

    <n-modal
      v-model:show="detailModal"
      preset="card"
      title="审计详情"
      style="width: 640px"
    >
      <div v-if="detailEntry">
        <div class="detail-grid">
          <span class="k">时间</span><span>{{ fmtTs(detailEntry.ts) }}</span>
          <span class="k">来源</span>
          <span>
            <n-tag
              size="small"
              :bordered="false"
              :type="detailEntry.source === 'ai' ? 'warning' : detailEntry.source === 'user' ? 'info' : 'default'"
            >
              {{ detailEntry.source }}
            </n-tag>
          </span>
          <span class="k">操作</span><span>{{ detailEntry.action }}</span>
          <span class="k">服务器</span>
          <span>{{ detailEntry.serverName || detailEntry.serverId || "—" }}</span>
          <template v-if="detailEntry.serverName && detailEntry.serverId">
            <span class="k">服务器 ID</span>
            <span style="font-family: ui-monospace, Menlo, monospace; font-size: 12px; opacity: 0.7">
              {{ detailEntry.serverId }}
            </span>
          </template>
          <span class="k">结果</span><span>{{ detailEntry.outcome }}</span>
          <template v-if="detailEntry.session">
            <span class="k">会话</span>
            <span style="font-family: ui-monospace, Menlo, monospace; font-size: 12px">{{ detailEntry.session }}</span>
          </template>
        </div>
        <div class="k" style="margin: 14px 0 6px">内容</div>
        <n-input
          :value="detailEntry.detail || '—'"
          type="textarea"
          :rows="10"
          readonly
          style="font-family: ui-monospace, Menlo, monospace; font-size: 12px"
        />
      </div>
      <template #footer>
        <div style="display: flex; justify-content: flex-end">
          <n-button size="small" @click="copyDetail">复制详情</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
.detail-grid {
  display: grid;
  grid-template-columns: 72px 1fr;
  row-gap: 10px;
  font-size: 13px;
}
.k {
  opacity: 0.55;
  font-size: 13px;
}
</style>
