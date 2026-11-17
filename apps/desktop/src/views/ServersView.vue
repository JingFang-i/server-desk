<script setup lang="ts">
import { computed, h, onMounted, onUnmounted, reactive, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NDropdown,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NInputNumber,
  NModal,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NSwitch,
  NTag,
  NTooltip,
  useDialog,
  useMessage,
  type DataTableColumns,
  type DropdownOption,
  type FormInst,
  type FormRules,
} from "naive-ui";
import { EllipsisVertical, LinkOutline, TerminalOutline } from "@vicons/ionicons5";
import { api, type SaveServerInput, type ServerConfig } from "../api";
import { open, save as save$ } from "@tauri-apps/plugin-dialog";
import { useServersStore } from "../stores/servers";

const store = useServersStore();
const message = useMessage();
const dialog = useDialog();

// 行内下拉操作菜单
const actionOptions: DropdownOption[] = [
  { label: "SSH 测试", key: "test" },
  { label: "编辑", key: "edit" },
  { label: "文件传输", key: "files" },
  { label: "重置指纹", key: "reset" },
  { type: "divider", key: "d1" },
  { label: "删除", key: "delete", props: { style: { color: "#e88080" } } },
];

function handleAction(key: string, row: ServerConfig) {
  switch (key) {
    case "test":
      testRow(row);
      break;
    case "edit":
      openEdit(row);
      break;
    case "files":
      openFiles(row);
      break;
    case "reset":
      confirmResetHostkey(row);
      break;
    case "delete":
      confirmRemove(row);
      break;
  }
}

// ---------- 连接状态 ----------
const connMap = ref<Record<string, boolean>>({});
const connectingId = ref<string | null>(null);
let statusTimer: number | undefined;

async function refreshConnStatus() {
  try {
    const list = await api.sshStatus();
    const map: Record<string, boolean> = {};
    for (const c of list) map[c.serverId] = c.status === "connected";
    connMap.value = map;
  } catch {
    /* 锁定状态下静默 */
  }
}

onMounted(() => {
  store.load().catch((e) => message.error(String(e)));
  refreshConnStatus();
  statusTimer = window.setInterval(refreshConnStatus, 5000);
});
onUnmounted(() => {
  if (statusTimer) window.clearInterval(statusTimer);
});

// ---------- 列表过滤 ----------
const keyword = ref("");
const groupFilter = ref<string | null>(null);
const filtered = computed(() =>
  store.servers.filter((s) => {
    const kw = keyword.value.trim().toLowerCase();
    const kwOk =
      !kw ||
      [s.name, s.host, s.username, ...(s.tags ?? [])]
        .join(" ")
        .toLowerCase()
        .includes(kw);
    const gOk = !groupFilter.value || s.group === groupFilter.value;
    return kwOk && gOk;
  })
);
const groupOptions = computed(() => store.groups.map((g) => ({ label: g, value: g })));

// ---------- 连接 / 测试 / 执行 ----------
async function connectRow(row: ServerConfig) {
  connectingId.value = row.id;
  try {
    if (connMap.value[row.id]) {
      await api.sshDisconnect(row.id);
      message.info(`「${row.name}」已断开`);
    } else {
      await api.sshConnect(row.id);
      message.success(`「${row.name}」已连接（保活 30s）`);
    }
  } catch (e) {
    message.error(String(e));
  } finally {
    connectingId.value = null;
    refreshConnStatus();
  }
}

async function testRow(row: ServerConfig) {
  message.loading(`正在测试「${row.name}」的 SSH 认证…`, { duration: 1500 });
  try {
    const r = await api.sshTest(row.id);
    if (r.ok) message.success(`「${row.name}」${r.message}`);
    else message.error(`「${row.name}」${r.message}`);
  } catch (e) {
    message.error(String(e));
  } finally {
    refreshConnStatus();
  }
}

async function resetHostkey(row: ServerConfig) {
  try {
    await api.sshResetHostkey(row.id);
    message.success(`「${row.name}」主机密钥指纹已重置，下次连接将重新记录`);
  } catch (e) {
    message.error(String(e));
  }
}

function confirmRemove(row: ServerConfig) {
  dialog.warning({
    title: "删除确认",
    content: `确认删除「${row.name}」？该操作不可恢复。`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: () => removeRow(row),
  });
}

