<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import {
  NConfigProvider,
  NLayout,
  NLayoutSider,
  NLayoutContent,
  NMenu,
  NMessageProvider,
  darkTheme,
  zhCN,
  dateZhCN,
  type MenuOption,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type VaultStatus } from "./api";
import ServersView from "./views/ServersView.vue";
import SettingsView from "./views/SettingsView.vue";
import LockGate from "./views/LockGate.vue";

type PageKey = "servers" | "settings";
const page = ref<PageKey>("servers");
const vault = ref<VaultStatus | null>(null);
const booted = ref(false);

const menuOptions: MenuOption[] = [
  { label: "🖥 服务器", key: "servers" },
  { label: "✅ 审批中心（M3）", key: "approvals", disabled: true },
  { label: "📜 审计日志（M3）", key: "audit", disabled: true },
  { label: "💾 备份恢复（M4）", key: "backup", disabled: true },
  { label: "⚙️ 设置", key: "settings" },
];

function onMenu(key: string) {
  if (key === "servers" || key === "settings") page.value = key;
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

let unlisten: UnlistenFn | undefined;
let pollTimer: number | undefined;

onMounted(async () => {
  await refreshVault();
  booted.value = true;
  unlisten = await listen("vault-locked", () => refreshVault());
  pollTimer = window.setInterval(() => {
    // 已解锁时轻量轮询，保证多窗口/外部锁定状态一致
    if (vault.value?.unlocked) refreshVault();
  }, 20000);
});

onUnmounted(() => {
  unlisten?.();
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
            本地服务器管理 · M2
          </div>
          <n-menu :value="page" :options="menuOptions" @update:value="onMenu" />
          <div class="sider-note">
            审批中心 / 审计日志将于 M3 提供<br />
            备份恢复将于 M4 提供
          </div>
        </n-layout-sider>
        <n-layout-content
          content-style="padding: 20px 24px; height: 100vh; overflow: auto"
        >
          <ServersView v-if="page === 'servers'" />
          <SettingsView v-else-if="page === 'settings'" />
        </n-layout-content>
      </n-layout>
    </n-message-provider>
  </n-config-provider>
</template>
