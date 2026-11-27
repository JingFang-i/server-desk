<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NButton, NConfigProvider, NDialogProvider, NIcon, NInput, NMessageProvider,
  NModal, NNotificationProvider, NResult, NSpin, NTooltip, darkTheme, dateZhCN,
  zhCN, type GlobalThemeOverrides, type InputInst,
} from "naive-ui";
import {
  ArchiveOutline, CheckmarkCircleOutline, DocumentTextOutline, GitCompareOutline,
  LockClosedOutline, PulseOutline, SearchOutline, ServerOutline, SettingsOutline,
} from "@vicons/ionicons5";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type VaultStatus } from "./api";
import logoUrl from "./assets/logo.png";
import ApprovalAlert from "./components/ApprovalAlert.vue";
import UpdatePrompt from "./components/UpdatePrompt.vue";
import ServersView from "./views/ServersView.vue";
import SettingsView from "./views/SettingsView.vue";
import ApprovalsView from "./views/ApprovalsView.vue";
import AuditView from "./views/AuditView.vue";
import BackupView from "./views/BackupView.vue";
import ChangeLogView from "./views/ChangeLogView.vue";
import LockGate from "./views/LockGate.vue";
import { useActivityStore } from "./stores/activity";
import { matchesServer, useServersStore } from "./stores/servers";

const pages = {
  servers: { label: "服务器", icon: ServerOutline, view: ServersView },
  approvals: { label: "审批中心", icon: CheckmarkCircleOutline, view: ApprovalsView },
  audit: { label: "审计日志", icon: DocumentTextOutline, view: AuditView },
  changes: { label: "变更记录", icon: GitCompareOutline, view: ChangeLogView },
  backup: { label: "备份恢复", icon: ArchiveOutline, view: BackupView },
  settings: { label: "设置", icon: SettingsOutline, view: SettingsView },
};
type PageKey = keyof typeof pages;
const page = ref<PageKey>("servers");
const vault = ref<VaultStatus | null>(null);
const booted = ref(false);
const vaultError = ref(false);
let vaultVersion = 0;
const unlocked = computed(() => vault.value?.unlocked === true);
const approvalCount = ref(0);
const approvalAlert = ref<InstanceType<typeof ApprovalAlert> | null>(null);
const activity = useActivityStore();
const servers = useServersStore();
const latestActivity = computed(() => activity.entries[0]);
const searchOpen = ref(false);
const query = ref("");
const activeResult = ref(0);
const searchInput = ref<InputInst | null>(null);
const focusRequest = ref<{ id: string } | null>(null);

const rootStyle = getComputedStyle(document.documentElement);
const searchShortcut = /Mac/.test(navigator.platform) ? "⌘ K" : "Ctrl K";
const token = (name: string) => rootStyle.getPropertyValue(name).trim();
const theme: GlobalThemeOverrides = {
  common: {
    primaryColor: token("--accent"),
    primaryColorHover: token("--accent-hover"),
    primaryColorPressed: token("--accent-pressed"),
    primaryColorSuppl: token("--accent"),
    successColor: token("--success"),
    warningColor: token("--warning"),
    errorColor: token("--danger"),
    bodyColor: token("--canvas"),
    cardColor: token("--surface"),
    modalColor: token("--surface"),
    popoverColor: token("--popover"),
    textColor1: token("--text"),
    textColor2: token("--text"),
    textColor3: token("--muted"),
    borderColor: token("--line"),
    dividerColor: token("--line"),
    borderRadius: token("--radius-control"),
  },
  Button: { textColorPrimary: token("--canvas") },
  Card: { borderRadius: token("--radius-panel") },
  Drawer: { color: token("--surface") },
  DataTable: { tdColor: token("--surface"), thColor: token("--canvas") },
};

const pageResults = (Object.keys(pages) as PageKey[]).map((key) => ({ key, ...pages[key] }));
const results = computed(() => {
  const keyword = query.value.trim();
  const matchingServers = keyword
    ? servers.servers.filter((server) => matchesServer(server, keyword)).slice(0, 8)
    : [];
  return [
    ...matchingServers.map(({ id, name, host }) => ({ key: id, label: name, hint: host, target: "servers" as const, serverId: id })),
    ...pageResults.filter(({ key, label }) => label.includes(keyword) || key.includes(keyword.toLowerCase()))
      .map(({ key, label }) => ({ key, label, hint: "页面", target: key, serverId: null })),
  ];
});

let selectedSearchResult: (typeof results.value)[number] | null = null;

