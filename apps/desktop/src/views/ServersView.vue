<script setup lang="ts">
import { computed, h, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDescriptions,
  NDescriptionsItem,
  NDropdown,
  NEmpty,
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
  NTree,
  useDialog,
  useMessage,
  type TreeOption,
  type DropdownOption,
  type FormInst,
  type FormRules,
} from "naive-ui";
import { EllipsisVertical, FolderOpenOutline } from "@vicons/ionicons5";
import { api, type SaveServerInput, type ServerConfig } from "../api";
import { open, save as save$ } from "@tauri-apps/plugin-dialog";
import { useServersStore } from "../stores/servers";
import { useActivityStore } from "../stores/activity";

const store = useServersStore();
const activity = useActivityStore();
const message = useMessage();
const dialog = useDialog();

/** AI 实时活动小窗：可折叠（默认展开） */
const showActivity = ref(true);

// ---------- 行内下拉操作菜单 ----------
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

// ---------- 连接状态（三态：绿=已连接，红=最近一次连接失败，灰=未连接） ----------
type ConnState = "connected" | "error" | "disconnected";
const connMap = ref<Record<string, ConnState>>({});
/**
 * 最近一次连接/SSH 测试失败的服务器。连接池只有「在/不在」两种状态，
 * 失败不会留痕，所以红点由前端记忆：保持到下一次连接成功、主动断开或删除服务器。
 */
const connectFailures = new Set<string>();
const connectingId = ref<string | null>(null);
let statusTimer: number | undefined;

async function refreshConnStatus() {
  try {
    const list = await api.sshStatus();
    const map: Record<string, ConnState> = {};
    for (const c of list)
      map[c.serverId] = c.status === "connected" ? "connected" : "disconnected";
    for (const id of connectFailures) {
      if (map[id] && map[id] !== "connected") map[id] = "error";
    }
    connMap.value = map;
  } catch {
    /* 锁定状态下静默 */
  }
}

const DOT_STYLES: Record<ConnState, string> = {
  connected: "background:#63c28a;box-shadow:0 0 0 3px rgba(99,194,138,.15)",
  error: "background:#e88080;box-shadow:0 0 0 3px rgba(232,128,128,.15)",
  disconnected: "background:transparent;border:1.5px solid rgba(255,255,255,.35)",
};
const dotStyle = (id: string) => DOT_STYLES[connMap.value[id] ?? "disconnected"];

onMounted(() => {
  store.load().catch((e) => message.error(String(e)));
  refreshConnStatus();
  statusTimer = window.setInterval(refreshConnStatus, 5000);
});
onUnmounted(() => {
  if (statusTimer) window.clearInterval(statusTimer);
});

// ---------- 左侧树（只显示名称） ----------
const keyword = ref("");
const DEFAULT_GROUP = "默认分组";
const UNGROUPED_KEY = "g:__ungrouped__";

const filteredServers = computed(() =>
  store.servers.filter((s) => {
    const kw = keyword.value.trim().toLowerCase();
    return (
      !kw ||
      [s.name, s.host, s.username, s.group ?? "", ...(s.tags ?? [])]
        .join(" ")
        .toLowerCase()
        .includes(kw)
    );
  })
);

/** 分组 → 该组服务器（未分组用独立键，不会被真实的「默认分组」合并） */
const groupings = computed(() => {
  const groups = new Map<string, ServerConfig[]>();
  let ungrouped: ServerConfig[] = [];
  for (const s of filteredServers.value) {
    const g = (s.group ?? "").trim();
    if (!g) {
      ungrouped.push(s);
      continue;
    }
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g)!.push(s);
  }
  return { groups, ungrouped };
});

const treeData = computed<TreeOption[]>(() => {
  const { groups, ungrouped } = groupings.value;
  const nodes: TreeOption[] = [];
  const names = [...groups.keys()].sort((a, b) => a.localeCompare(b, "zh-CN"));
  for (const g of names) {
    const children = groups.get(g)!;
    nodes.push({
      key: `g:${g}`,
      label: `${g}（${children.length}）`,
      prefix: () =>
        h(NIcon, { size: 15, color: "#e2b93b" }, { default: () => h(FolderOpenOutline) }),
      isGroupNode: true,
      groupName: g,
      children: children.map((s) => serverNode(s)),
    });
  }
  if (ungrouped.length) {
    nodes.push({
      key: UNGROUPED_KEY,
      label: `${DEFAULT_GROUP}（${ungrouped.length}）`,
      prefix: () =>
        h(NIcon, { size: 15, color: "#e2b93b" }, { default: () => h(FolderOpenOutline) }),
      isGroupNode: true,
      groupName: null,
      children: ungrouped.map((s) => serverNode(s)),
    });
  }
  return nodes;
});

