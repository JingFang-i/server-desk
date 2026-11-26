<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import {
  NButton,
  NCard,
  NEmpty,
  NInput,
  NInputNumber,
  NSelect,
  NSwitch,
  NTag,
  useMessage,
  type SelectOption,
} from "naive-ui";
import { getVersion } from "@tauri-apps/api/app";
import UpdatePrompt from "../components/UpdatePrompt.vue";
import { api, type GuardInfo, type GuardRules, type McpInfo, type ReviewConfig, type ReviewEntry, type ReviewInfo, type ReviewTestResult, type VaultStatus } from "../api";
import { CHIME_OPTIONS, currentChime, alertAudioState, ensureAudio, setChime } from "../alert";
import { notifyPermission, notifyPermissionState } from "../notifyStatus";
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

// ---------- 设置项目录（左侧锚点导航） ----------
const sections = [
  { id: "sec-vault", label: "保险库安全" },
  { id: "sec-alerts", label: "审批提醒与声音" },
  { id: "sec-mcp", label: "AI 管线（MCP 接入）" },
  { id: "sec-policy", label: "AI 高级策略" },
  { id: "sec-review", label: "AI 智能审查" },
  { id: "sec-rules", label: "命令安全规则" },
  { id: "sec-update", label: "应用更新" },
];
const activeSection = ref("sec-vault");
let sectionObserver: IntersectionObserver | undefined;

/** 点击目录：平滑滚动到对应卡片 */
function jumpTo(id: string) {
  activeSection.value = id;
  document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
}

// ---------- 审批提醒与声音 ----------
// 「提示音一定会响」的两个前提都在这里显式管理：
// 1) 应用内提示音：受自动播放策略约束，需一次用户手势（点击启用并试听）
// 2) 系统通知（后台可达、带声音）：macOS 需用户授权，未授权时给出申请入口
const notifyPerm = notifyPermissionState;
const audio = alertAudioState;
const chime = ref(currentChime());
const chimeOptions: SelectOption[] = CHIME_OPTIONS.map((o) => ({
  label: o.label,
  value: o.value,
}));

/** 切换提示音风格：立即生效并试听一声 */
function changeChime(v: string) {
  setChime(v);
  void ensureAudio();
}

async function loadAlertStatus() {
  await notifyPermission();
}

async function enableSound() {
  if (await ensureAudio()) message.success("提示音已启用（刚才那声就是试听）");
  else message.error("启用失败：请检查系统音量与输出设备后重试");
}

async function requestPerm() {
  const status = await notifyPermission(true);
  if (status === "granted") message.success("系统通知已授权");
  else if (status === "denied")
    message.warning("授权被拒绝：请到系统设置 → 通知 → ServerDesk 手动允许");
}

// ---------- AI 智能审查（R8） ----------
const rv = ref<ReviewInfo | null>(null);
const rvEnabled = ref(false);
const rvBaseUrl = ref("");
const rvModel = ref("");
const rvKeyInput = ref("");
const rvTimeoutMs = ref(4000);
const rvMaxTokens = ref(120);
const rvFailurePolicy = ref("escalate");
const rvDenyMode = ref("escalate");
const rvSendCtx = ref(false);
const rvSaving = ref(false);
const rvTesting = ref(false);
const rvTestResult = ref<ReviewTestResult | null>(null);
const rvRecent = ref<ReviewEntry[]>([]);

