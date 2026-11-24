<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  NButton,
  NCard,
  NInput,
  NInputNumber,
  NSelect,
  NSwitch,
  NTag,
  useMessage,
} from "naive-ui";
import { api, type GuardInfo, type GuardRules, type McpInfo, type VaultStatus } from "../api";
import { useServersStore } from "../stores/servers";

const message = useMessage();
const serversStore = useServersStore();
const status = ref<VaultStatus | null>(null);
const autoLockMinutes = ref<number>(0);
const keychain = ref(false);
const saving = ref(false);

const guard = ref<GuardInfo | null>(null);
const denyText = ref("");
const approvalText = ref("");
const whitelistMode = ref(false);
const whitelistText = ref("");
const readonly = ref(false);
const ratePerMin = ref<number>(30);
const strictGroups = ref<string[]>([]);
const savingRules = ref(false);
const mcpInfo = ref<McpInfo | null>(null);
const showToken = ref(false);

const lockOptions = [
  { label: "5 分钟", value: 5 },
  { label: "10 分钟", value: 10 },
  { label: "30 分钟", value: 30 },
  { label: "60 分钟", value: 60 },
  { label: "从不自动锁", value: 0 },
];

const groupOptions = computed(() =>
  serversStore.groups.map((g) => ({ label: g, value: g }))
);

const modeValue = computed({
  get: () => (whitelistMode.value ? "whitelist" : "blacklist"),
  set: (v: string) => {
    whitelistMode.value = v === "whitelist";
  },
});

const modeOptions = [
  { label: "黑名单模式", value: "blacklist" },
  { label: "白名单模式", value: "whitelist" },
];

async function loadGuard() {
  try {
    guard.value = await api.guardInfo();
    denyText.value = guard.value.deny.join("\n");
    approvalText.value = guard.value.approval.join("\n");
    whitelistMode.value = guard.value.whitelistMode;
    whitelistText.value = guard.value.whitelist.join("\n");
    readonly.value = guard.value.readonly;
    ratePerMin.value = guard.value.ratePerMin;
    strictGroups.value = [...guard.value.strictGroups];
    mcpInfo.value = await api.mcpInfo();
  } catch (e) {
    message.error(String(e));
  }
}