function serverNode(s: ServerConfig): TreeOption {
  return {
    key: s.id,
    label: s.name,
    // 状态点：绿=已连接，红=最近一次连接失败，灰=未连接
    prefix: () => h("span", { class: "srv-dot", style: dotStyle(s.id) }),
    isServerNode: true,
    server: s,
  };
}

/** key → 节点附加信息（选中后取详情用） */
const nodeInfo = computed(() => {
  const map = new Map<
    string,
    { server?: ServerConfig; groupName?: string | null; isGroupNode?: boolean }
  >();
  const walk = (nodes: TreeOption[]) => {
    for (const n of nodes) {
      const info = n as TreeOption & {
        server?: ServerConfig;
        groupName?: string | null;
        isGroupNode?: boolean;
      };
      map.set(String(n.key), {
        server: info.server,
        groupName: info.groupName,
        isGroupNode: info.isGroupNode,
      });
      if (n.children) walk(n.children);
    }
  };
  walk(treeData.value);
  return map;
});

const selectedKey = ref<string | null>(null);
const selectedKeys = computed<string[]>(() =>
  selectedKey.value ? [selectedKey.value] : []
);
const selectedServer = computed(() => {
  if (!selectedKey.value) return null;
  return nodeInfo.value.get(selectedKey.value)?.server ?? null;
});
const selectedGroup = computed(() => {
  if (!selectedKey.value) return null;
  const info = nodeInfo.value.get(selectedKey.value);
  if (!info?.isGroupNode) return null;
  const { groupName } = info;
  const { groups, ungrouped } = groupings.value;
  const members = groupName === null ? ungrouped : groups.get(groupName ?? "") ?? [];
  return { name: groupName ?? DEFAULT_GROUP, isUngrouped: groupName === null, members };
});

function onSelect(keys: string[]) {
  selectedKey.value = keys[0] ?? null;
}

const expandedKeys = ref<string[]>([]);
const knownGroups = new Set<string>();
// 保留用户已有的折叠状态：搜索/刷新导致数据重算时，只有「新出现的分组」默认展开
watch(
  treeData,
  (nodes) => {
    const keys = nodes.map((n) => String(n.key));
    const prev = new Set(expandedKeys.value);
    expandedKeys.value = keys.filter((k) => prev.has(k) || !knownGroups.has(k));
    for (const k of keys) knownGroups.add(k);
    // 数据加载完成且尚未选中时，默认选中第一台服务器
    if (!selectedKey.value) {
      const first = nodes[0]?.children?.[0];
      if (first) selectedKey.value = String(first.key);
    }
  },
  { immediate: true }
);

// 删除/搜索后选中的服务器消失 → 清空选中（右侧回空态）
watch(
  () => store.servers.length,
  () => {
    if (selectedKey.value && !nodeInfo.value.has(selectedKey.value)) {
      selectedKey.value = null;
    }
  }
);

const groupOptions = computed(() => store.groups.map((g) => ({ label: g, value: g })));

