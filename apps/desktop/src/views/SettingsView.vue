<script setup lang="ts">
import { onMounted, ref } from "vue";
import { NButton, NCard, NSelect, NSwitch, NTag, useMessage } from "naive-ui";
import { api, type VaultStatus } from "../api";

const message = useMessage();
const status = ref<VaultStatus | null>(null);
const autoLockMinutes = ref<number>(10);
const keychain = ref(false);
const saving = ref(false);

const lockOptions = [
  { label: "5 分钟", value: 5 },
  { label: "10 分钟", value: 10 },
  { label: "30 分钟", value: 30 },
  { label: "60 分钟", value: 60 },
  { label: "从不自动锁", value: 0 },
];

onMounted(async () => {
  try {
    status.value = await api.vaultStatus();
    autoLockMinutes.value = status.value.autoLockMinutes;
    keychain.value = status.value.keychainAutoUnlock;
  } catch (e) {
    message.error(String(e));
  }
});

async function save() {
  saving.value = true;
  try {
    status.value = await api.vaultSetSettings(autoLockMinutes.value, keychain.value);
    message.success("安全设置已保存");
  } catch (e) {
    message.error(String(e));
  } finally {
    saving.value = false;
  }
}

async function lockNow() {
  try {
    await api.vaultLock();
    message.success("已锁定（窗口即将回到锁屏）");
  } catch (e) {
    message.error(String(e));
  }
}
</script>

<template>
  <div class="settings">
    <n-card title="保险库安全设置" size="small" style="max-width: 640px">
      <div class="row">
        <div class="label">
          <div>空闲自动锁定</div>
          <div class="desc">无任何凭据访问操作达到该时长后，保险库自动锁定并要求重新输入主密码</div>
        </div>
        <n-select v-model:value="autoLockMinutes" :options="lockOptions" style="width: 160px" />
      </div>
      <div class="row">
        <div class="label">
          <div>系统钥匙串自动解锁</div>
          <div class="desc">
            将数据密钥镜像存入系统钥匙串（macOS Keychain / Windows 凭据管理器），
            启动应用时免输主密码。关闭本机自动锁屏的用户不建议开启
          </div>
        </div>
        <n-switch v-model:value="keychain" />
      </div>
      <div class="row">
        <div class="label">
          <div>当前状态</div>
          <div class="desc">
            <n-tag size="small" :type="status?.unlocked ? 'success' : 'warning'" :bordered="false">
              {{ status?.unlocked ? "已解锁" : "已锁定" }}
            </n-tag>
          </div>
        </div>
        <n-button size="small" @click="lockNow">立即锁定</n-button>
      </div>
      <template #footer>
        <n-button type="primary" :loading="saving" @click="save">保存设置</n-button>
      </template>
    </n-card>

    <n-card title="路线图" size="small" style="max-width: 640px; margin-top: 16px">
      <ul class="roadmap">
        <li>M3：AI 安全执行管线（高危命令拦截 / 人工审批 / 全量审计 / 输出脱敏）+ MCP 工具</li>
        <li>M4：服务器信息加密备份与恢复、定时自动备份</li>
        <li>M5：macOS（Apple Silicon）与 Windows（amd64）安装包</li>
      </ul>
    </n-card>
  </div>
</template>

<style scoped>
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 10px 0;
}
.row + .row {
  border-top: 1px dashed rgba(255, 255, 255, 0.08);
}
.label > div:first-child {
  font-weight: 600;
}
.desc {
  font-size: 12px;
  opacity: 0.6;
  margin-top: 4px;
  max-width: 420px;
  line-height: 1.6;
}
.roadmap {
  margin: 0;
  padding-left: 18px;
  font-size: 13px;
  line-height: 2;
  opacity: 0.8;
}
</style>
