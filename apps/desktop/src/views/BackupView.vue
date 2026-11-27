<script setup lang="ts">
import { onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NInput,
  NInputNumber,
  NPopconfirm,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NSwitch,
  NTag,
  useDialog,
  useMessage,
} from "naive-ui";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, type BackupInfo } from "../api";

const message = useMessage();
const dialog = useDialog();
const info = ref<BackupInfo | null>(null);
const loading = ref(true);
const loadError = ref(false);

// 手动导出
const exportPassword = ref("");
const exportConfirm = ref("");
const exporting = ref(false);

// 手动恢复
const restorePath = ref("");
const restorePassword = ref("");
const restoreMode = ref<"merge" | "overwrite">("merge");
const restoring = ref(false);

// 自动备份
const autoEnabled = ref(false);
const autoKeep = ref(7);
const autoInterval = ref(24);
const savingAuto = ref(false);
const autoRunning = ref(false);

const intervalOptions = [
  { label: "每 6 小时", value: 6 },
  { label: "每 12 小时", value: 12 },
  { label: "每天", value: 24 },
  { label: "每 2 天", value: 48 },
];

async function refresh() {
  loading.value = true;
  loadError.value = false;
  try {
    info.value = await api.backupInfo();
    autoEnabled.value = info.value.settings.enabled;
    autoKeep.value = info.value.settings.keep;
    autoInterval.value = info.value.settings.intervalHours;
  } catch {
    loadError.value = true;
  } finally {
    loading.value = false;
  }
}

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

onMounted(refresh);

async function doExport() {
  if (exportPassword.value.length < 8) {
    message.error("备份口令至少 8 位");
    return;
  }
  if (exportPassword.value !== exportConfirm.value) {
    message.error("两次输入的备份口令不一致");
    return;
  }
  const path = await save({
    title: "选择备份保存位置",
    defaultPath: `serverdesk-backup-${new Date().toISOString().slice(0, 10)}.sdbackup`,
    filters: [{ name: "ServerDesk 加密备份", extensions: ["sdbackup"] }],
  });
  if (!path) return;
  exporting.value = true;
  try {
    const r = await api.backupExport(path, exportPassword.value);
    message.success(`已导出 ${r.count} 台服务器（凭据已加密）→ ${r.path}`);
    exportPassword.value = "";
    exportConfirm.value = "";
  } catch (e) {
    message.error(String(e));
  } finally {
    exporting.value = false;
  }
}

async function pickFile() {
  const path = await open({
    title: "选择备份文件",
    multiple: false,
    filters: [{ name: "ServerDesk 加密备份", extensions: ["sdbackup"] }],
  });
  if (typeof path === "string") restorePath.value = path;
}

function requestRestore() {
  if (restoring.value) return;
  if (!restorePath.value) {
    message.error("请先选择备份文件");
    return;
  }
  if (!restorePassword.value) {
    message.error("请输入备份口令");
    return;
  }
  if (restoreMode.value === "overwrite") {
    dialog.warning({
      title: "覆盖恢复服务器资产",
      content: "当前服务器资产和已保存的 SSH 主机指纹将被清空，然后导入所选备份。",
      positiveText: "覆盖恢复",
      negativeText: "取消",
      onPositiveClick: () => doRestore("overwrite"),
    });
    return;
  }
  void doRestore("merge");
}

async function doRestore(mode: "merge" | "overwrite") {
  if (restoring.value) return;
  restoring.value = true;
  try {
    const r = await api.backupRestore(restorePath.value, restorePassword.value, mode);
    message.success(`恢复完成：${r.imported} 台服务器（${r.mode === "merge" ? "合并" : "覆盖"}）`);
    restorePassword.value = "";
    restorePath.value = "";
  } catch (e) {
    message.error(String(e));
  } finally {
    restoring.value = false;
  }
}

async function saveAuto() {
  savingAuto.value = true;
  try {
    info.value = await api.autoBackupSaveSettings(autoEnabled.value, autoKeep.value, autoInterval.value);
    message.success(
      autoEnabled.value
        ? `自动备份已开启：每 ${autoInterval.value} 小时一次，保留最近 ${autoKeep.value} 份`
        : "自动备份已关闭"
    );
  } catch (e) {
    message.error(String(e));
  } finally {
    savingAuto.value = false;
  }
}

async function runAutoNow() {
  autoRunning.value = true;
  try {
    const path = await api.autoBackupNow();
    message.success(`备份完成 → ${path}`);
    refresh();
  } catch (e) {
    message.error(String(e));
  } finally {
    autoRunning.value = false;
  }
}

async function restoreAuto(name: string) {
  try {
    const r = await api.autoBackupRestore(name, "merge");
    message.success(`恢复完成：${r.imported} 台服务器（合并）`);
  } catch (e) {
    message.error(String(e));
  }
}

async function deleteAuto(name: string) {
  try {
    await api.autoBackupDelete(name);
    message.success("已删除");
    refresh();
  } catch (e) {
    message.error(String(e));
  }
}
</script>