watch(query, () => { activeResult.value = 0; });
watch(page, (key) => { document.title = `ServerDesk · ${pages[key].label}`; }, { immediate: true });
watch(unlocked, (ready) => {
  if (ready) {
    void activity.init(() => unlocked.value).catch(() => {});
    void approvalAlert.value?.refresh();
  } else {
    activity.clear();
    servers.clear();
    searchOpen.value = false;
    selectedSearchResult = null;
    approvalCount.value = 0;
    query.value = "";
    focusRequest.value = null;
  }
});

const pageBindings = computed(() => {
  if (page.value === "servers") return { focusRequest: focusRequest.value, onFocusHandled: clearFocusRequest };
  if (page.value === "approvals") return { onUpdated: () => approvalAlert.value?.refresh() };
  return {};
});

function clearFocusRequest() {
  focusRequest.value = null;
}

function navigate(key: PageKey) {
  page.value = key;
}

function openSearch() {
  if (!unlocked.value || searchOpen.value || selectedSearchResult) return;
  query.value = "";
  activeResult.value = 0;
  searchOpen.value = true;
  if (page.value !== "servers" && !servers.servers.length && !servers.loading) void servers.load();
}

function chooseResult(result: (typeof results.value)[number]) {
  selectedSearchResult = result;
  searchOpen.value = false;
}

function finishSearch() {
  const result = selectedSearchResult;
  selectedSearchResult = null;
  query.value = "";
  if (!result || !unlocked.value) return;
  if (result.serverId) focusRequest.value = { id: result.serverId };
  navigate(result.target);
}

function onResultKey(event: KeyboardEvent) {
  if (event.isComposing || event.keyCode === 229 || !results.value.length) return;
  const index = Math.min(activeResult.value, results.value.length - 1);
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const step = event.key === "ArrowDown" ? 1 : -1;
    activeResult.value = (index + step + results.value.length) % results.value.length;
    void nextTick(() => document.getElementById(`search-result-${activeResult.value}`)?.scrollIntoView({ block: "nearest" }));
  } else if (event.key === "Enter") {
    event.preventDefault();
    chooseResult(results.value[index]);
  }
}

function onShortcut(event: KeyboardEvent) {
  if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey ||
    event.key.toLowerCase() !== "k" || event.isComposing || event.keyCode === 229 ||
    !unlocked.value || document.activeElement?.closest('[role="dialog"], [aria-modal="true"]')) return;
  event.preventDefault();
  openSearch();
}

async function refreshVault() {
  const version = ++vaultVersion;
  try {
    const status = await api.vaultStatus();
    if (version !== vaultVersion) return;
    vault.value = status;
    vaultError.value = false;
  } catch {
    if (version === vaultVersion && !vault.value) vaultError.value = true;
  } finally {
    if (version === vaultVersion) booted.value = true;
  }
}

async function lockVault() {
  try {
    await api.vaultLock();
    if (vault.value) vault.value = { ...vault.value, unlocked: false };
    void refreshVault();
  } catch {
    void refreshVault();
  }
}

const listeners: UnlistenFn[] = [];
let disposed = false;
let pollTimer: number | undefined;

onMounted(() => {
  void refreshVault();
  window.addEventListener("keydown", onShortcut);
  void Promise.allSettled([
    listen("vault-locked", () => {
      if (vault.value) vault.value = { ...vault.value, unlocked: false };
      void refreshVault();
    }),
    listen("menu-lock-vault", lockVault),
    listen<string>("navigate", ({ payload }) => {
      if (Object.prototype.hasOwnProperty.call(pages, payload)) navigate(payload as PageKey);
    }),
  ]).then((results) => {
    for (const result of results) {
      if (result.status !== "fulfilled") continue;
      if (disposed) result.value();
      else listeners.push(result.value);
    }
  });
  pollTimer = window.setInterval(() => { if (unlocked.value) void refreshVault(); }, 20000);
});

onUnmounted(() => {
  disposed = true;
  window.removeEventListener("keydown", onShortcut);
  listeners.forEach((stop) => stop());
  if (pollTimer) window.clearInterval(pollTimer);
});
</script>

