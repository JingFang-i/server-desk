<script setup lang="ts">
import { computed, h, onActivated, onDeactivated, reactive, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NDrawer,
  NDrawerContent,
  NEmpty,
  NSelect,
  NTag,
  useMessage,
  type DataTableColumns,
} from "naive-ui";
import { api, type AuditEntry } from "../api";

const message = useMessage();
const rows = ref<AuditEntry[]>([]);
const loading = ref(false);
const loadError = ref(false);
let refreshVersion = 0;
const sourceFilter = ref<string>("all");

const sourceOptions = [
  { label: "全部来源", value: "all" },
  { label: "AI（MCP）", value: "ai" },
  { label: "用户手动", value: "manual" },
  { label: "用户（界面操作）", value: "user" },
];

/** 动作筛选（R8.4）：SQL 层 action = ?，覆盖高频排查场景 */
const actionFilter = ref<string>("all");
const actionOptions = [
  { label: "全部动作", value: "all" },
  { label: "AI 审查", value: "ai_review" },
  { label: "AI 命令执行", value: "mcp_exec_command" },
  { label: "AI 批量执行", value: "mcp_exec_batch" },
  { label: "AI 文件写入", value: "mcp_write_file" },
  { label: "AI 审批请求", value: "mcp_approval_requested" },
  { label: "系统通知授权", value: "notify_permission" },
  { label: "通知通道测试", value: "notify_test" },
  { label: "活动悬浮窗", value: "float_window" },
  { label: "手动命令", value: "ssh_exec" },
];

const rowKey = (r: AuditEntry) => r.id;

/** 会话筛选在 SQL 层完成；分页也在 SQL 层（LIMIT/OFFSET + COUNT），前端只渲染当前页 */
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

const reviewOutcomes = {
  allow: { label: "AI 审查：放行", type: "success" },
  review: { label: "AI 审查：建议转审批", type: "warning" },
  deny: { label: "AI 审查：建议拒绝", type: "error" },
  skipped: { label: "AI 审查：未送审", type: "default" },
  degraded: { label: "AI 审查：降级", type: "error" },
} as const;

function displayOutcome(row: AuditEntry) {
  const lower = row.outcome.toLowerCase();
  if (row.action === "ai_review") {
    const match = /^(?:ok:)?ai_review:(?:cached:)?(allow|review|deny|skipped|degraded)(?:$|:)/.exec(lower);
    if (match) return reviewOutcomes[match[1] as keyof typeof reviewOutcomes];
  }
  const type = row.ok
    ? "success"
    : lower.includes("block") || lower.includes("denied") || lower.includes("timed_out")
      ? "error"
      : "default";
  return { label: row.outcome, type } as const;
}

function outcomeTag(row: AuditEntry) {
  const { label, type } = displayOutcome(row);
  return h(NTag, { size: "small", type, bordered: false, title: row.outcome }, { default: () => label });
}

// ---------- 详情弹窗 ----------
const detailModal = ref(false);
const detailEntry = ref<AuditEntry | null>(null);

let returnFocus: HTMLElement | null = null;

function openDetail(row: AuditEntry, event: MouseEvent) {
  returnFocus = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
  detailEntry.value = row;
  detailModal.value = true;
}

function closeDetail() {
  detailEntry.value = null;
  returnFocus?.focus();
  returnFocus = null;
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
    fixed: "right",
    render: (r) =>
      h(
        NButton,
        { size: "tiny", onClick: (event: MouseEvent) => openDetail(r, event) },
        { default: () => "详情" }
      ),
  },
]);

/** 服务端分页：翻页/改每页条数都会重新查询 */
const pagination = reactive({
  page: 1,
  pageSize: 50,
  itemCount: 0,
  showSizePicker: true,
  pageSizes: [20, 50, 100, 200],
  prefix: ({ itemCount }: { itemCount?: number }) => `共 ${itemCount ?? 0} 条`,
  onChange: (p: number) => {
    pagination.page = p;
    void refresh();
  },
  onUpdatePageSize: (ps: number) => {
    pagination.pageSize = ps;
    pagination.page = 1;
    void refresh();
  },
});

/** 筛选条件变化回到第 1 页再查 */
function onFilterChange() {
  pagination.page = 1;
  void refresh();
}