function confirmResetHostkey(row: ServerConfig) {
  dialog.warning({
    title: "重置主机密钥指纹",
    content: `确认重置「${row.name}」的指纹？仅应在服务器重装/更换主机密钥后使用。`,
    positiveText: "重置",
    negativeText: "取消",
    onPositiveClick: () => resetHostkey(row),
  });
}

// 执行命令弹窗
const execModal = ref(false);
const execTarget = ref<ServerConfig | null>(null);
const execCommand = ref("uptime");
const execRunning = ref(false);
const execOutput = ref<{ stdout: string; stderr: string; exitCode: number | null; truncated: boolean } | null>(null);

function openExec(row: ServerConfig) {
  execTarget.value = row;
  execOutput.value = null;
  execCommand.value = "uptime";
  execModal.value = true;
}

async function runExec() {
  if (!execTarget.value || !execCommand.value.trim()) return;
  execRunning.value = true;
  execOutput.value = null;
  try {
    execOutput.value = await api.sshExec(execTarget.value.id, execCommand.value, 60);
  } catch (e) {
    message.error(String(e));
  } finally {
    execRunning.value = false;
    refreshConnStatus();
  }
}

// ---------- 表格 ----------
const rowKey = (r: ServerConfig) => r.id;
const columns = computed<DataTableColumns<ServerConfig>>(() => [
  {
    title: "名称",
    key: "name",
    width: 190,
    render: (row) =>
      h("div", null, [
        h("div", { style: "font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" }, row.name),
        h("div", { style: "font-size:12px;opacity:.65;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" }, `${row.host}:${row.port}`),
      ]),
  },
  { title: "用户名", key: "username", width: 84 },
  {
    title: "认证",
    key: "auth",
    width: 70,
    render: (row) =>
      row.auth.type === "password"
        ? h(NTag, { size: "small", type: "info", bordered: false }, { default: () => "密码" })
        : h(NTag, { size: "small", type: "success", bordered: false }, { default: () => "私钥" }),
  },
  { title: "分组", key: "group", width: 90, render: (row) => row.group ?? "—" },
  {
    title: "标签",
    key: "tags",
    width: 130,
    render: (row) =>
      row.tags.length
        ? h(
            "div",
            { style: "display:flex;gap:4px;flex-wrap:wrap" },
            row.tags.map((t) =>
              h(NTag, { size: "tiny", bordered: false }, { default: () => t })
            )
          )
        : "—",
  },
  {
    title: "连接",
    key: "conn",
    width: 76,
    render: (row) =>
      connMap.value[row.id]
        ? h(NTag, { size: "small", type: "success", bordered: false }, { default: () => "已连接" })
        : h(NTag, { size: "small", bordered: false }, { default: () => "未连接" }),
  },
  {
    title: "AI",
    key: "allowAi",
    width: 56,
    render: (row) =>
      row.allowAi
        ? h(NTag, { size: "small", type: "warning", bordered: false }, { default: () => "允许" })
        : h(NTag, { size: "small", bordered: false }, { default: () => "禁止" }),
  },
  {
    title: "操作",
    key: "actions",
    width: 104,
    render: (row) =>
      h("div", { style: "display:flex;gap:6px;align-items:center" }, [
        h(NTooltip, null, {
          trigger: () =>
            h(
              NButton,
              {
                size: "tiny",
                circle: true,
                type: connMap.value[row.id] ? "warning" : "primary",
                loading: connectingId.value === row.id,
                onClick: () => connectRow(row),
              },
              {
                icon: () => h(NIcon, null, { default: () => h(LinkOutline) }),
              }
            ),
          default: () => (connMap.value[row.id] ? "断开连接" : "连接"),
        }),
        h(NTooltip, null, {
          trigger: () =>
            h(
              NButton,
              { size: "tiny", circle: true, onClick: () => openExec(row) },
              { icon: () => h(NIcon, null, { default: () => h(TerminalOutline) }) }
            ),
          default: () => "执行命令",
        }),
        h(
          NDropdown,
          { options: actionOptions, trigger: "click", onSelect: (key: string) => handleAction(key, row) },
          {
            default: () =>
              h(
                NButton,
                { size: "tiny", circle: true, quaternary: true },
                { icon: () => h(NIcon, null, { default: () => h(EllipsisVertical) }) }
              ),
          }
        ),
      ]),
  },
]);

// ---------- 新增 / 编辑表单 ----------
const showModal = ref(false);
const editingId = ref<string | null>(null);
const formRef = ref<FormInst | null>(null);
const saving = ref(false);

