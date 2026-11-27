<script setup lang="ts">
import { computed, h, nextTick, onActivated, onDeactivated, reactive, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCard,
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
  type TreeDropInfo,
  type TreeOption,
  type DropdownOption,
  type FormInst,
  type FormRules,
} from "naive-ui";
import { FolderOpenOutline, RefreshOutline, SearchOutline, ServerOutline } from "@vicons/ionicons5";
import { api, type LayoutItem, type SaveServerInput, type ServerConfig } from "../api";
import { open, save as save$ } from "@tauri-apps/plugin-dialog";
import { matchesServer, useServersStore } from "../stores/servers";

const store = useServersStore();
const message = useMessage();
const dialog = useDialog();


const props = defineProps<{ focusRequest?: { id: string } | null }>();
const emit = defineEmits<{ focusHandled: [] }>();
const detailTitle = ref<HTMLElement | null>(null);

const actionOptions = computed<DropdownOption[]>(() => {
  const connected = selectedConnState.value === "connected";
  return [
    { label: "SSH 测试", key: "test" },
    ...(connected
      ? [{ label: "断开连接", key: "disconnect", disabled: selectedServer.value ? disconnectingIds.has(selectedServer.value.id) : false }]
      : [{ label: "执行命令", key: "exec" }, { label: "文件传输", key: "files" }]),
    { label: "组内上移", key: "move-up", disabled: !canMove(-1) },
    { label: "组内下移", key: "move-down", disabled: !canMove(1) },
    { label: "重置指纹", key: "reset" },
    { type: "divider", key: "divider" },
    { label: "删除服务器", key: "delete", props: { style: { color: "var(--danger)" } } },
  ];
});

function handleAction(key: string, row: ServerConfig) {
  switch (key) {
    case "test": void testRow(row); break;
    case "disconnect": void disconnectRow(row); break;
    case "exec": openExec(row); break;
    case "files": openFiles(row); break;
    case "reset": confirmResetHostkey(row); break;
    case "delete": confirmRemove(row); break;
    case "move-up": void moveWithinGroup(row, -1); break;
    case "move-down": void moveWithinGroup(row, 1); break;
  }
}

type ConnState = "connected" | "error" | "disconnected";
const connMap = ref<Record<string, ConnState>>({});
const connectFailures = reactive(new Map<string, string>());
const connectingIds = reactive(new Set<string>());
const disconnectingIds = reactive(new Set<string>());
const justConnected = ref<string | null>(null);
const connLabels = {
  connected: "已连接",
  error: "连接失败",
  disconnected: "未连接",
  connecting: "连接中",
};
let statusTimer: number | undefined;
let statusVersion = 0;

async function refreshConnStatus() {
  const version = ++statusVersion;
  try {
    const list = await api.sshStatus();
    if (version !== statusVersion) return;
    const map: Record<string, ConnState> = {};
    for (const c of list) {
      map[c.serverId] = c.status === "connected" ? "connected" : "disconnected";
      if (c.status === "connected") connectFailures.delete(c.serverId);
    }
    for (const id of connectFailures.keys()) {
      if (map[id] !== "connected") map[id] = "error";
    }
    connMap.value = map;
  } catch {
    // 保险库锁定时由主窗口切换到锁屏。
  }
}

function connState(id: string) {
  if (connectingIds.has(id)) return "connecting";
  return connMap.value[id] ?? "disconnected";
}

function markConn(id: string, state: ConnState) {
  statusVersion++;
  connMap.value = { ...connMap.value, [id]: state };
}

onActivated(() => {
  if (!savingLayout.value) void store.load();
  void refreshConnStatus();
  statusTimer = window.setInterval(refreshConnStatus, 1500);
  window.addEventListener("focus", refreshConnStatus);
  document.addEventListener("visibilitychange", onVisibleRefresh);
});
onDeactivated(() => {
  statusVersion++;
  window.clearInterval(statusTimer);
  window.removeEventListener("focus", refreshConnStatus);
  document.removeEventListener("visibilitychange", onVisibleRefresh);
  showModal.value = false;
  Object.assign(model, emptyModel());
  renameModal.value = false;
  execModal.value = false;
  filesModal.value = false;
});

