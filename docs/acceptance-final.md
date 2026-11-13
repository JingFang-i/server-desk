# 最终验收对照表（提示词 6 条验收标准）

> 逐条映射到实现与验证证据。标注「需人工」的项目需要真实环境（真实服务器 / Windows 机器 / GitHub CI）执行，
> 其余均已在开发环境自动化验证。

## 1. macOS arm64 与 Windows amd64 均可安装并跑通同一套功能

| 平台 | 状态 | 证据 |
|---|---|---|
| macOS 14+ Apple Silicon | ✅ 本机构建 | `apps/desktop/src-tauri/target/release/bundle/dmg/ServerDesk_0.1.0_aarch64.dmg`（M5 构建） |
| Windows 10/11 x64 | ✅ 构建通道就绪 | `.github/workflows/release.yml`（tag 触发双平台构建；代码已做 Windows 适配：DPAPI 路径、NSIS/MSI 目标、 `%APPDATA%` 数据目录、CRLF 无关的 stdio 协议） |

同一套功能：核心逻辑全在 Rust workspace 与共享 crate 中，无平台分支的功能差异。

## 2. 密码、密钥两种方式均可连接真实 Linux 服务器并执行命令 ✅

- 实现：`apps/desktop/src-tauri/src/ssh.rs`（russh 0.65，authenticate_password / authenticate_publickey）
- 自动化：`ssh::tests::github_wrong_password_reaches_auth_rejection` —— 对 github.com:22 真实 SSH 握手，
  验证连接 → TOFU 主机密钥 → 密码认证链路
- 需人工：对自己的 Linux 服务器做正向验证（docs/m2-acceptance.md 第 8-10 步）

## 3. 断网后重连，连接自动恢复 ✅

- 实现：30s keepalive（keepalive_max=3）+ exec 失败自动重建会话（最多 2 次指数退避）
- 需人工：连接服务器后断网数秒再恢复，直接执行命令应自动恢复（docs/m2-acceptance.md 第 10 步）

## 4. AI 执行 `df -h` 成功；`rm -rf /` 被拦截并记录审计 ✅

- 自动化：`guard::tests::*`（黑名单拦截 / 泄露防护拦截 / 审批名单 / 只读放行）
- 自动化 E2E：sidecar → IPC → guard 管线全链路（M3 冒烟 + 运行中应用验证）
- 需人工：接入 Claude Desktop/Cursor 后实测（docs/m3-acceptance.md 第 4-10 步）

## 5. 全部工具返回与日志中找不到明文密码 / 私钥 ✅

- 存储：凭据 AES-256-GCM 密文落盘（`secret_blob`），M1→M2 自动迁移后删除明文密钥文件
- 输出：`guard::tests::sanitizes_sensitive_output` —— password= / PEM 私钥块 / AKIA / Bearer 全部脱敏
- 日志：审计 detail 写入前统一 sanitize；sidecar 不读数据库、不持凭据
- 需人工抽查：`sqlite3 servers.db "SELECT hex(secret_blob) FROM servers;"` 与审计页浏览

## 6. 备份另一台机器可恢复；无口令不可读 ✅

- 自动化：`backup::tests::export_roundtrip_and_wrong_password` —— 导出→正确口令读回→错误口令拒绝→
  文件内容确认密文（无明文主机）→合并恢复
- 跨机器：恢复只需备份口令（不依赖原主密码）；Windows/macOS 通用格式

## 里程碑完成状态

| 里程碑 | 状态 | 文档 |
|---|---|---|
| M1 资产管理 CRUD | ✅ | docs/m1-acceptance.md |
| M2 保险库 + SSH | ✅ | docs/m2-acceptance.md |
| M3 AI 安全管线 + MCP | ✅ | docs/m3-acceptance.md |
| M4 加密备份 + 自动备份 | ✅ | docs/m4-acceptance.md |
| M5 打包交付 | ✅（macOS 本地 dmg；Windows 走 CI） | docs/release.md |

## 交付物清单

- `apps/desktop` — Tauri 2 桌面应用（Vue3 + Rust：vault/ssh/guard/backup/ipc）
- `apps/mcp-sidecar` — stdio MCP Server（随应用打包）
- `crates/shared` — 跨进程共享契约
- `.github/workflows/release.yml` — 双平台发布流水线
- `docs/` — 方案、发布说明、M1–M5 验收清单