async function refresh() {
  const version = ++refreshVersion;
  loading.value = true;
  loadError.value = false;
  try {
    const res = await api.auditPage(
      pagination.page,
      pagination.pageSize,
      sourceFilter.value,
      sessionFilter.value,
      actionFilter.value
    );
    if (version !== refreshVersion) return;
    const lastPage = Math.max(1, Math.ceil(res.total / pagination.pageSize));
    if (pagination.page > lastPage) {
      pagination.page = lastPage;
      void refresh();
      return;
    }
    rows.value = res.entries;
    pagination.itemCount = res.total;
    const [failures, sessions] = await Promise.all([api.auditFailures(), api.auditSessions()]);
    if (version !== refreshVersion) return;
    auditFailures.value = failures;
    sessionList.value = sessions;
  } catch {
    if (version === refreshVersion) loadError.value = true;
  } finally {
    if (version === refreshVersion) loading.value = false;
  }
}

onActivated(() => { void refresh(); });
onDeactivated(() => {
  refreshVersion++;
  detailModal.value = false;
  returnFocus = null;
});
</script>

<template>
  <div class="page">
    <div class="toolbar audit-toolbar">
      <n-select
        v-model:value="sourceFilter"
        :options="sourceOptions"
        class="audit-filter" aria-label="按来源筛选"
        size="small"
        @update:value="onFilterChange"
      />
      <n-select
        v-model:value="actionFilter"
        :options="actionOptions"
        class="audit-filter" aria-label="按动作筛选"
        size="small"
        @update:value="onFilterChange"
      />
      <n-select
        v-model:value="sessionFilter"
        :options="sessionOptions"
        size="small"
        class="audit-filter" aria-label="按会话筛选"
        placeholder="按会话筛选"
        @update:value="onFilterChange"
      />
      <n-button size="small" :loading="loading" @click="refresh">刷新</n-button>
    </div>
    <n-card size="small">
      <n-alert v-if="loadError" type="error" title="暂时无法加载审计记录" class="audit-error">请点击刷新重试。</n-alert>
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
        remote
        :columns="columns"
        :data="rows"
        :loading="loading"
        :bordered="false"
        size="small"
        :row-key="rowKey"
        :max-height="'max(180px, calc(100vh - 290px))'"
        :scroll-x="1080"
        :pagination="pagination"
      >
        <template #empty>
          <n-empty :description="loadError ? '审计记录读取失败' : loading ? '正在读取审计记录…' : '没有匹配的审计记录'" />
        </template>
      </n-data-table>
      <p class="hint" style="margin-top: 10px">
        所有 AI 操作、人工命令与安全事件（拦截 / 审批 / 授权变更）均记录在案；列表仅显示内容摘要，点「详情」查看全文。服务器列显示操作发生时的名称快照（服务器改名或删除后仍可追溯）。内容已自动脱敏。
      </p>
    </n-card>

    <n-drawer v-model:show="detailModal" width="min(560px, calc(100vw - 48px))" @after-leave="closeDetail">
      <n-drawer-content title="审计详情" closable>
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
            <span class="k">结果</span><span>{{ displayOutcome(detailEntry).label }}</span>
            <template v-if="detailEntry.session">
              <span class="k">会话</span>
              <span style="font-family: ui-monospace, Menlo, monospace; font-size: 12px">{{ detailEntry.session }}</span>
            </template>
          </div>
          <div class="k" style="margin: 14px 0 6px">内容</div>
          <pre class="audit-detail">{{ detailEntry.detail || "—" }}</pre>
        </div>
        <template #footer>
          <n-button size="small" @click="copyDetail">复制详情</n-button>
        </template>
      </n-drawer-content>
    </n-drawer>
  </div>
</template>

<style scoped>
.audit-toolbar { gap: 8px; }
.audit-filter { width: 154px; }
.audit-error { margin-bottom: 10px; }
.audit-detail {
  white-space: pre-wrap; overflow-wrap: anywhere; padding: 12px; background: var(--canvas);
  border-radius: var(--radius-control); font-size: 12px;
}
.detail-grid {
  display: grid;
  grid-template-columns: 72px minmax(0, 1fr);
  overflow-wrap: anywhere;
  row-gap: 10px;
  font-size: 13px;
}
.k {
  color: var(--muted);
  font-size: 13px;
}
</style>