function onVisibleRefresh() {
  if (document.visibilityState === "visible") void refreshConnStatus();
}

const keyword = ref("");
const failedOnly = ref(false);
const DEFAULT_GROUP = "默认分组";
const UNGROUPED_KEY = "g:__ungrouped__";
const filtering = computed(() => Boolean(keyword.value.trim() || failedOnly.value));
const connectedCount = computed(() => store.servers.filter(({ id }) => connState(id) === "connected").length);
const failedCount = computed(() => store.servers.filter(({ id }) => connState(id) === "error").length);
const filteredServers = computed(() => store.servers.filter((server) =>
  (!failedOnly.value || connState(server.id) === "error") && matchesServer(server, keyword.value)
));

/** 树节点上挂的业务字段（naive-ui 的 TreeOption 允许透传自定义字段） */
type NodeExt = TreeOption & {
  isServerNode?: boolean;
  isGroupNode?: boolean;
  server?: ServerConfig;
  groupName?: string | null;
};

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
        h(NIcon, { size: 15, color: "var(--muted)" }, { default: () => h(FolderOpenOutline) }),
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
        h(NIcon, { size: 15, color: "var(--muted)" }, { default: () => h(FolderOpenOutline) }),
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
    prefix: () => h("span", { class: `srv-dot s-${connState(s.id)}`, "aria-hidden": "true" }),
    suffix: () => h("span", { class: "tree-status" }, connLabels[connState(s.id)]),
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
      const info = n as NodeExt;
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
const selectedServer = computed(() => store.servers.find((server) => server.id === selectedKey.value) ?? null);
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
const collapsedGroups = new Set<string>();
function updateExpanded(keys: Array<string | number>) {
  expandedKeys.value = keys.map(String);
  for (const node of treeData.value) {
    const key = String(node.key);
    if (expandedKeys.value.includes(key)) collapsedGroups.delete(key);
    else collapsedGroups.add(key);
  }
}
// 保留用户已有的折叠状态：搜索/刷新导致数据重算时，只有「新出现的分组」默认展开
watch(
  treeData,
  (nodes) => {
    const keys = nodes.map((n) => String(n.key));
    expandedKeys.value = keys.filter((key) => filtering.value || !collapsedGroups.has(key));
    // 数据加载完成且尚未选中时，默认选中第一台服务器
    if (!selectedKey.value) {
      const first = nodes[0]?.children?.[0];
      if (first) selectedKey.value = String(first.key);
    }
  },
  { immediate: true }
);

watch(() => store.servers.length, () => {
  if (selectedKey.value && !selectedServer.value && !nodeInfo.value.has(selectedKey.value)) {
    selectedKey.value = null;
  }
});

watch(() => props.focusRequest, async (request) => {
  if (!request) return;
  emit("focusHandled");
  keyword.value = "";
  failedOnly.value = false;
  const server = store.servers.find(({ id }) => id === request.id);
  if (!server) return;
  const group = normGroup(server.group);
  const key = group ? `g:${group}` : UNGROUPED_KEY;
  collapsedGroups.delete(key);
  selectedKey.value = request.id;
  await nextTick();
  if (!expandedKeys.value.includes(key)) expandedKeys.value.push(key);
  detailTitle.value?.focus();
}, { immediate: true, flush: "post" });

const groupOptions = computed(() => store.groups.map((g) => ({ label: g, value: g })));

// ---------- 拖拽排序 / 拖到其它分组 ----------
const savingLayout = ref(false);

/** 分组名归一：空白 → null（默认分组），与后端 group_name 的语义保持一致 */
function normGroup(group: string | null | undefined): string | null {
  const trimmed = (group ?? "").trim();
  return trimmed ? trimmed : null;
}