interface FormModel {
  name: string;
  host: string;
  port: number;
  username: string;
  authType: "password" | "key";
  password: string;
  privateKey: string;
  passphrase: string | null;
  group: string | null;
  tags: string[];
  notes: string | null;
  allowAi: boolean;
}

const emptyModel = (): FormModel => ({
  name: "",
  host: "",
  port: 22,
  username: "root",
  authType: "password",
  password: "",
  privateKey: "",
  passphrase: null,
  group: null,
  tags: [],
  notes: null,
  allowAi: true,
});
const model = reactive<FormModel>(emptyModel());

const rules: FormRules = {
  name: { required: true, message: "请输入名称", trigger: "blur" },
  host: { required: true, message: "请输入主机地址", trigger: "blur" },
  username: { required: true, message: "请输入用户名", trigger: "blur" },
  port: { required: true, type: "number", message: "请输入端口", trigger: "blur" },
};

function openCreate() {
  Object.assign(model, emptyModel());
  editingId.value = null;
  showModal.value = true;
}

function openEdit(row: ServerConfig) {
  Object.assign(model, emptyModel(), {
    name: row.name,
    host: row.host,
    port: row.port,
    username: row.username,
    authType: row.auth.type,
    password: row.auth.type === "password" ? row.auth.password : "",
    privateKey: row.auth.type === "key" ? row.auth.privateKey : "",
    passphrase: row.auth.type === "key" ? row.auth.passphrase : null,
    group: row.group,
    tags: [...row.tags],
    notes: row.notes,
    allowAi: row.allowAi,
  });
  editingId.value = row.id;
  showModal.value = true;
}

function buildInput(): SaveServerInput {
  const auth =
    model.authType === "password"
      ? { type: "password" as const, password: model.password }
      : {
          type: "key" as const,
          privateKey: model.privateKey,
          passphrase: model.passphrase || null,
        };
  return {
    id: editingId.value,
    name: model.name.trim(),
    host: model.host.trim(),
    port: model.port,
    username: model.username.trim(),
    auth,
    group: model.group,
    tags: model.tags,
    notes: model.notes,
    allowAi: model.allowAi,
  };
}

async function submit() {
  try {
    await formRef.value?.validate();
  } catch {
    return;
  }
  if (model.authType === "password" && !model.password) {
    message.error("密码不能为空");
    return;
  }
  if (model.authType === "key" && !model.privateKey.trim()) {
    message.error("私钥内容不能为空");
    return;
  }
  saving.value = true;
  try {
    await store.save(buildInput());
    message.success(editingId.value ? "服务器已更新" : "服务器已添加");
    showModal.value = false;
  } catch (e) {
    message.error(String(e));
  } finally {
    saving.value = false;
  }
}

async function removeRow(row: ServerConfig) {
  try {
    await store.remove(row.id);
    message.success(`已删除「${row.name}」`);
  } catch (e) {
    message.error(String(e));
  } finally {
    refreshConnStatus();
  }
}

async function testForm() {
  try {
    const r = await api.testConnection(model.host.trim(), model.port);
    if (r.ok) message.success(r.message);
    else message.error(r.message);
  } catch (e) {
    message.error(String(e));
  }
}

// 从文件导入私钥（~/.ssh/id_ed25519、id_rsa、*.pem 等）
const importingKey = ref(false);
async function pickKeyFile() {
  importingKey.value = true;
  try {
    const path = await open({
      title: "选择私钥文件",
      multiple: false,
      // 不按后缀过滤：id_ed25519 / id_rsa 等默认私钥无后缀；选错内容由后端 PRIVATE KEY 标记校验兜底
    });
    if (typeof path !== "string") return;
    const content = await api.readKeyFile(path);
    model.privateKey = content;
    message.success(
      `已导入私钥：${path.split("/").pop()}（如有口令请在下方填写）`
    );
  } catch (e) {
    message.error(String(e));
  } finally {
    importingKey.value = false;
  }
}

// ---------- 文件传输（SFTP） ----------
const filesModal = ref(false);
const filesTarget = ref<ServerConfig | null>(null);
const uploadRemoteDir = ref("/root/");
const uploading = ref(false);
const downloadRemotePath = ref("");
const downloading = ref(false);

function openFiles(row: ServerConfig) {
  filesTarget.value = row;
  uploadRemoteDir.value = "/root/";
  downloadRemotePath.value = "";
  filesModal.value = true;
}

function joinRemotePath(dir: string, filename: string): string {
  const d = dir.trim();
  const sep = d.endsWith("/") ? "" : "/";
  return `${d}${sep}${filename}`;
}