// ---------- 连接 / 测试 / 执行 ----------
async function connectRow(row: ServerConfig) {
  connectingId.value = row.id;
  try {
    if (connMap.value[row.id] === "connected") {
      await api.sshDisconnect(row.id);
      connectFailures.delete(row.id); // 主动断开 → 灰点
      message.info(`「${row.name}」已断开`);
    } else {
      try {
        await api.sshConnect(row.id);
        connectFailures.delete(row.id);
        message.success(`「${row.name}」已连接（保活 30s）`);
      } catch (e) {
        connectFailures.add(row.id); // 连接失败 → 红点保持
        message.error(`「${row.name}」连接失败：${String(e)}`);
      }
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
    if (r.ok) {
      connectFailures.delete(row.id);
      message.success(`「${row.name}」${r.message}`);
    } else {
      connectFailures.add(row.id);
      message.error(`「${row.name}」${r.message}`);
    }
  } catch (e) {
    connectFailures.add(row.id);
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
const execOutput = ref<{ stdout: string; stderr: string; exitCode: number | null; truncated: boolean; timedOut?: boolean } | null>(null);

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

function openCreate(group: string | null = null) {
  Object.assign(model, emptyModel(), { group: group ?? null });
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
    const saved = await store.save(buildInput());
    message.success(editingId.value ? "服务器已更新" : "服务器已添加");
    showModal.value = false;
    // 新增后直接选中新服务器，详情面板立即可见
    if (!editingId.value && saved) selectedKey.value = saved.id;
  } catch (e) {
    message.error(String(e));
  } finally {
    saving.value = false;
  }
}

async function removeRow(row: ServerConfig) {
  connectFailures.delete(row.id);
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

// ---------- 详情面板辅助 ----------
function fmtTs(ts: string | null): string {
  if (!ts) return "—";
  const d = new Date(ts);
  return isNaN(+d) ? ts : d.toLocaleString("zh-CN", { hour12: false });
}

const authLabel = (s: ServerConfig) =>
  s.auth.type === "password"
    ? "密码"
    : `SSH 私钥${s.auth.passphrase ? "（带口令）" : ""}`;

const selectedConnState = computed<ConnState>(() =>
  selectedServer.value ? (connMap.value[selectedServer.value.id] ?? "disconnected") : "disconnected"
);
</script>

<template>
  <div class="page">
    <div class="toolbar">
      <span style="font-size: 13px; opacity: 0.7">服务器资产</span>
      <div style="flex: 1" />
      <n-button :loading="store.loading" @click="store.load()">刷新</n-button>
      <n-button type="primary" @click="openCreate()">＋ 新增服务器</n-button>
    </div>

    <n-alert v-if="store.loadError" type="error" style="margin-bottom: 2px">
      <div style="font-size: 12px; white-space: pre-wrap; line-height: 1.7">
        {{ store.loadError }}
      </div>
    </n-alert>

    <div class="servers-layout">
      <!-- 左：分组树（只显示名称） -->
      <aside class="tree-pane">
        <n-input
          v-model:value="keyword"
          size="small"
          placeholder="搜索名称 / 主机 / 标签"
          clearable
        />
        <div class="tree-scroll">
          <n-tree
            v-if="treeData.length"
            block-line
            :data="treeData"
            :selected-keys="selectedKeys"
            :expanded-keys="expandedKeys"
            @update:selected-keys="onSelect"
            @update:expanded-keys="(k: any) => (expandedKeys = k)"
          />
          <n-empty
            v-else
            :description="keyword ? '没有匹配的服务器' : '还没有服务器'"
            size="small"
            style="margin-top: 48px"
          />
        </div>
        <div class="tree-foot">
          {{ store.servers.length }} 台 · {{ treeData.length }} 个分组
        </div>
      </aside>

      <!-- 右：详情面板 -->
      <section class="detail-pane">
        <n-card
          v-if="selectedServer"
          size="small"
          class="detail-card"
        >
          <template #header>
            <div class="detail-head">
              <span class="srv-dot big" :style="dotStyle(selectedServer.id)" />
              <span class="detail-name">{{ selectedServer.name }}</span>
              <n-tag
                size="small"
                :type="selectedConnState === 'connected'
                  ? 'success'
                  : selectedConnState === 'error'
                    ? 'error'
                    : 'default'"
                :bordered="false"
              >
                {{ selectedConnState === "connected"
                  ? "已连接"
                  : selectedConnState === "error"
                    ? "连接失败"
                    : "未连接" }}
              </n-tag>
              <n-tag
                v-if="!selectedServer.allowAi"
                size="small"
                type="warning"
                :bordered="false"
              >
                AI 禁止
              </n-tag>
              <n-tag
                v-for="t in selectedServer.tags"
                :key="t"
                size="small"
                :bordered="false"
              >
                {{ t }}
              </n-tag>
            </div>
          </template>
          <template #header-extra>
            <n-dropdown
              :options="actionOptions"
              trigger="click"
              @select="(key: string) => handleAction(key, selectedServer!)"
            >
              <n-button size="small" circle quaternary>
                <template #icon>
                  <n-icon><EllipsisVertical /></n-icon>
                </template>
              </n-button>
            </n-dropdown>
          </template>

          <div class="detail-sub">
            <code class="mono">{{ selectedServer.host }}:{{ selectedServer.port }}</code>
            <span class="mono" style="opacity: 0.7">{{ selectedServer.username }}</span>
          </div>

          <div class="detail-actions">
            <n-button
              :type="selectedConnState === 'connected' ? 'warning' : 'primary'"
              :loading="connectingId === selectedServer.id"
              @click="connectRow(selectedServer)"
            >
              {{ selectedConnState === "connected" ? "断开连接" : "连接" }}
            </n-button>
            <n-button @click="openExec(selectedServer)">执行命令</n-button>
            <n-button @click="openFiles(selectedServer)">文件传输</n-button>
            <n-button @click="openEdit(selectedServer)">编辑</n-button>
            <n-button @click="testRow(selectedServer)">SSH 测试</n-button>
          </div>

          <n-descriptions
            label-placement="left"
            bordered
            size="small"
            :column="1"
            style="margin-top: 14px"
          >
            <n-descriptions-item label="主机">
              <code class="mono">{{ selectedServer.host }}</code>
            </n-descriptions-item>
            <n-descriptions-item label="端口">
              {{ selectedServer.port }}
            </n-descriptions-item>
            <n-descriptions-item label="用户名">
              {{ selectedServer.username }}
            </n-descriptions-item>
            <n-descriptions-item label="认证方式">
              {{ authLabel(selectedServer) }}
            </n-descriptions-item>
            <n-descriptions-item label="分组">
              {{ selectedServer.group ?? "默认分组" }}
            </n-descriptions-item>
            <n-descriptions-item label="允许 AI">
              {{ selectedServer.allowAi ? "允许" : "禁止" }}
            </n-descriptions-item>
            <n-descriptions-item label="备注">
              <span style="white-space: pre-wrap">{{ selectedServer.notes || "—" }}</span>
            </n-descriptions-item>
            <n-descriptions-item label="创建时间">
              {{ fmtTs(selectedServer.createdAt) }}
            </n-descriptions-item>
            <n-descriptions-item label="更新时间">
              {{ fmtTs(selectedServer.updatedAt) }}
            </n-descriptions-item>
          </n-descriptions>

          <p class="hint" style="margin: 12px 0 0">
            连接保持 30s 心跳；手动执行与文件传输都会写入审计日志。
          </p>
        </n-card>

        <!-- 选中分组：展示组内成员 -->
        <n-card v-else-if="selectedGroup" size="small" class="detail-card">
          <template #header>
            <div class="detail-head">
              <n-icon :size="16" color="#e2b93b"><FolderOpenOutline /></n-icon>
              <span class="detail-name">{{ selectedGroup.name }}</span>
              <n-tag size="small" :bordered="false">
                {{ selectedGroup.members.length }} 台
              </n-tag>
            </div>
          </template>
          <div class="member-list">
            <div
              v-for="m in selectedGroup.members"
              :key="m.id"
              class="member-row"
              @click="selectedKey = m.id"
            >
              <span class="srv-dot" :style="dotStyle(m.id)" />
              <span>{{ m.name }}</span>
              <span class="mono" style="opacity: 0.55; margin-left: auto">
                {{ m.host }}:{{ m.port }}
              </span>
            </div>
          </div>
          <div class="detail-actions">
            <n-button
              type="primary"
              @click="openCreate(selectedGroup.isUngrouped ? null : selectedGroup.name)"
            >
              ＋ 新增到该分组
            </n-button>
          </div>
        </n-card>

        <div v-else class="detail-empty">
          <n-empty description="从左侧选择一台服务器查看详情" style="margin-top: 96px" />
        </div>
      </section>
    </div>

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
          <n-tag v-if="execOutput.timedOut" size="small" type="error" :bordered="false">
            读取超时（远端可能仍在执行）
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

    <!-- AI 实时活动小窗：哪个服务器正在被调用什么工具（事件流驱动，右下角可折叠） -->
    <div class="activity-widget">
      <div class="activity-head" @click="showActivity = !showActivity">
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
  </div>
</template>

<style scoped>
.servers-layout {
  display: flex;
  gap: 14px;
  flex: 1;
  min-height: 0;
}

/* AI 实时活动小窗：右下角悬浮，可折叠 */
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

/* 左：树面板 */
.tree-pane {
  width: 250px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  padding: 10px;
  background: rgba(255, 255, 255, 0.02);
}
.tree-scroll {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.tree-foot {
  font-size: 12px;
  opacity: 0.5;
  text-align: center;
}

/* 右：详情面板 */
.detail-pane {
  flex: 1;
  min-width: 0;
  min-height: 0;
  overflow: auto;
}
.detail-card {
  max-width: 760px;
}
.detail-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.detail-name {
  font-size: 17px;
  font-weight: 700;
}
.detail-sub {
  display: flex;
  gap: 14px;
  align-items: center;
  margin-top: 2px;
  font-size: 13px;
}
.detail-actions {
  display: flex;
  gap: 8px;
  margin-top: 12px;
  flex-wrap: wrap;
}
.mono {
  font-family: ui-monospace, Menlo, monospace;
  font-size: 12.5px;
}
.member-list {
  display: flex;
  flex-direction: column;
}
.member-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
}
.member-row:hover {
  background: rgba(255, 255, 255, 0.05);
}
.detail-empty {
  border: 1px dashed rgba(255, 255, 255, 0.12);
  border-radius: 8px;
  min-height: 320px;
}

/* 状态点 */
.srv-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.srv-dot.big {
  width: 10px;
  height: 10px;
}

/* 让树节点留出状态点的位置间距 */
.tree-scroll :deep(.n-tree-node-content) {
  padding-left: 6px;
}
.tree-scroll :deep(.n-tree-node-content__prefix) {
  margin-right: 8px;
}

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
