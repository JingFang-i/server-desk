<script setup lang="ts">
/**
 * 应用内更新：
 * - `autoCheck`（默认 true）时，挂载 3 秒后静默检查一次（失败不打扰），发现新版本弹窗提示；
 *   设置页里那份实例传 `:auto-check="false"`——启动检查由 App.vue 的全局实例负责，
 *   否则每次点进设置都会再查一遍（看起来像「设置页自己一直自动检查更新」）
 * - 暴露 check(manual) 给设置页「检查更新」按钮复用（手动模式会反馈结果）
 * - 用户点「立即更新」→ 下载（显示进度）→ 安装 → 自动重启
 *
 * 更新源：GitHub Release 的 latest.json（tauri-action 在发布时生成并签名）。
 * Linux 的 deb/rpm 安装不支持应用内更新（仅 AppImage 支持），失败会明确报错。
 */
import { h, onMounted, ref } from "vue";
import { useDialog, useMessage } from "naive-ui";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";

const props = withDefaults(
  defineProps<{
    /** 挂载后是否自动检查更新；设置页内嵌实例必须关掉 */
    autoCheck?: boolean;
  }>(),
  { autoCheck: true }
);
const message = useMessage();
const dialog = useDialog();
const checking = ref(false);
const version = ref("");
let unlistenMenu: (() => void) | undefined;

async function checkUpdate(manual: boolean) {
  checking.value = true;
  try {
    if (!version.value) version.value = await getVersion();
    const update: Update | null = await check();
    if (!update) {
      if (manual) message.success(`已是最新版本（v${version.value}）`);
      return;
    }
    prompt(update);
  } catch (e) {
    if (manual) message.error(`检查更新失败：${String(e)}`);
    // 启动时的自动检查失败保持静默（离线/网络抖动不打扰）
  } finally {
    checking.value = false;
  }
}

function prompt(update: Update) {
  const prog = ref({ received: 0, total: 0, installing: false });
  const pct = () =>
    prog.value.total > 0
      ? Math.min(100, Math.round((prog.value.received / prog.value.total) * 100))
      : null;
  const d = dialog.warning({
    title: `发现新版本 v${update.version}`,
    content: () =>
      h("div", { style: "display:flex;flex-direction:column;gap:8px" }, [
        h(
          "div",
          { style: "font-size:13px" },
          `当前 v${version.value} → 新版本 v${update.version}`
        ),
        update.body
          ? h(
              "pre",
              {
                style:
                  "max-height:180px;overflow:auto;background:rgba(255,255,255,.05);border-radius:6px;padding:8px 10px;font-size:12px;line-height:1.6;white-space:pre-wrap;word-break:break-all;margin:0",
              },
              update.body
            )
          : null,
        prog.value.installing
          ? h(
              "div",
              { style: "font-size:12px;opacity:.8" },
              `下载安装中… ${pct() !== null ? `${pct()}%` : ""}`
            )
          : null,
      ]),
    positiveText: "立即更新并重启",
    negativeText: "下次再说",
    onPositiveClick: async () => {
      prog.value.installing = true;
      try {
        let received = 0;
        let total = 0;
        await update.downloadAndInstall((event) => {
          if (event.event === "Started") {
            total = event.data.contentLength ?? 0;
          } else if (event.event === "Progress") {
            received += event.data.chunkLength;
          }
          prog.value = { received, total, installing: true };
        });
        d.destroy();
        dialog.success({
          title: "更新已就绪",
          content: "应用即将重启以完成安装…",
          positiveText: "好的",
        });
        setTimeout(() => relaunch(), 2000);
      } catch (e) {
        prog.value.installing = false;
        message.error(`更新失败：${String(e)}`);
      }
      return false; // 由上面手动控制弹窗关闭
    },
  });
}

onMounted(async () => {
  version.value = await getVersion().catch(() => "");
  // 原生菜单「检查更新…」（mac 应用菜单 / win 菜单栏，快捷键 Cmd/Ctrl+U）
  try {
    unlistenMenu = await listen("menu-check-update", () => void checkUpdate(true));
  } catch {
    /* 事件通道不可用时忽略 */
  }
  if (props.autoCheck) setTimeout(() => void checkUpdate(false), 3000);
});

defineExpose({ check: checkUpdate, checking });
</script>

<template>
  <span style="display: none" />
</template>