const providerPresets = [
  { label: "本地 Ollama（零出网）", baseUrl: "http://127.0.0.1:11434/v1", model: "openbmb/minicpm5-2b:latest" },
  { label: "LM Studio（零出网）", baseUrl: "http://127.0.0.1:1234/v1", model: "" },
  { label: "DeepSeek", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  { label: "月之暗面 Kimi", baseUrl: "https://api.moonshot.cn/v1", model: "moonshot-v1-8k" },
  { label: "智谱 GLM", baseUrl: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4-flash" },
  { label: "硅基流动", baseUrl: "https://api.siliconflow.cn/v1", model: "deepseek-ai/DeepSeek-V3" },
  { label: "OpenAI", baseUrl: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  { label: "自定义", baseUrl: "", model: "" },
];
const presetOptions = providerPresets.map((p) => ({ label: p.label, value: p.label }));

function applyPreset(label: string) {
  const p = providerPresets.find((x) => x.label === label);
  if (!p) return;
  if (p.baseUrl) rvBaseUrl.value = p.baseUrl;
  if (p.model) rvModel.value = p.model;
}

const failurePolicyOptions = [
  { label: "转人工审批（推荐）", value: "escalate" },
  { label: "按静态规则放行", value: "allow" },
  { label: "直接拒绝", value: "block" },
];
const denyModeOptions = [
  { label: "转人工审批（推荐）", value: "escalate" },
  { label: "直接拒绝", value: "reject" },
];

function verdictTagType(v: string): "success" | "warning" | "error" | "default" {
  if (v === "allow") return "success";
  if (v === "review") return "warning";
  if (v === "deny") return "error";
  return "default";
}

async function loadReview() {
  try {
    rv.value = await api.aiReviewInfo();
    rvEnabled.value = rv.value.enabled;
    rvBaseUrl.value = rv.value.baseUrl;
    rvModel.value = rv.value.model;
    rvTimeoutMs.value = rv.value.timeoutMs;
    rvMaxTokens.value = rv.value.maxTokens;
    rvFailurePolicy.value = rv.value.failurePolicy;
    rvDenyMode.value = rv.value.denyMode;
    rvSendCtx.value = rv.value.sendServerContext;
    rvRecent.value = await api.aiReviewRecent(20);
  } catch (e) {
    message.error(String(e));
  }
}

async function saveReview() {
  rvSaving.value = true;
  try {
    const cfg: ReviewConfig = {
      enabled: rvEnabled.value,
      baseUrl: rvBaseUrl.value,
      model: rvModel.value,
      protocol: "openaicompat",
      timeoutMs: rvTimeoutMs.value,
      maxInputBytes: rv.value?.maxInputBytes ?? 8192,
      maxTokens: rvMaxTokens.value,
      failurePolicy: rvFailurePolicy.value as ReviewConfig["failurePolicy"],
      denyMode: rvDenyMode.value as ReviewConfig["denyMode"],
      reviewFiles: rv.value?.reviewFiles ?? false,
      sendServerContext: rvSendCtx.value,
      cacheTtlSecs: rv.value?.cacheTtlSecs ?? 1800,
      promptVersion: rv.value?.promptVersion ?? "v2",
    };
    // 端点变更时，后端会移除旧 Key；保存成功后清空输入框
    rv.value = await api.aiReviewSave(cfg, rvKeyInput.value.trim() || undefined);
    rvKeyInput.value = "";
    message.success(rv.value.enabled ? "AI 审查配置已保存并热生效" : "AI 审查已关闭");
  } catch (e) {
    message.error(String(e));
  } finally {
    rvSaving.value = false;
  }
}

async function testReview() {
  rvTesting.value = true;
  rvTestResult.value = null;
  try {
    const r = await api.aiReviewTest();
    rvTestResult.value = r;
    if (r.ok) message.success(`连通正常：${r.model || "模型"}（${r.latencyMs}ms）`);
    else message.error(r.error || "测试失败");
  } catch (e) {
    message.error(String(e));
  } finally {
    rvTesting.value = false;
  }
}

// ---------- 应用内更新 ----------
const appVersion = ref("");
const updatePromptRef = ref<{ check: (manual: boolean) => Promise<void>; checking: boolean } | null>(null);
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
  getVersion().then((v: string) => (appVersion.value = v)).catch(() => {});
  try {
    status.value = await api.vaultStatus();
    autoLockMinutes.value = status.value.autoLockMinutes;
    keychain.value = status.value.keychainAutoUnlock;
  } catch (e) {
    message.error(String(e));
  }
  serversStore.load().catch(() => {});
  loadGuard();
  loadReview();
  loadAlertStatus();
});

// IntersectionObserver 挂在 DOM 就绪后（卡片 id 存在于模板）
onMounted(() => {
  sectionObserver = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) activeSection.value = e.target.id;
      }
    },
    { rootMargin: "-15% 0px -75% 0px" }
  );
  for (const s of sections) {
    const el = document.getElementById(s.id);
    if (el) sectionObserver.observe(el);
  }
});
onUnmounted(() => sectionObserver?.disconnect());

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
    message.success("已锁定");
    status.value = await api.vaultStatus();
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
  <div class="settings-layout">
    <aside class="settings-toc">
      <div
        v-for="s in sections"
        :key="s.id"
        class="toc-item"
        :class="{ active: activeSection === s.id }"
        @click="jumpTo(s.id)"
      >
        {{ s.label }}
      </div>
    </aside>
    <div class="settings">
    <n-card id="sec-vault" title="保险库安全设置" size="small" style="max-width: 720px">
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

    <n-card id="sec-alerts" title="审批提醒与声音" size="small" style="max-width: 720px; margin-top: 16px">
      <p class="hint" style="margin: 0 0 6px">
        确保 AI 请求审批时你一定能听到。两条通道互为补充：
        应用内提示音在窗口内响（三音提示，未处理每 30 秒重复）；
        系统通知在后台也可达（带系统音效 + 程序坞/任务栏提醒）。
      </p>
      <div class="row">
        <div class="label">
          <div>提示音（应用内）</div>
          <div class="desc">受浏览器自动播放策略约束：需要点击「启用」完成一次授权并试听；切换风格会立即播一声当前音色</div>
        </div>
        <div style="display: flex; gap: 8px; align-items: center">
          <n-tag size="small" :type="audio === 'running' ? 'success' : 'warning'" :bordered="false">
            {{ audio === "running" ? "已启用" : "未启用" }}
          </n-tag>
          <n-select
            v-model:value="chime"
            :options="chimeOptions"
            size="small"
            style="width: 170px"
            @update:value="changeChime"
          />
          <n-button size="small" @click="enableSound">{{ audio === "running" ? "试听" : "启用并试听" }}</n-button>
        </div>
      </div>
      <div class="row">
        <div class="label">
          <div>系统通知（后台可达）</div>
          <div class="desc">
            后台提醒由系统通知设置控制。打包后的 ServerDesk.app 可在此查询/申请权限；
            <template v-if="notifyPerm === 'dev'">
              当前为<strong>开发模式</strong>——没有应用包身份，系统通知权限无法查询，请以打包版验证。
            </template>
          </div>
        </div>
        <div style="display: flex; gap: 8px; align-items: center">
          <n-tag
            size="small"
            :type="notifyPerm === 'granted' ? 'success' : notifyPerm === 'denied' ? 'error' : notifyPerm === 'prompt' ? 'warning' : 'default'"
            :bordered="false"
          >
            {{
              notifyPerm === "granted"
                ? "已授权"
                : notifyPerm === "denied"
                  ? "已拒绝"
                  : notifyPerm === "prompt"
                    ? "待授权"
                    : notifyPerm === "dev"
                      ? "开发模式"
                      : "无法确认"
            }}
          </n-tag>
          <n-button v-if="notifyPerm === 'prompt'" size="small" type="primary" ghost @click="requestPerm">
            申请授权
          </n-button>
          <n-button v-else size="small" quaternary @click="loadAlertStatus">刷新状态</n-button>
        </div>
      </div>
    </n-card>

    <n-card id="sec-mcp" title="AI 安全管线（MCP 接入）" size="small" style="max-width: 720px; margin-top: 16px">
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

    <n-card id="sec-policy" title="AI 高级策略" size="small" style="max-width: 720px; margin-top: 16px">
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

    <n-card id="sec-review" title="AI 智能审查（实验性）" size="small" style="max-width: 720px; margin-top: 16px">
      <p class="hint" style="margin: 0 0 6px">
        启用后，AI 的每条命令在静态规则判定之后，会再交给一个你配置的模型做语义安全审查；
        结论只会让判定更严格（放行/转人工/建议拒绝）。待审命令文本会先做凭据脱敏再发送到所选服务——
        选「本地 Ollama / LM Studio」可完全零出网。
      </p>
      <div class="row">
        <div class="label">
          <div>启用 AI 审查</div>
          <div class="desc">关闭时行为与此功能上线前完全一致（静态规则 + 人工审批 + 审计）</div>
        </div>
        <n-switch v-model:value="rvEnabled" />
      </div>
      <div class="row">
        <div class="label">
          <div>供应商预设</div>
          <div class="desc">只做默认值填充，以下字段仍可手改</div>
        </div>
        <n-select :options="presetOptions" style="width: 220px" @update:value="applyPreset" />
      </div>
      <div class="row">
        <div class="label">
          <div>BaseUrl</div>
          <div class="desc">OpenAI 兼容地址；本机服务可用 http://127.0.0.1</div>
        </div>
        <n-input v-model:value="rvBaseUrl" placeholder="https://api.deepseek.com/v1" style="width: 340px" />
      </div>
      <div class="row">
        <div class="label">
          <div>API Key</div>
          <div class="desc">
            保存后仅显示掩码（{{ rv?.keyMask || "未配置" }}）。地址变更时请填写新 Key，留空会清除旧 Key。
          </div>
        </div>
        <n-input
          v-model:value="rvKeyInput"
          type="password"
          show-password-on="click"
          placeholder="sk-…"
          style="width: 340px"
        />
      </div>
      <div class="row">
        <div class="label">
          <div>模型名称</div>
          <div class="desc">需兼容 OpenAI chat/completions 协议</div>
        </div>
        <n-input v-model:value="rvModel" placeholder="openbmb/minicpm5-2b:latest" style="width: 340px" />
      </div>
      <div class="row">
        <div class="label">
          <div>审查超时</div>
          <div class="desc">单次审查最长等待；超时按「失败策略」处理。推理型模型建议 8000ms 以上</div>
        </div>
        <n-input-number v-model:value="rvTimeoutMs" :min="1000" :max="15000" :step="500" style="width: 170px">
          <template #suffix>ms</template>
        </n-input-number>
      </div>
      <div class="row">
        <div class="label">
          <div>输出 token 预算</div>
          <div class="desc">推理模型（DeepSeek-R1、MiniCPM 等）建议 ≥512；过小会截断思维链导致审查失败</div>
        </div>
        <n-input-number v-model:value="rvMaxTokens" :min="64" :max="4096" :step="64" style="width: 170px" />
      </div>
      <div class="row">
        <div class="label">
          <div>审查失败时（超时 / 网络 / 解析）</div>
          <div class="desc">连续失败 3 次进入 60s 熔断，冷却期内不再调用模型</div>
        </div>
        <n-select v-model:value="rvFailurePolicy" :options="failurePolicyOptions" style="width: 220px" />
      </div>
      <div class="row">
        <div class="label">
          <div>AI 判「拒绝」时</div>
          <div class="desc">转人工审批保留你的最终裁决权；直接拒绝不产生审批卡</div>
        </div>
        <n-select v-model:value="rvDenyMode" :options="denyModeOptions" style="width: 220px" />
      </div>
      <div class="row">
        <div class="label">
          <div>发送服务器分组 / 名称</div>
          <div class="desc">默认关闭（不向模型发送主机名等资产信息）；开启可提升分组场景下的判定质量</div>
        </div>
        <n-switch v-model:value="rvSendCtx" />
      </div>
      <div class="row">
        <div class="label">
          <div>连通性测试</div>
          <div class="desc">用一条固定的安全命令真实调用一次模型</div>
        </div>
        <n-button size="small" :loading="rvTesting" :disabled="!rvEnabled" @click="testReview">测试连接</n-button>
      </div>
      <div v-if="rvTestResult" class="test-result" :class="rvTestResult.ok ? 'ok' : 'bad'">
        <template v-if="rvTestResult.ok">
          ✓ {{ rvTestResult.model }}（{{ rvTestResult.latencyMs }}ms）：{{ rvTestResult.content || "（空响应）" }}
        </template>
        <template v-else>✗ {{ rvTestResult.error }}</template>
      </div>
      <template #footer>
        <n-button type="primary" :loading="rvSaving" @click="saveReview">保存 AI 审查配置</n-button>
      </template>
      <div class="rv-recent-title">最近审查记录</div>
      <n-empty v-if="!rvRecent.length" size="small" description="暂无记录（审查产生的结论会显示在这里）" />
      <div v-for="e in rvRecent" :key="e.id" class="rv-recent-item">
        <n-tag size="small" :type="verdictTagType(e.aiVerdict)" :bordered="false">
          {{ e.aiVerdict }}{{ e.cached ? "·缓存" : "" }}
        </n-tag>
        <span class="rv-tool">{{ e.tool }}</span>
        <span class="rv-reason">{{ e.reason || e.error || "—" }}</span>
        <span class="rv-latency">{{ e.latencyMs }}ms</span>
      </div>
    </n-card>

    <n-card id="sec-rules" title="命令安全规则（可自定义）" size="small" style="max-width: 720px; margin-top: 16px">
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
    <n-card id="sec-update" title="应用更新" size="small" style="max-width: 720px; margin-top: 16px">
      <div style="display: flex; gap: 10px; align-items: center">
        <span style="font-size: 13px">当前版本</span>
        <span style="font-family: ui-monospace, Menlo, monospace; font-size: 12.5px">
          v{{ appVersion || "…" }}
        </span>
        <n-button
          size="small"
          :loading="updatePromptRef?.checking"
          @click="updatePromptRef?.check(true)"
        >
          检查更新
        </n-button>
      </div>
      <p class="hint" style="margin: 8px 0 0">
        启动时也会自动检查；发现新版本会弹窗提示，确认后自动下载安装并重启。更新包经数字签名校验，
        Linux 的 deb/rpm 安装不支持应用内更新（AppImage 支持）。
      </p>
      <UpdatePrompt ref="updatePromptRef" />
    </n-card>
    </div>
  </div>