async function doUpload() {
  if (!filesTarget.value) return;
  const picked = await open({ title: "选择要上传的本地文件", multiple: false });
  if (typeof picked !== "string") return;
  const filename = picked.split("/").pop() ?? "file";
  const remote = joinRemotePath(uploadRemoteDir.value, filename);
  uploading.value = true;
  try {
    const bytes = await api.sftpUpload(filesTarget.value.id, picked, remote);
    message.success(`已上传 ${filename} → ${remote}（${bytes} 字节）`);
  } catch (e) {
    message.error(String(e));
  } finally {
    uploading.value = false;
  }
}

async function doDownload() {
  if (!filesTarget.value || !downloadRemotePath.value.trim()) {
    message.error("请输入要下载的远程文件路径");
    return;
  }
  const remote = downloadRemotePath.value.trim();
  const suggested = remote.split("/").pop() ?? "download";
  const save = await save$({
    title: "选择保存位置",
    defaultPath: suggested,
  });
  if (typeof save !== "string") return;
  downloading.value = true;
  try {
    const bytes = await api.sftpDownload(filesTarget.value.id, remote, save);
    message.success(`已下载 ${remote} → ${save}（${bytes} 字节）`);
  } catch (e) {
    message.error(String(e));
  } finally {
    downloading.value = false;
  }
}
</script>