/** 树上的可见顺序 → 扁平列表（分组按名称排序、默认分组垫底，与 treeData 一致） */
function flattenedOrder(): ServerConfig[] {
  const { groups, ungrouped } = groupings.value;
  const names = [...groups.keys()].sort((a, b) => a.localeCompare(b, "zh-CN"));
  const flat: ServerConfig[] = [];
  for (const name of names) flat.push(...groups.get(name)!);
  flat.push(...ungrouped);
  return flat;
}

/**
 * 允许拖放：搜索态直接禁止（列表是过滤后的子集，按它算出的顺序会错）。
 * 具体的合法性（只搬服务器、分组不可拖）在 onDrop 里判定——
 * naive-ui 的 allowDrop 回调拿不到 dragNode，这里拦不住「拖分组」。
 */
function allowDrop() {
  return !filtering.value && !savingLayout.value;
}

/** 提交整表新顺序 + 分组归属（后端单事务落库），失败回滚本地状态 */
async function commitLayout(flat: ServerConfig[]) {
  const normalized = flat.map((s) => ({ ...s, group: normGroup(s.group) }));
  const items: LayoutItem[] = normalized.map((s, index) => ({
    id: s.id,
    group: s.group,
    sortOrder: index,
  }));
  const generation = store.generation;
  const snapshot = store.servers.slice();
  // 乐观更新：松手立刻看到新位置，不必等一趟 IPC
  store.servers = normalized;
  savingLayout.value = true;
  try {
    await api.applyServerLayout(items);
  } catch (e) {
    if (generation === store.generation) store.servers = snapshot;
    message.error(`保存排序失败：${String(e)}`);
  } finally {
    savingLayout.value = false;
  }
}

function canMove(step: number) {
  if (!allowDrop() || !selectedServer.value) return false;
  const flat = flattenedOrder();
  const index = flat.findIndex(({ id }) => id === selectedKey.value);
  const target = flat[index + step];
  return Boolean(index >= 0 && target && normGroup(target.group) === normGroup(selectedServer.value.group));
}

async function moveWithinGroup(row: ServerConfig, step: number) {
  if (!canMove(step)) return;
  const flat = flattenedOrder();
  const index = flat.findIndex(({ id }) => id === row.id);
  flat.splice(index, 1);
  flat.splice(index + step, 0, row);
  await commitLayout(flat);
}

/**
 * 落点规则：
 * - 落到分组行里（inside）→ 移到该分组末尾
 * - 落到服务器行前/后 → 在目标所在分组内排序
 * 分组本身不可拖动（按名称排序），搜索态不可拖。
 */
async function onDrop({ node, dragNode, dropPosition }: TreeDropInfo) {
  if (!allowDrop()) return;
  const target = node as NodeExt;
  const moving = (dragNode as NodeExt).server;
  if (!moving || !target) return;

  const flat = flattenedOrder().filter((s) => s.id !== moving.id);
  let group: string | null;
  let insertAt: number;
  if (target.isGroupNode) {
    group = normGroup(target.groupName);
    const members = flat.filter((s) => normGroup(s.group) === group);
    const last = members[members.length - 1];
    insertAt = last ? flat.findIndex((s) => s.id === last.id) + 1 : flat.length;
  } else if (target.server) {
    group = normGroup(target.server.group);
    const index = flat.findIndex((s) => s.id === target.server!.id);
    if (index < 0) return;
    insertAt = dropPosition === "before" ? index : index + 1;
  } else {
    return;
  }

  flat.splice(insertAt, 0, { ...moving, group });
  await commitLayout(flat);
}

// ---------- 分组重命名 ----------
const renameModal = ref(false);
const renaming = ref(false);
const renameTo = ref("");

function openRename() {
  const group = selectedGroup.value;
  if (!group || group.isUngrouped) return;
  renameTo.value = group.name;
  renameModal.value = true;
}

