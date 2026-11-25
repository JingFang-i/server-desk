<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import {
  NButton,
  NConfigProvider,
  NDialogProvider,
  NLayout,
  NLayoutSider,
  NLayoutContent,
  NMenu,
  NMessageProvider,
  NNotificationProvider,
  darkTheme,
  zhCN,
  dateZhCN,
  type MenuOption,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type VaultStatus } from "./api";
import ApprovalAlert from "./components/ApprovalAlert.vue";
import UpdatePrompt from "./components/UpdatePrompt.vue";
import ServersView from "./views/ServersView.vue";
import SettingsView from "./views/SettingsView.vue";
import ApprovalsView from "./views/ApprovalsView.vue";
import AuditView from "./views/AuditView.vue";
import BackupView from "./views/BackupView.vue";
import ChangeLogView from "./views/ChangeLogView.vue";
import LockGate from "./views/LockGate.vue";

type PageKey = "servers" | "approvals" | "audit" | "changes" | "backup" | "settings";
const page = ref<PageKey>("servers");
const vault = ref<VaultStatus | null>(null);
const booted = ref(false);
/** 待审批数量：菜单角标，由 ApprovalAlert 轮询维护 */
const approvalCount = ref(0);

const menuOptions = computed<MenuOption[]>(() => [
  { label: "🖥 服务器", key: "servers" },
  {
    label: approvalCount.value > 0 ? `✅ 审批中心 (${approvalCount.value})` : "✅ 审批中心",
    key: "approvals",
  },
  { label: "📜 审计日志", key: "audit" },
  { label: "🔄 变更记录", key: "changes" },
  { label: "💾 备份恢复", key: "backup" },
  { label: "⚙️ 设置", key: "settings" },
]);

function onMenu(key: string) {
  if (["servers", "approvals", "audit", "changes", "backup", "settings"].includes(key)) {
    page.value = key as PageKey;
  }
}

async function refreshVault() {
  try {
    vault.value = await api.vaultStatus();
  } catch {
    /* ignore */
  }
}

function onUnlocked() {
  refreshVault();
}

async function lockVault() {
  try {
    await api.vaultLock();
    refreshVault();
  } catch {
    /* ignore */
  }
}

let unlisten: UnlistenFn | undefined;
let unlistenMenuLock: UnlistenFn | undefined;
let pollTimer: number | undefined;

onMounted(async () => {
  await refreshVault();
  booted.value = true;
  unlisten = await listen("vault-locked", () => refreshVault());
  // 原生菜单「锁定保险库」（mac 应用菜单 / win 菜单栏，快捷键 Cmd/Ctrl+L）
  unlistenMenuLock = await listen("menu-lock-vault", () => void lockVault());
  pollTimer = window.setInterval(() => {
    if (vault.value?.unlocked) refreshVault();
  }, 20000);
});

onUnmounted(() => {
  unlisten?.();
  unlistenMenuLock?.();
  if (pollTimer) window.clearInterval(pollTimer);
});
</script>

<template>
  <n-config-provider
    :theme="darkTheme"
    :locale="zhCN"
    :date-locale="dateZhCN"
    style="height: 100%"
  >
    <n-message-provider>
      <n-notification-provider placement="top-right" :max="3">
        <!-- 全局审批提醒：提示音 + 应用内通知 + 待审批角标；未授权/未启用时顶部横幅引导 -->
        <ApprovalAlert @count="approvalCount = $event" @open="page = 'approvals'" @settings="page = 'settings'" />
        <!-- 应用内更新：启动静默检查，发现新版本弹窗提示 -->
        <UpdatePrompt />
        <n-dialog-provider>
          <!-- 保险库锁屏门 -->
          <LockGate
            v-if="booted && vault && !vault.unlocked"
            :status="vault"
            @unlocked="onUnlocked"
          />
          <n-layout v-else-if="booted && vault" has-sider style="height: 100vh">
            <n-layout-sider
              bordered
              :width="216"
              content-style="padding: 18px 12px 14px; display: flex; flex-direction: column; height: 100%"
            >
              <div class="brand-title">ServerDesk</div>
              <div class="brand-sub" style="margin-bottom: 14px">
                本地服务器管理
              </div>
              <n-menu :value="page" :options="menuOptions" @update:value="onMenu" />
              <div style="flex: 1" />
              <n-button size="small" block quaternary style="margin-top: 12px" @click="lockVault">
                🔒 锁定保险库
              </n-button>
            </n-layout-sider>
            <n-layout-content
              content-style="padding: 20px 24px; height: 100vh; overflow: auto"
            >
              <ServersView v-if="page === 'servers'" />
              <ApprovalsView v-else-if="page === 'approvals'" />
              <AuditView v-else-if="page === 'audit'" />
              <ChangeLogView v-else-if="page === 'changes'" />
              <BackupView v-else-if="page === 'backup'" />
              <SettingsView v-else-if="page === 'settings'" />
            </n-layout-content>
          </n-layout>
        </n-dialog-provider>
      </n-notification-provider>
    </n-message-provider>
  </n-config-provider>
</template>
