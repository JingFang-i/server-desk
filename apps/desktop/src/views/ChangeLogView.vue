<script setup lang="ts">
import { computed, h, onMounted, ref } from "vue";
import {
  NButton,
  NCard,
  NDataTable,
  NEmpty,
  NSelect,
  NTag,
  useDialog,
  useMessage,
  type DataTableColumns,
} from "naive-ui";
import { api, type AuditEntry, type FileBackupMeta, type ServerConfig } from "../api";

const message = useMessage();
const dialog = useDialog();

const backups = ref<FileBackupMeta[]>([]);
const changes = ref<AuditEntry[]>([]);
const servers = ref<ServerConfig[]>([]);
const loading = ref(false);
const serverFilter = ref<string | null>(null);

/** id → 当前名称：服务器已删除时退回显示 id */
const serverNames = computed(() => {
  const m = new Map<string, string>();
  for (const s of servers.value) m.set(s.id, s.name);
  return m;
});
function nameOf(id: string | null | undefined): string {
  if (!id) return "—";
  return serverNames.value.get(id) ?? id;
}

const serverOptions = computed(() => {
  const ids = new Set<string>();
  for (const s of servers.value) ids.add(s.id);
  for (const b of backups.value) ids.add(b.serverId);
  for (const c of changes.value) if (c.serverId) ids.add(c.serverId);
  return [...ids].map((id) => ({ label: nameOf(id), value: id }));
});

const displayBackups = computed(() =>
  serverFilter.value
    ? backups.value.filter((b) => b.serverId === serverFilter.value)
    : backups.value
);
const displayChanges = computed(() => {
  const writeActions = new Set([
    "mcp_write_file", "mcp_append_file", "mcp_upload_file",
    "mcp_restore_file", "file_backup_restore", "sftp_write", "sftp_upload",
  ]);
  return changes.value.filter(
    (c) => writeActions.has(c.action) && (!serverFilter.value || c.serverId === serverFilter.value)
  );
});

function fmtTs(ts: string): string {
  const d = new Date(/Z|[+-]/.test(ts) ? ts : ts.replace(" ", "T") + "Z");
  return isNaN(+d) ? ts : d.toLocaleString("zh-CN", { hour12: false });
}

const backupColumns: DataTableColumns<FileBackupMeta> = [
  { title: "备份时间", key: "ts", width: 165, render: (r) => fmtTs(r.ts) },
  {
    title: "服务器",
    key: "serverId",
    width: 200,
    render: (r) =>
      h("div", null, [
        h("div", { style: "font-size:13px;font-weight:600" }, nameOf(r.serverId)),
        h(
          "div",
          {
            style:
              "font-size:11px;opacity:.5;font-family:ui-monospace,Menlo,monospace;overflow:hidden;text-overflow:ellipsis;white-space:nowrap",
          },
          r.serverId
        ),
      ]),
  },
  {
    title: "内容",
    key: "encrypted",
    width: 90,
    render: (r) =>
      h(
        NTag,
        { size: "tiny", bordered: false, type: r.encrypted ? "success" : "warning" },
        { default: () => (r.encrypted ? "已加密" : "旧明文") }
      ),
  },
  {
    title: "原文件路径",
    key: "path",
    minWidth: 240,
    render: (r) => h("span", { style: "font-family:ui-monospace,Menlo,monospace;font-size:12px" }, r.path),
  },
  { title: "大小", key: "sizeBytes", width: 90, render: (r) => `${r.sizeBytes} B` },
  {
    title: "",
    key: "actions",
    width: 90,
    render: (r) =>
      h(
        NButton,
        { size: "tiny", type: "warning", onClick: () => askRestore(r) },
        { default: () => "回滚" }
      ),
  },
];

const changeColumns: DataTableColumns<AuditEntry> = [
  { title: "时间", key: "ts", width: 165, render: (r) => fmtTs(r.ts) },
  {
    title: "来源",
    key: "source",
    width: 80,
    render: (r) =>
      h(
        NTag,
        { size: "small", bordered: false, type: r.source === "ai" ? "warning" : "default" },
        { default: () => r.source }
      ),
  },
  { title: "操作", key: "action", width: 150 },
  {
    title: "服务器",
    key: "serverName",
    width: 170,
    render: (r) => r.serverName || nameOf(r.serverId),
  },
  {
    title: "内容",
    key: "detail",
    minWidth: 260,
    render: (r) =>
      h(
        "span",
        { style: "font-size:12px;font-family:ui-monospace,Menlo,monospace;word-break:break-all" },
        r.detail || "—"
      ),
  },
  { title: "结果", key: "outcome", width: 150 },
];

async function refresh() {
  loading.value = true;
  try {
    backups.value = await api.fileBackupsList();
    changes.value = await api.auditList(200);
    servers.value = (await api.listServers()).servers;
  } catch (e) {
    message.error(String(e));
  } finally {
    loading.value = false;
  }
}

function askRestore(b: FileBackupMeta) {
  dialog.warning({
    title: "回滚文件",
    content: `将把「${b.path}」回滚到备份 ${b.id}（${b.sizeBytes} 字节，备份于 ${fmtTs(b.ts)}）时的内容。确认？`,
    positiveText: "回滚",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        const n = await api.fileBackupRestore(b.serverId, b.id);
        message.success(`已回滚 ${b.path}（${n} 字节）`);
        refresh();
      } catch (e) {
        message.error(String(e));
      }
    },
  });
}

onMounted(refresh);
</script>

<template>
  <div class="page">
    <div class="toolbar">
      <span style="font-size: 13px; opacity: 0.7">服务器</span>
      <n-select
        v-model:value="serverFilter"
        :options="serverOptions"
        clearable
        placeholder="全部服务器"
        style="width: 240px"
        size="small"
      />
      <div style="flex: 1" />
      <n-button size="small" :loading="loading" @click="refresh">刷新</n-button>
    </div>

    <n-card title="文件写前备份（可回滚）" size="small">
      <n-empty v-if="!displayBackups.length" description="暂无备份。AI 通过 write_file 修改文件时会自动留存原文件" />
      <n-data-table
        v-else
        :columns="backupColumns"
        :data="displayBackups"
        :bordered="false"
        size="small"
        :row-key="(r: FileBackupMeta) => r.id"
        :max-height="300"
      />
    </n-card>

    <n-card title="变更事件（AI 写入 / 上传 / 追加 / 回滚）" size="small" style="margin-top: 16px">
      <n-empty v-if="!displayChanges.length" description="暂无变更事件" />
      <n-data-table
        v-else
        :columns="changeColumns"
        :data="displayChanges"
        :bordered="false"
        size="small"
        :row-key="(r: AuditEntry) => r.id"
        :max-height="320"
      />
    </n-card>
  </div>
</template>
