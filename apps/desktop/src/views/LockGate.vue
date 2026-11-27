<script setup lang="ts">
import { ref } from "vue";
import { NButton, NInput, NPopconfirm, useMessage } from "naive-ui";
import logoUrl from "../assets/logo.png";
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
  if (!password.value) {
    message.error("请输入主密码");
    return;
  }
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
  if (busy.value) return;
  if (resetText.value !== "RESET") {
    message.error('请输入大写 RESET 以确认');
    return;
  }
  busy.value = true;
  try {
    const snapshot = await api.vaultReset();
    message.success(`已重置。重置前数据已快照到：${snapshot}`, { duration: 12000 });
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
    <section class="lock-panel" aria-labelledby="lock-title">
      <header class="lock-heading">
        <img :src="logoUrl" class="brand-logo" alt="" />
        <h1 id="lock-title">{{ status.initialized ? "保险库已锁定" : "设置主密码" }}</h1>
        <p>{{ status.initialized
          ? "输入主密码，继续管理服务器。"
          : "主密码用于加密服务器凭据。主密码遗忘后，只能重置保险库并清空数据。" }}</p>
      </header>
      <form class="lockform" novalidate @submit.prevent="submit">
        <n-input
          v-model:value="password"
          type="password"
          show-password-on="click"
          :placeholder="status.initialized ? '主密码' : '设置主密码（至少 8 位）'"
          :input-props="{ 'aria-label': '主密码', autocomplete: status.initialized ? 'current-password' : 'new-password' }"
        />
        <n-input
          v-if="!status.initialized"
          v-model:value="confirm"
          type="password"
          show-password-on="click"
          placeholder="再次输入主密码"
          :input-props="{ 'aria-label': '确认主密码', autocomplete: 'new-password' }"
        />
        <n-button attr-type="submit" type="primary" block :loading="busy">
          {{ status.initialized ? "解锁" : "设置主密码并开始使用" }}
        </n-button>
      </form>

      <div v-if="status.initialized" class="lockfoot">
        <n-popconfirm @positive-click="showReset = true">
          <template #trigger>
            <n-button text class="linklike">忘记主密码？</n-button>
          </template>
          重置将删除所有服务器记录与凭据。重置前会自动保存一份数据库快照到数据目录
          backups/，但请确认那确实是你要的操作。确定继续？
        </n-popconfirm>
      </div>

      <div v-if="showReset" class="resetbox">
        <p>
          此操作将删除<b>全部</b>服务器资产与凭据。
          重置前会自动保存一份数据库快照（可用旧主密码解密或直接替换回滚），
          但界面上的数据仍会清空。输入大写 <code>RESET</code> 确认：
        </p>
        <n-input v-model:value="resetText" placeholder="RESET" :input-props="{ 'aria-label': '重置确认文字' }" />
        <n-button type="error" block :loading="busy" @click="doReset">
          我已知晓后果，清空全部数据
        </n-button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.lockgate {
  min-height: 100%;
  display: grid;
  place-items: center;
  padding: 24px;
  background: radial-gradient(ellipse at top, var(--surface), var(--canvas) 65%);
}
.lock-panel {
  width: min(100%, 420px);
  padding: 24px;
  border: 1px solid var(--line);
  border-radius: var(--radius-panel);
  background: var(--sidebar);
}
.lock-heading { text-align: center; }
.lock-heading h1 { margin: 8px 0 4px; font-size: 23px; }
.lock-heading p { margin: 0 0 18px; color: var(--muted); line-height: 1.6; }
.lockform { display: flex; flex-direction: column; gap: 10px; }
.lockfoot { text-align: center; margin-top: 12px; }
.linklike {
  background: none;
  border: none;
  color: var(--muted);
  font-size: 12px;
  cursor: pointer;
  text-decoration: underline;
  padding: 0;
}
.linklike:hover {
  color: var(--text);
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