<template>
  <div class="page">
    <div class="toolbar">
      <n-input
        v-model:value="keyword"
        placeholder="搜索名称 / 主机 / 用户名 / 标签"
        clearable
        style="width: 300px"
      />
      <n-select
        v-model:value="groupFilter"
        :options="groupOptions"
        placeholder="全部分组"
        clearable
        style="width: 170px"
      />
      <div style="flex: 1" />
      <n-button :loading="store.loading" @click="store.load()">刷新</n-button>
      <n-button type="primary" @click="openCreate">＋ 新增服务器</n-button>
    </div>

    <n-alert v-if="store.loadError" type="error" style="margin-bottom: 2px">
      <div style="font-size: 12px; white-space: pre-wrap; line-height: 1.7">
        {{ store.loadError }}
      </div>
    </n-alert>

    <n-data-table
      :columns="columns"
      :data="filtered"
      :loading="store.loading"
      :bordered="false"
      size="small"
      :row-key="rowKey"
      :scroll-x="820"
    />

    <n-modal
      v-model:show="showModal"
      preset="card"
      :title="editingId ? '编辑服务器' : '新增服务器'"
      style="width: 660px"
    >
      <n-form
        ref="formRef"
        :model="model"
        :rules="rules"
        label-placement="left"
        label-width="96"
      >
        <n-form-item label="名称" path="name">
          <n-input v-model:value="model.name" placeholder="如：生产-网关-01" />
        </n-form-item>
        <n-form-item label="主机" path="host">
          <n-input v-model:value="model.host" placeholder="IP 或域名" />
        </n-form-item>
        <n-form-item label="端口" path="port">
          <n-input-number
            v-model:value="model.port"
            :min="1"
            :max="65535"
            style="width: 150px"
          />
        </n-form-item>
        <n-form-item label="用户名" path="username">
          <n-input v-model:value="model.username" placeholder="如：root" />
        </n-form-item>
        <n-form-item label="认证方式">
          <n-radio-group v-model:value="model.authType">
            <n-radio-button value="password">密码</n-radio-button>
            <n-radio-button value="key">SSH 私钥</n-radio-button>
          </n-radio-group>
        </n-form-item>
        <n-form-item v-if="model.authType === 'password'" label="密码">
          <n-input
            v-model:value="model.password"
            type="password"
            show-password-on="click"
            placeholder="登录密码"
          />
        </n-form-item>
        <template v-else>
          <n-form-item label="私钥">
            <div style="width: 100%">
              <n-input
                v-model:value="model.privateKey"
                type="textarea"
                :rows="6"
                placeholder="粘贴私钥内容，或点击下方按钮从文件导入"
              />
              <div style="margin-top: 6px">
                <n-button size="small" :loading="importingKey" @click="pickKeyFile">
                  选择私钥文件…
                </n-button>
                <span class="hint">支持 id_ed25519 / id_rsa / *.pem 等 OpenSSH/PEM 明文私钥</span>
              </div>
            </div>
          </n-form-item>
          <n-form-item label="私钥口令">
            <n-input
              v-model:value="model.passphrase"
              type="password"
              show-password-on="click"
              placeholder="无则留空"
            />
          </n-form-item>
        </template>
        <n-form-item label="分组">
          <n-select
            v-model:value="model.group"
            :options="groupOptions"
            tag
            filterable
            clearable
            placeholder="选择或输入新分组"
          />
        </n-form-item>
        <n-form-item label="标签">
          <n-select
            v-model:value="model.tags"
            :options="[]"
            tag
            filterable
            multiple
            placeholder="输入后回车添加"
          />
        </n-form-item>
        <n-form-item label="允许 AI">
          <n-switch v-model:value="model.allowAi" />
          <span class="hint">关闭后 AI 工具不可操作此服务器</span>
        </n-form-item>
        <n-form-item label="备注">
          <n-input v-model:value="model.notes" type="textarea" :rows="2" />
        </n-form-item>
      </n-form>
      <template #footer>
        <div style="display: flex; gap: 8px; justify-content: flex-end">
          <n-button :disabled="!model.host" @click="testForm">测试 TCP 连接</n-button>
          <n-button @click="showModal = false">取消</n-button>
          <n-button type="primary" :loading="saving" @click="submit">保存</n-button>
        </div>
      </template>
    </n-modal>

    <n-modal
      v-model:show="execModal"
      preset="card"
      :title="execTarget ? `执行命令 · ${execTarget.name}` : '执行命令'"
      style="width: 760px"
    >
      <div style="display: flex; gap: 8px">
        <n-input
          v-model:value="execCommand"
          placeholder="如：df -h /"
          @keyup.enter="runExec"
        />
        <n-button type="primary" :loading="execRunning" @click="runExec">
          执行（60s 超时）
        </n-button>
      </div>
      <p class="hint" style="margin: 8px 0 0">
        手动执行记录会写入审计日志；AI Agent 调用会经过高危命令拦截与审批管线。
      </p>
      <div v-if="execOutput" class="execout">
        <div class="execline">
          <n-tag size="small" :type="execOutput.exitCode === 0 ? 'success' : 'error'" :bordered="false">
            exit = {{ execOutput.exitCode ?? "无" }}
          </n-tag>
          <n-tag v-if="execOutput.truncated" size="small" type="warning" :bordered="false">
            输出已截断
          </n-tag>
        </div>
        <pre v-if="execOutput.stdout" class="pre">stdout：
{{ execOutput.stdout }}</pre>
        <pre v-if="execOutput.stderr" class="pre preerr">stderr：
{{ execOutput.stderr }}</pre>
      </div>
    </n-modal>

    <n-modal
      v-model:show="filesModal"
      preset="card"
      :title="filesTarget ? `文件传输（SFTP） · ${filesTarget.name}` : '文件传输'"
      style="width: 640px"
    >
      <n-card size="small" title="上传：本地 → 服务器" style="margin-bottom: 14px">
        <div style="display: flex; gap: 8px; align-items: center">
          <span style="flex-shrink: 0; font-size: 13px; opacity: 0.8">远端目录</span>
          <n-input v-model:value="uploadRemoteDir" placeholder="/root/" />
          <n-button type="primary" :loading="uploading" @click="doUpload">
            选择文件并上传
          </n-button>
        </div>
        <p class="hint" style="margin: 8px 0 0">
          文件将以原文件名传到远端目录；同名文件会被覆盖。上限 512MB。
        </p>
      </n-card>
      <n-card size="small" title="下载：服务器 → 本地">
        <div style="display: flex; gap: 8px; align-items: center">
          <span style="flex-shrink: 0; font-size: 13px; opacity: 0.8">远端文件</span>
          <n-input
            v-model:value="downloadRemotePath"
            placeholder="如 /var/log/nginx/access.log"
            @keyup.enter="doDownload"
          />
          <n-button type="primary" :loading="downloading" @click="doDownload">
            保存到本地…
          </n-button>
        </div>
        <p class="hint" style="margin: 8px 0 0">
          上限 1GB。所有传输都会写入审计日志。
        </p>
      </n-card>
    </n-modal>
  </div>
</template>

<style scoped>
.execout {
  margin-top: 12px;
}
.execline {
  display: flex;
  gap: 6px;
  margin-bottom: 8px;
}
.pre {
  background: rgba(255, 255, 255, 0.04);
  border-radius: 6px;
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.6;
  max-height: 320px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 6px 0;
}
.preerr {
  color: #ff8a8a;
}
</style>