onMounted(async () => {
  try {
    status.value = await api.vaultStatus();
    autoLockMinutes.value = status.value.autoLockMinutes;
    keychain.value = status.value.keychainAutoUnlock;
  } catch (e) {
    message.error(String(e));
  }
  serversStore.load().catch(() => {});
  loadGuard();
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

function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

async function saveRules() {
  savingRules.value = true;
  try {
    const rules: GuardRules = {
      deny: parseLines(denyText.value),
      approval: parseLines(approvalText.value),
      whitelistMode: whitelistMode.value,
      whitelist: parseLines(whitelistText.value),
      readonly: readonly.value,
      ratePerMin: Math.max(0, Math.round(ratePerMin.value || 0)),
      strictGroups: strictGroups.value,
    };
    guard.value = await api.guardSetRules(rules);
    denyText.value = guard.value.deny.join("\n");
    approvalText.value = guard.value.approval.join("\n");
    whitelistText.value = guard.value.whitelist.join("\n");
    strictGroups.value = [...guard.value.strictGroups];
    message.success("安全规则已保存");
  } catch (e) {
    message.error(String(e));
  } finally {
    savingRules.value = false;
  }
}

async function toggleAi(enabled: boolean) {
  try {
    guard.value = await api.mcpSetEnabled(enabled);
    message.success(enabled ? "AI 接入已开启（MCP 工具可用）" : "AI 接入已关闭（MCP 工具全部拒绝）");
  } catch (e) {
    message.error(String(e));
  }
}

async function copy(text: string, label: string) {
  try {
    await navigator.clipboard.writeText(text);
    message.success(`已复制${label}`);
  } catch {
    message.warning("复制失败，请手动选择文本复制");
  }
}
</script>

<template>
  <div class="settings">
    <n-card title="保险库安全设置" size="small" style="max-width: 720px">
      <div class="row">
        <div class="label">
          <div>空闲自动锁定</div>
          <div class="desc">无任何凭据访问操作达到该时长后，保险库自动锁定并要求重新输入主密码。默认从不自动锁，由你在侧栏手动锁定</div>
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

    <n-card title="AI 安全管线（MCP 接入）" size="small" style="max-width: 720px; margin-top: 16px">
      <div class="row">
        <div class="label">
          <div>允许 AI 通过 MCP 操作服务器</div>
          <div class="desc">
            关闭后 sidecar 的所有工具调用直接拒绝。每台服务器还可单独设置「允许 AI」开关；
            AI 命令经泄露防护 → 模式判定 → 黑名单/白名单 → 人工审批多层管线，全程审计
          </div>
        </div>
        <n-switch :value="guard?.aiEnabled ?? false" @update:value="toggleAi" />
      </div>
      <div class="row">
        <div class="label">
          <div>MCP 客户端配置</div>
          <div class="desc">
            将以下配置加入 Claude Desktop / Cursor 的 MCP 设置；token 每次启动应用后自动更新，无需改配置
          </div>
        </div>
        <n-button size="small" @click="copy(mcpInfo?.configJson ?? '', '配置 JSON')">复制配置</n-button>
      </div>
      <pre class="pre">{{ mcpInfo?.configJson ?? "（应用启动后生成）" }}</pre>
    </n-card>

    <n-card title="AI 高级策略" size="small" style="max-width: 720px; margin-top: 16px">
      <div class="row">
        <div class="label">
          <div>只读模式</div>
          <div class="desc">
            开启后 AI 的一切写入操作被硬拒绝：文件写入/追加/上传/回滚全拒，
            命令按**子命令**逐段检查，含 rm/cp/ln/chmod/systemctl restart 等写入特征即拒（无审批逃生口）；
            写到 /dev/null 的重定向不算写入，命令替换（$( )、反引号）在受限模式下直接拒。适合临时排查场景
          </div>
        </div>
        <n-switch v-model:value="readonly" />
      </div>
      <div class="row">
        <div class="label">
          <div>AI 调用速率限制</div>
          <div class="desc">每分钟最多允许的 AI 工具调用次数，防止 Agent 失控循环。设为 0 表示不限</div>
        </div>
        <n-input-number v-model:value="ratePerMin" :min="0" :max="600" style="width: 130px">
          <template #suffix>次/分</template>
        </n-input-number>
      </div>
      <div class="row">
        <div class="label">
          <div>严格分组（全部审批）</div>
          <div class="desc">这些分组的服务器上，AI 的所有命令（包括只读命令与观测五件套）都需要人工批准</div>
        </div>
        <n-select
          v-model:value="strictGroups"
          :options="groupOptions"
          multiple
          clearable
          placeholder="选择分组"
          style="width: 240px"
        />
      </div>
    </n-card>

    <n-card title="命令安全规则（可自定义）" size="small" style="max-width: 720px; margin-top: 16px">
      <div class="row" style="margin-bottom: 10px">
        <div class="label">
          <div>判定模式</div>
          <div class="desc">
            黑名单：默认——除黑名单/审批名单外都放行。
            白名单：只放行命中白名单的子命令，其余直接拒（生产环境推荐）。
            复合命令（;、&&、||、|）逐段匹配，任何一段未命中即整体拒绝；
            观测五件套这类应用内置的只读模板不套用白名单
          </div>
        </div>
        <n-select v-model:value="modeValue" :options="modeOptions" style="width: 160px" />
      </div>
      <div v-if="whitelistMode" class="row" style="margin-bottom: 10px">
        <div class="label">
          <div>白名单条目</div>
          <div class="desc">子命令命中任意一条即放行（子串匹配，每行一条）</div>
        </div>
      </div>
      <textarea
        v-if="whitelistMode"
        v-model="whitelistText"
        class="rules-input"
        rows="4"
        spellcheck="false"
        placeholder="uptime&#10;df -h&#10;free"
        style="margin-bottom: 14px"
      />
      <div class="rules-grid">
        <div>
          <div class="rule-title">
            <n-tag size="small" type="error" :bordered="false">硬拦截</n-tag>
            命中即拒绝，不执行、不审批（每行一条，子串匹配）
          </div>
          <textarea v-model="denyText" class="rules-input" rows="10" spellcheck="false" />
        </div>
        <div>
          <div class="rule-title">
            <n-tag size="small" type="warning" :bordered="false">需审批</n-tag>
            AI 提交后需在「审批中心」人工批准（120 秒超时自动拒绝）
          </div>
          <textarea v-model="approvalText" class="rules-input" rows="10" spellcheck="false" />
        </div>
      </div>
      <p class="hint">
        凭据泄露防护（env / printenv / 私钥文件 / .pem / /etc/shadow 等）为内置固定规则，不开放编辑且优先级最高。
      </p>
      <template #footer>
        <n-button type="primary" :loading="savingRules" @click="saveRules">保存全部规则与策略</n-button>
      </template>
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
  max-width: 480px;
  line-height: 1.6;
}
.pre {
  background: rgba(255, 255, 255, 0.04);
  border-radius: 6px;
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 8px 0;
}
.rules-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px;
}
.rule-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  margin-bottom: 8px;
}
.rules-input {
  width: 100%;
  background: rgba(255, 255, 255, 0.04);
  color: inherit;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  padding: 10px;
  font-size: 12px;
  line-height: 1.7;
  font-family: ui-monospace, Menlo, monospace;
  resize: vertical;
}
</style>