<template>
  <n-config-provider :theme="darkTheme" :theme-overrides="theme" :locale="zhCN" :date-locale="dateZhCN" style="height: 100%">
    <n-message-provider>
      <n-notification-provider placement="top-right" :max="3">
        <ApprovalAlert ref="approvalAlert" :current-page="page" @count="approvalCount = $event" @open="navigate('approvals')" />
        <n-dialog-provider>
          <UpdatePrompt />
          <LockGate v-if="vault && !unlocked" :status="vault" @unlocked="refreshVault" />
          <div v-else-if="unlocked" class="workbench">
            <aside class="workbench-nav">
              <div class="brand">
                <img class="brand-logo" :src="logoUrl" alt="" />
                <span class="brand-title">ServerDesk</span>
              </div>
              <nav aria-label="主导航" class="nav-list">
                <n-tooltip v-for="(item, key) in pages" :key="key" placement="right">
                  <template #trigger>
                    <button class="nav-item" :aria-current="page === key ? 'page' : undefined"
                      :aria-label="key === 'approvals' && approvalCount ? `审批中心，${approvalCount} 条待审批` : item.label"
                      @click="navigate(key)">
                      <n-icon :component="item.icon" size="19" />
                      <span class="nav-label">{{ item.label }}</span>
                      <span v-if="key === 'approvals' && approvalCount" class="nav-count" aria-hidden="true">{{ approvalCount }}</span>
                    </button>
                  </template>
                  {{ item.label }}
                </n-tooltip>
              </nav>
              <n-tooltip placement="right">
                <template #trigger>
                  <button class="nav-item nav-lock" aria-label="锁定保险库" @click="lockVault">
                    <n-icon :component="LockClosedOutline" size="18" />
                    <span class="nav-label">锁定保险库</span>
                  </button>
                </template>
                锁定保险库
              </n-tooltip>
            </aside>
            <header class="workbench-header">
              <h1>{{ pages[page].label }}</h1>
              <button class="search-trigger" @click="openSearch">
                <n-icon :component="SearchOutline" size="18" />
                <span>搜索服务器或页面</span>
                <kbd>{{ searchShortcut }}</kbd>
              </button>
            </header>
            <main class="workbench-content" :class="{ 'server-workspace': page === 'servers' }">
              <KeepAlive include="ServersView,AuditView">
                <component :is="pages[page].view" v-bind="pageBindings" />
              </KeepAlive>
            </main>
            <footer class="activity-track">
              <n-icon :component="PulseOutline" size="17" class="track-icon" />
              <span class="track-label">AI 活动</span>
              <div class="track-lane" role="status" aria-live="polite">
                <Transition name="track" mode="out-in">
                  <span v-if="approvalCount" key="approval" class="track-item track-warning">
                    <span class="track-dot" />{{ approvalCount }} 条操作待审批
                  </span>
                  <span v-else-if="latestActivity" :key="latestActivity.id" class="track-item"
                    :class="{ 'track-running': latestActivity.running, 'track-success': latestActivity.ok, 'track-error': latestActivity.ok === false }">
                    <span class="track-dot" />
                    <span class="track-text">{{ latestActivity.server || "全局" }} · {{ latestActivity.tool }}</span>
                    <span class="track-state">{{ latestActivity.running ? "进行中" : latestActivity.ok ? "已完成" : "未成功" }}</span>
                  </span>
                  <span v-else key="idle" class="track-item"><span class="track-dot" />暂无 AI 活动</span>
                </Transition>
              </div>
              <button v-if="approvalCount" class="track-link" @click="navigate('approvals')">去审批中心 →</button>
            </footer>
          </div>
          <div v-else class="startup-state">
            <n-result v-if="vaultError" status="error" title="暂时无法读取保险库状态">
              <template #footer><n-button @click="refreshVault">重试</n-button></template>
            </n-result>
            <n-spin v-else-if="!booted" description="正在读取保险库…" />
          </div>
          <n-modal v-if="unlocked" v-model:show="searchOpen" preset="card" title="搜索服务器或页面"
            class="search-palette" :auto-focus="false" @after-enter="searchInput?.focus()" @after-leave="finishSearch">
            <n-input ref="searchInput" v-model:value="query" clearable autofocus placeholder="名称、主机地址、标签或页面名"
              :input-props="{ 'aria-label': '搜索服务器或页面' }" @keydown="onResultKey" />
            <p v-if="!results.length" class="palette-empty" role="status">
              {{ servers.loading ? "正在加载服务器…" : servers.loadError ? "无法加载服务器，请在服务器页重试" : "没有匹配的结果" }}
            </p>
            <ul v-else class="palette-list">
              <li v-for="(result, index) in results" :id="`search-result-${index}`" :key="result.key">
                <button :class="{ 'result-active': index === activeResult }" @click="chooseResult(result)" @focus="activeResult = index">
                  <span>{{ result.label }}</span><small>{{ result.hint }}</small>
                </button>
              </li>
            </ul>
          </n-modal>
        </n-dialog-provider>
      </n-notification-provider>
    </n-message-provider>
  </n-config-provider>
</template>