async function submitRename() {
  const group = selectedGroup.value;
  if (!group || group.isUngrouped) return;
  const to = renameTo.value.trim();
  if (!to) {
    message.error("分组名不能为空");
    return;
  }
  if (to === group.name) {
    renameModal.value = false;
    return;
  }
  renaming.value = true;
  try {
    const affected = await api.renameServerGroup(group.name, to);
    message.success(`分组已重命名为「${to}」（${affected} 台服务器）`);
    renameModal.value = false;
    await store.load();
    // 选中改名后的分组节点，避免面板跳空
    selectedKey.value = `g:${to}`;
  } catch (e) {
    message.error(String(e));
  } finally {
    renaming.value = false;
  }
}

// ---------- 连接 / 测试 / 执行 ----------
async function connectRow(row: ServerConfig) {
  if (connectingIds.has(row.id)) return;
  connectingIds.add(row.id);
  try {
    await api.sshConnect(row.id);
    connectFailures.delete(row.id);
    markConn(row.id, "connected");
    justConnected.value = row.id;
    message.success(`「${row.name}」已连接`);
  } catch (e) {
    const error = String(e);
    if (error.includes("连接已取消")) {
      message.info(`「${row.name}」已取消连接`);
    } else {
      connectFailures.set(row.id, error);
      markConn(row.id, "error");
      message.error(`「${row.name}」连接失败`);
    }
  } finally {
    connectingIds.delete(row.id);
    void refreshConnStatus();
  }
}

async function disconnectRow(row: ServerConfig) {
  if (disconnectingIds.has(row.id)) return;
  disconnectingIds.add(row.id);
  try {
    await api.sshDisconnect(row.id);
    connectFailures.delete(row.id);
    markConn(row.id, "disconnected");
    message.info(`「${row.name}」已断开连接`);
  } catch (e) {
    message.error(String(e));
  } finally {
    disconnectingIds.delete(row.id);
    void refreshConnStatus();
  }
}