</template>

<style scoped>
/* 左侧设置目录：sticky 跟随滚动，点击平滑跳转到对应卡片 */
.settings-layout {
  display: flex;
  gap: 18px;
  align-items: flex-start;
}
.settings-toc {
  position: sticky;
  top: 12px;
  width: 150px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.toc-item {
  padding: 7px 10px;
  border-radius: 6px;
  font-size: 13px;
  cursor: pointer;
  opacity: 0.65;
  transition: background 0.15s, opacity 0.15s;
}
.toc-item:hover {
  background: rgba(255, 255, 255, 0.06);
  opacity: 0.9;
}
.toc-item.active {
  background: rgba(255, 255, 255, 0.09);
  opacity: 1;
  font-weight: 600;
}
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
.test-result {
  margin-top: 4px;
  font-size: 12px;
  line-height: 1.6;
  padding: 8px 10px;
  border-radius: 6px;
  word-break: break-all;
}
.test-result.ok {
  color: #63e2b7;
  background: rgba(99, 226, 183, 0.08);
}
.test-result.bad {
  color: #e88080;
  background: rgba(232, 128, 128, 0.08);
}
.rv-recent-title {
  margin-top: 16px;
  font-weight: 600;
  font-size: 13px;
}
.rv-recent-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 0;
  border-bottom: 1px dashed rgba(255, 255, 255, 0.06);
  font-size: 12px;
}
.rv-tool {
  font-family: ui-monospace, Menlo, monospace;
  opacity: 0.7;
  flex-shrink: 0;
}
.rv-reason {
  flex: 1;
  opacity: 0.85;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.rv-latency {
  opacity: 0.5;
  flex-shrink: 0;
}
</style>