<template>
  <div class="page backup-page">
    <n-card title="导出加密备份" size="small">
      <div class="row">
        <span class="lab">备份口令</span>
        <n-input
          v-model:value="exportPassword"
          type="password"
          show-password-on="click"
          placeholder="至少 8 位，用于加密备份文件（忘记则无法恢复）"
          style="flex: 1"
        />
      </div>
      <div class="row">
        <span class="lab">确认口令</span>
        <n-input
          v-model:value="exportConfirm"
          type="password"
          show-password-on="click"
          placeholder="再次输入备份口令"
          style="flex: 1"
        />
      </div>
      <template #footer>
        <n-button type="primary" :loading="exporting" @click="doExport">
          选择位置并导出（AES-256-GCM）
        </n-button>
        <span class="hint" style="margin-left: 12px">
          内容：全部服务器连接信息 + 加密凭据。文件本身为密文，无口令不可读
        </span>
      </template>
    </n-card>

    <n-card title="从备份文件恢复" size="small">
      <div class="row">
        <span class="lab">备份文件</span>
        <n-input v-model:value="restorePath" readonly placeholder="尚未选择" style="flex: 1" />
        <n-button @click="pickFile">选择文件…</n-button>
      </div>
      <div class="row">
        <span class="lab">备份口令</span>
        <n-input
          v-model:value="restorePassword"
          type="password"
          show-password-on="click"
          style="flex: 1"
        />
      </div>
      <div class="row">
        <span class="lab">恢复模式</span>
        <n-radio-group v-model:value="restoreMode">
          <n-radio-button value="merge">合并</n-radio-button>
          <n-radio-button value="overwrite">覆盖</n-radio-button>
        </n-radio-group>
      </div>
      <template #footer>
        <div class="toolbar">
          <n-button :type="restoreMode === 'overwrite' ? 'warning' : 'primary'" :loading="restoring" @click="requestRestore">
            {{ restoreMode === "overwrite" ? "覆盖恢复…" : "合并恢复" }}
          </n-button>
          <span class="hint">{{ restoreMode === "overwrite"
            ? "先清空现有服务器资产及其 SSH 指纹，再导入备份。"
            : "同 ID 的服务器会更新，备份中的新服务器会添加，其余服务器仍保留。" }}</span>
        </div>
      </template>
    </n-card>

    <n-card title="本机自动备份" size="small">
      <div class="row">
        <div class="label">
          <div>开启定时自动备份</div>
          <div class="desc">
            应用运行期间按周期自动备份到本机数据目录，防误删/数据库损坏。
            用当前数据密钥加密，恢复时需保险库处于解锁状态
          </div>
        </div>
        <n-switch v-model:value="autoEnabled" />
      </div>
      <div class="row">
        <span class="lab">备份周期</span>
        <n-select v-model:value="autoInterval" :options="intervalOptions" style="width: 140px" />
        <span class="lab" style="margin-left: 16px">保留份数</span>
        <n-input-number v-model:value="autoKeep" :min="1" :max="50" style="width: 110px" />
      </div>
      <div class="row" v-if="info?.settings.lastRun">
        <span class="lab">上次自动备份</span>
        <span class="desc">{{ info.settings.lastRun }}</span>
      </div>
      <template #footer>
        <n-button type="primary" :loading="savingAuto" @click="saveAuto">保存自动备份设置</n-button>
        <n-button style="margin-left: 10px" :loading="autoRunning" @click="runAutoNow">
          立即备份一次
        </n-button>
      </template>

      <n-alert v-if="loadError" type="error" title="暂时无法读取自动备份">
        <n-button size="tiny" secondary @click="refresh">重试</n-button>
      </n-alert>
      <div v-else-if="info?.files.length" class="filelist">
        <div v-for="f in info.files" :key="f.name" class="filerow">
          <span class="fname">auto-{{ f.createdAt }}</span>
          <span class="fsize">{{ fmtSize(f.sizeBytes) }}</span>
          <n-popconfirm @positive-click="restoreAuto(f.name)">
            <template #trigger>
              <n-button size="tiny" type="primary" ghost>恢复（合并）</n-button>
            </template>
            将备份内容按 ID 合并到当前数据？
          </n-popconfirm>
          <n-popconfirm @positive-click="deleteAuto(f.name)">
            <template #trigger>
              <n-button size="tiny" type="error" ghost>删除</n-button>
            </template>
            确认删除该自动备份？
          </n-popconfirm>
        </div>
      </div>
      <p v-else class="hint" role="status">{{ loading ? "正在读取自动备份…" : "暂无自动备份文件" }}</p>
    </n-card>
  </div>
</template>

<style scoped>
.backup-page { max-width: 850px; }
.row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 0;
}
.lab {
  width: 88px;
  flex-shrink: 0;
  font-size: 13px;
  opacity: 0.85;
}
.label > div:first-child {
  font-weight: 600;
  font-size: 13px;
}
.desc {
  font-size: 12px;
  opacity: 0.6;
  margin-top: 4px;
  line-height: 1.6;
}
.filelist {
  margin-top: 14px;
  border-top: 1px dashed rgba(255, 255, 255, 0.1);
  padding-top: 6px;
}
.filerow {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 0;
  border-bottom: 1px dashed rgba(255, 255, 255, 0.06);
}
.fname {
  font-family: ui-monospace, Menlo, monospace;
  font-size: 12px;
}
.fsize {
  font-size: 12px;
  opacity: 0.6;
  width: 70px;
}
</style>