/** 取消进行中的连接：握手立即中断，connectRow 的 Promise 会以「连接已取消」结束 */
async function cancelConnect(row: ServerConfig) {
  try {
    const ok = await api.sshConnectCancel(row.id);
    if (!ok) message.info("连接已结束，无需取消");
  } catch (e) {
    message.error(String(e));
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
      connectFailures.set(row.id, r.message);
      message.error(`「${row.name}」SSH 测试失败`);
    }
  } catch (e) {
    connectFailures.set(row.id, String(e));
    message.error(`「${row.name}」SSH 测试失败`);
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
  if (execRunning.value || !execTarget.value || !execCommand.value.trim()) return;
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
  if (saving.value) return;
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
    selectedKey.value = saved.id;
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

const selectedConnState = computed(() => selectedServer.value ? connState(selectedServer.value.id) : "disconnected");
</script>

<template>
  <div class="page server-page">
    <div class="toolbar server-toolbar">
      <span class="summary-status"><span class="srv-dot s-connected" aria-hidden="true" />{{ connectedCount }} 台已连接</span>
      <button class="failure-filter" :aria-pressed="failedOnly" @click="failedOnly = !failedOnly">
        <span class="srv-dot" :class="failedCount ? 's-error' : 's-disconnected'" aria-hidden="true" />
        {{ failedCount }} 台连接失败
      </button>
      <div class="toolbar-spacer" />
      <n-button secondary :loading="store.loading" :disabled="savingLayout" @click="store.load()">
        <template #icon><n-icon :component="RefreshOutline" /></template>
        刷新
      </n-button>
      <n-button type="primary" @click="openCreate()">＋ 新增服务器</n-button>
    </div>

    <n-alert v-if="store.loadError" type="error" title="服务器列表加载失败">
      {{ store.loadError }}
    </n-alert>

    <div class="servers-layout">
      <aside class="tree-pane" aria-label="服务器分组">
        <div class="tree-heading"><h2>分组与服务器</h2><span>{{ store.servers.length }} 台</span></div>
        <n-input v-model:value="keyword" size="small" placeholder="搜索名称、地址或标签" clearable
          :input-props="{ 'aria-label': '筛选服务器' }">
          <template #prefix><n-icon :component="SearchOutline" /></template>
        </n-input>
        <div v-if="failedOnly" class="tree-filter">
          <span>仅显示连接失败</span>
          <button @click="failedOnly = false">清除筛选</button>
        </div>
        <div class="tree-scroll">
          <n-tree v-if="treeData.length" block-line :draggable="!filtering"
            :allow-drop="allowDrop" :data="treeData" :selected-keys="selectedKeys" :expanded-keys="expandedKeys"
            @update:selected-keys="onSelect" @update:expanded-keys="updateExpanded" @drop="onDrop" />
          <n-empty v-else size="small" class="tree-empty"
            :description="store.loading ? '正在加载服务器…' : store.loadError ? '请刷新重试' : filtering ? '没有匹配的服务器' : '还没有服务器'" />
        </div>
        <div v-if="savingLayout" class="tree-foot" role="status">正在保存排序…</div>
      </aside>

      <section class="detail-pane" aria-label="服务器详情">
        <n-card v-if="selectedServer" size="small" class="detail-card">
          <div class="detail-identity">
            <div class="server-glyph"><n-icon :component="ServerOutline" size="23" /></div>
            <div class="detail-heading">
              <h2 ref="detailTitle" tabindex="-1" class="detail-name">{{ selectedServer.name }}</h2>
              <div class="detail-sub">
                <code>{{ selectedServer.host }}:{{ selectedServer.port }}</code>
                <span>{{ selectedServer.username }}</span>
              </div>
            </div>
            <n-tag size="small" :type="selectedConnState === 'connected' ? 'success' : selectedConnState === 'error' ? 'error' : 'default'" :bordered="false">
              <span class="inline-status">
                <span class="srv-dot" :class="[`s-${selectedConnState}`, { 'connection-flash': justConnected === selectedServer.id }]"
                  aria-hidden="true" @animationend="justConnected = null" />
                {{ connLabels[selectedConnState] }}
              </span>
            </n-tag>
          </div>
          <div class="detail-actions">
            <n-button v-if="selectedConnState === 'connecting'" @click="cancelConnect(selectedServer)">取消连接</n-button>
            <template v-else-if="selectedConnState === 'connected'">
              <n-button type="primary" @click="openExec(selectedServer)">执行命令</n-button>
              <n-button @click="openFiles(selectedServer)">文件传输</n-button>
            </template>
            <n-button v-else type="primary" @click="connectRow(selectedServer)">
              {{ selectedConnState === "error" ? "重试连接" : "连接" }}
            </n-button>
            <n-dropdown :options="actionOptions" trigger="click" @select="(key: string) => handleAction(key, selectedServer!)">
              <n-button>更多</n-button>
            </n-dropdown>
          </div>
          <n-alert v-if="selectedConnState === 'error'" class="connection-error" type="error" title="连接失败">
            {{ connectFailures.get(selectedServer.id) }}
          </n-alert>
          <div class="detail-section-heading">
            <h3>概览</h3>
            <n-button size="small" quaternary @click="openEdit(selectedServer)">编辑服务器</n-button>
          </div>
          <dl class="detail-facts">
            <div><dt>认证方式</dt><dd>{{ authLabel(selectedServer) }}</dd></div>
            <div><dt>分组</dt><dd>{{ selectedServer.group ?? "默认分组" }}</dd></div>
            <div><dt>AI 权限</dt><dd>{{ selectedServer.allowAi ? "允许" : "禁止" }}</dd></div>
            <div><dt>标签</dt><dd class="server-tags">
              <n-tag v-for="tag in selectedServer.tags" :key="tag" size="small" :bordered="false">{{ tag }}</n-tag>
              <span v-if="!selectedServer.tags.length">—</span>
            </dd></div>
            <div v-if="selectedServer.notes" class="server-notes"><dt>备注</dt><dd>{{ selectedServer.notes }}</dd></div>
          </dl>
          <details class="detail-metadata">
            <summary>资产时间</summary>
            <p>创建 {{ fmtTs(selectedServer.createdAt) }}</p>
            <p>更新 {{ fmtTs(selectedServer.updatedAt) }}</p>
          </details>
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
            <button
              v-for="m in selectedGroup.members"
              :key="m.id"
              class="member-row" type="button"
              @click="selectedKey = m.id"
            >
              <span class="srv-dot" :class="`s-${connState(m.id)}`" />
              <span>{{ m.name }} · {{ connLabels[connState(m.id)] }}</span>
              <span class="mono" style="opacity: 0.55; margin-left: auto">
                {{ m.host }}:{{ m.port }}
              </span>
            </button>
          </div>
          <div class="detail-actions">
            <n-button
              type="primary"
              @click="openCreate(selectedGroup.isUngrouped ? null : selectedGroup.name)"
            >
              ＋ 新增到该分组
            </n-button>
            <n-button v-if="!selectedGroup.isUngrouped" @click="openRename">重命名分组</n-button>
          </div>
          <p class="hint" style="margin: 12px 0 0">
            左侧列表支持拖动：拖到某台服务器前/后 = 组内排序，拖到分组行上 = 移入该分组。
            分组按名称排序，名称可在此重命名。
          </p>
        </n-card>

        <div v-else class="detail-empty">
          <n-empty :description="store.loading ? '正在读取服务器…' : store.loadError ? '暂时无法读取服务器' : store.servers.length ? '从左侧选择一台服务器' : '添加服务器，开始管理连接'">
            <template v-if="!store.servers.length && !store.loading && !store.loadError" #extra>
              <n-button type="primary" @click="openCreate()">新增服务器</n-button>
            </template>
          </n-empty>
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
      v-model:show="renameModal"
      preset="card"
      title="重命名分组"
      style="width: 460px"
    >
      <n-form label-placement="left" label-width="80">
        <n-form-item label="分组名">
          <n-input
            v-model:value="renameTo"
            maxlength="60"
            show-count
            placeholder="如：生产环境"
            @keyup.enter="submitRename"
          />
        </n-form-item>
      </n-form>
      <p class="hint" style="margin: 0">
        改名会批量更新该分组内的所有服务器；与已有分组同名等同于把两个分组合并。
      </p>
      <template #footer>
        <div style="display: flex; gap: 8px; justify-content: flex-end">
          <n-button @click="renameModal = false">取消</n-button>
          <n-button type="primary" :loading="renaming" @click="submitRename">保存</n-button>
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

  </div>
</template>

<style scoped>
.server-page { flex: 1; min-height: 0; }
.server-toolbar { min-height: 36px; }
.toolbar-spacer { flex: 1; }
.summary-status, .failure-filter, .inline-status { display: inline-flex; align-items: center; gap: 7px; }
.summary-status, .failure-filter { font-size: 12px; color: var(--muted); }
.failure-filter {
  background: transparent; border: 1px solid var(--line); border-radius: var(--radius-control);
  padding: 5px 8px; cursor: pointer;
}
.failure-filter:hover, .failure-filter[aria-pressed="true"] { color: var(--text); background: var(--surface); }
.servers-layout { display: grid; grid-template-columns: clamp(220px, 24vw, 285px) minmax(0, 1fr); gap: 14px; flex: 1; min-height: 0; }
.tree-pane {
  display: flex; flex-direction: column; gap: 10px; min-height: 0; padding: 12px;
  border: 1px solid var(--line); border-radius: var(--radius-panel); background: var(--sidebar);
}
.tree-heading { display: flex; align-items: center; justify-content: space-between; color: var(--muted); font-size: 12px; }
.tree-heading h2 { margin: 0; color: var(--text); font-size: 13px; font-weight: 650; }
.tree-scroll { flex: 1; min-height: 0; overflow: auto; }
.tree-empty { padding: 28px 0; }
.tree-filter { display: flex; justify-content: space-between; color: var(--muted); font-size: 12px; }
.tree-filter button { background: transparent; color: var(--accent); border: 0; cursor: pointer; }
.tree-foot { font-size: 12px; color: var(--muted); text-align: center; }
.tree-scroll :deep(.n-tree-node-content) { padding-left: 5px; }
.tree-scroll :deep(.n-tree-node-content__text) { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tree-scroll :deep(.tree-status) { font-size: 10px; color: var(--muted); white-space: nowrap; }
.tree-scroll :deep(.n-tree-node--selected .n-tree-node-content) {
  box-shadow: inset 2px 0 var(--accent);
  animation: tree-select var(--motion-fast) ease-out;
}
@keyframes tree-select { from { box-shadow: inset 0 0 transparent; } }

.detail-pane { min-width: 0; min-height: 0; overflow: auto; }
.detail-card { width: 100%; }
.detail-head, .detail-identity { display: flex; align-items: center; gap: 10px; }
.detail-identity { flex-wrap: wrap; }
.detail-heading { flex: 1; min-width: 0; }
.detail-name { font-size: 20px; font-weight: 680; margin: 0; overflow-wrap: anywhere; }
.detail-sub { display: flex; flex-wrap: wrap; gap: 6px 12px; color: var(--muted); font-size: 12px; overflow-wrap: anywhere; }
.server-glyph {
  display: grid; place-items: center; width: 42px; height: 42px; flex: none;
  border: 1px solid var(--line); border-radius: 9px; color: var(--accent); background: var(--canvas);
}
.detail-actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 16px; }
.connection-error { margin-top: 12px; overflow-wrap: anywhere; }
.detail-section-heading { display: flex; align-items: center; justify-content: space-between; margin-top: 16px; }
.detail-section-heading h3 { margin: 0; font-size: 14px; }
.detail-facts { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); margin: 8px 0 0; }
.detail-facts > div { padding: 8px 8px 8px 0; border-top: 1px solid var(--line); }
.detail-facts dt { color: var(--muted); font-size: 11px; }
.detail-facts dd { margin: 3px 0 0; overflow-wrap: anywhere; }
.server-tags { display: flex; flex-wrap: wrap; gap: 4px; }
.server-notes { grid-column: 1 / -1; white-space: pre-wrap; }
.detail-metadata { border-top: 1px solid var(--line); padding-top: 8px; font-size: 12px; color: var(--muted); }
.detail-metadata summary { cursor: pointer; }
.detail-metadata p { margin: 4px 0; }
.mono { font-family: var(--mono); font-size: 12px; }
.member-list { display: flex; flex-direction: column; }
.member-row {
  display: flex; align-items: center; gap: 8px; padding: 7px 10px; border: none;
  border-radius: var(--radius-control); background: transparent; color: var(--text); cursor: pointer;
  text-align: left;
}
.member-row:hover { background: var(--popover); }
.detail-empty {
  display: grid; place-items: center; min-height: 220px;
  border: 1px dashed var(--line); border-radius: var(--radius-panel);
}

.srv-dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; flex: none; }
.srv-dot.s-connected { background: var(--success); }
.srv-dot.s-error { background: var(--danger); }
.srv-dot.s-connecting { background: var(--accent); }
.srv-dot.s-disconnected { border: 1px solid var(--muted); }
.connection-flash { animation: connection-flash var(--motion-fast) ease-out; }
@keyframes connection-flash {
  from { box-shadow: 0 0 0 0 var(--success); }
  to { box-shadow: 0 0 0 6px transparent; }
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
