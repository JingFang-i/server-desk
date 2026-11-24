# R7 验收：应用内在线更新

> 前置：M1–M5、R1–R6 已交付，v0.1.5 已发布。本轮给应用加上「检查新版本 → 提示 → 下载安装 → 重启」的完整闭环。

## 需求

1. 应用**自动检查**新版本（启动后静默检查一次），发现新版本就**弹窗提示**。
2. 用户自己点击「立即更新」→ 应用内**下载**（带进度）→ **安装** → 自动重启完成升级。
3. 设置页提供「检查更新」按钮（手动模式会反馈「已是最新」或失败原因）。

## 实现要点

| 层 | 行为 |
|---|---|
| 更新源 | GitHub Release 的 `latest.json`（固定 URL：`releases/latest/download/latest.json`，tauri-action 发布时生成） |
| 签名 | 更新包用 minisign 密钥签名：公钥在 `tauri.conf.json → plugins.updater.pubkey`，私钥在 GitHub Secrets `TAURI_SIGNING_PRIVATE_KEY`（空密码） |
| 产物 | `bundle.createUpdaterArtifacts = true` → 本地/CI 构建生成 `.app.tar.gz` / `-setup.exe` / `.AppImage` 及对应 `.sig` |
| Rust | 注册 `tauri-plugin-updater` 与 `tauri-plugin-process`（重启）；capabilities 加 `updater:default`、`process:default` |
| 前端 | `components/UpdatePrompt.vue`：启动 3s 静默检查（失败静默）；弹窗含版本对比 + 更新说明 + 下载进度；装完自动 `relaunch()` |
| 设置页 | 「应用更新」卡片：当前版本 + 「检查更新」按钮（手动模式反馈成功/失败） |

## 改动文件

| 文件 | 内容 |
|---|---|
| `Cargo.toml` ×2 | 新增 `tauri-plugin-updater`、`tauri-plugin-process` |
| `apps/desktop/src-tauri/src/main.rs` | 注册两个插件 |
| `tauri.conf.json` | `createUpdaterArtifacts`、updater 端点与公钥 |
| `capabilities/default.json` | `updater:default`、`process:default` |
| `.github/workflows/release.yml` | tauri-action 注入签名密钥环境变量 |
| `src/components/UpdatePrompt.vue` | 检查/提示/下载进度/重启（`check(manual)` 供设置页复用） |
| `src/App.vue`、`src/views/SettingsView.vue` | 自动检查入口、设置页「应用更新」卡片 |
| `.gitignore` | 忽略 `.updater-keys/`（本地签名私钥） |

## 密钥与安全

- 私钥：本地 `.updater-keys/serverdesk-updater.key`（已 gitignore，**丢失则无法再签发更新**，请备份）；CI 从 Secrets 读取。
- 密码为空，`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 留空即可。
- 所有更新包下载后先验签再安装，签名不符会直接拒绝——这是防「中间人替换安装包」的关键。

## 自动化验证

- `cargo check --workspace` / `cargo test --workspace`（36 项）通过
- `pnpm build`（vue-tsc + vite）通过；`check_vue_imports.py` 通过（10 个 .vue）
- 本地签名打包实测：生成 `ServerDesk.app.tar.gz` + `ServerDesk.app.tar.gz.sig`

## 已知边界

- **首次启用需要跨一版**：v0.1.5 及更早的安装没有更新器，需手动安装一次 v0.1.6；从 v0.1.6 起应用内更新可用。
- Linux：仅 AppImage 安装支持应用内更新；deb/rpm 用户继续用包管理器升级（应用内检查会明确报错，不影响其他功能）。
- Windows：NSIS 安装走静默升级；MSI 安装的用户更新时会跳转到 NSIS 安装器（Tauri 标准行为）。
- macOS：更新包为 `.app.tar.gz`，重启后替换 .app；未签名/未公证的 dmg 首次打开仍需右键放行。
- 更新检查走 GitHub，被墙/离线时静默失败（手动检查会报错）。

## 手工验收步骤

1. 用 v0.1.6 安装包安装并启动 → 3 秒后无弹窗（当前已是最新）。
2. 设置 → 应用更新：显示当前版本；点「检查更新」→ 「已是最新版本」。
3. 发布一个更高版本（如 v0.1.7，tauri-action 会生成 latest.json 与签名产物）→ 重启 v0.1.6 应用 → 弹窗显示新版本号与更新说明。
4. 点「立即更新并重启」→ 显示下载进度 → 自动重启 → 设置页版本变为新版本。
5. 断网点「检查更新」→ 报「检查更新失败」，应用其余功能不受影响。
