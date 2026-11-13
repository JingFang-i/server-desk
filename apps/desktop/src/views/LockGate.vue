<script setup lang="ts">
import { ref } from "vue";
import { NButton, NInput, NPopconfirm, NResult, useMessage } from "naive-ui";
import { api, type VaultStatus } from "../api";

const props = defineProps<{ status: VaultStatus }>();
const emit = defineEmits<{ unlocked: [] }>();

const message = useMessage();
const password = ref("");
const confirm = ref("");
const busy = ref(false);
const resetText = ref("");
const showReset = ref(false);

async function submit() {
  if (busy.value) return;
  busy.value = true;
  try {
    if (!props.status.initialized) {
      // 首次设置主密码
      if (password.value.length < 8) {
        message.error("主密码至少 8 位");
        return;
      }
      if (password.value !== confirm.value) {
        message.error("两次输入的主密码不一致");
        return;
      }
      const migration = await api.vaultSetup(password.value);
      if (migration) message.info(migration, { duration: 6000 });
      message.success("主密码已设置，保险库已解锁");
      password.value = "";
      confirm.value = "";
      emit("unlocked");
    } else {
      const migration = await api.vaultUnlock(password.value);
      if (migration) message.info(migration, { duration: 6000 });
      message.success("保险库已解锁");
      password.value = "";
      emit("unlocked");
    }
  } catch (e) {
    message.error(String(e));
  } finally {
    busy.value = false;
  }
}

async function doReset() {
  if (resetText.value !== "RESET") {
    message.error('请输入大写 RESET 以确认');
    return;
  }
  busy.value = true;
  try {
    await api.vaultReset();
    message.success("已重置。请重新设置主密码");
    resetText.value = "";
    showReset.value = false;
    emit("unlocked"); // 重置后 status.initialized 变为 false，父组件会刷新
    window.location.reload();
  } catch (e) {
    message.error(String(e));
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="lockgate">
    <n-result
      :status="status.initialized ? 'warning' : 'info'"
      :title="status.initialized ? '保险库已锁定' : '欢迎使用 ServerDesk'"
      :description="
        status.initialized
          ? '输入主密码解锁，以访问服务器凭据'
          : '首次使用：请设置主密码。主密码用于加密所有服务器凭据，一旦遗忘无法找回（只能重置并清空全部数据）'
      "
      style="max-width: 560px"
    />
    <div class="lockform">
      <n-input
        v-model:value="password"
        type="password"
        show-password-on="click"
        :placeholder="status.initialized ? '主密码' : '设置主密码（至少 8 位）'"
        @keyup.enter="submit"
      />
      <n-input
        v-if="!status.initialized"
        v-model:value="confirm"
        type="password"
        show-password-on="click"
        placeholder="再次输入主密码"
        @keyup.enter="submit"
      />
      <n-button type="primary" block :loading="busy" @click="submit">
        {{ status.initialized ? "解锁" : "设置主密码并开始使用" }}
      </n-button>

      <div v-if="status.initialized" class="lockfoot">
        <n-popconfirm @positive-click="showReset = true">
          <template #trigger>
            <button class="linklike">忘记主密码？</button>
          </template>
          重置将删除所有服务器记录与凭据，且不可恢复。确定继续？
        </n-popconfirm>
      </div>

      <div v-if="showReset" class="resetbox">
        <p>
          此操作将删除<b>全部</b>服务器资产与凭据，不可恢复。
          输入大写 <code>RESET</code> 确认：
        </p>
        <n-input v-model:value="resetText" placeholder="RESET" />
        <n-button type="error" block :loading="busy" @click="doReset">
          我已知晓后果，清空全部数据
        </n-button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.lockgate {
  height: 100vh;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 20px;
}
.lockform {
  width: 380px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.lockfoot {
  text-align: center;
  margin-top: 4px;
}
.linklike {
  background: none;
  border: none;
  color: rgba(255, 255, 255, 0.45);
  font-size: 12px;
  cursor: pointer;
  text-decoration: underline;
  padding: 0;
}
.linklike:hover {
  color: rgba(255, 255, 255, 0.7);
}
.resetbox {
  border: 1px solid rgba(255, 99, 99, 0.4);
  border-radius: 8px;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.resetbox p {
  margin: 0;
  font-size: 13px;
  line-height: 1.6;
  color: rgba(255, 255, 255, 0.75);
}
.resetbox code {
  color: #ff6363;
  font-weight: 700;
}
</style>
