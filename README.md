# ServerDesk — 本地服务器管理工具（AI 安全运维助手）

**简体中文** | [English](README.en.md)

运行在个人电脑上的本地桌面应用：集中管理服务器资产（账密 / SSH 密钥），维持稳定 SSH 连接，
并通过标准 **MCP 协议**向 AI Agent 暴露**受限的**运维能力 —— AI 能做日常运维，
但拿不到凭据、执行不了高危操作。

`macOS 14+ (Apple Silicon)` · `Windows 10/11 (amd64)` · `Debian 12 / Ubuntu 22.04+ (amd64)` · `Tauri 2 (Rust)` · `Vue 3 / TypeScript` · `SQLite` · `AES-256-GCM`

---

## ✨ 核心特性

- 🗂️ **服务器资产管理** — 主机 / 端口 / 账密 / SSH 密钥集中录入，TCP 连通性一键测试
- 🔐 **主密码保险库** — Argon2 派生密钥 + OS Keychain + 自动锁定；凭据 AES-256-GCM 密文落盘，库中无明文
- 🔌 **稳定 SSH 连接** — 连接保活、断线自动重连、远程命令执行
- 🤖 **AI 安全执行管线** — 泄露防护 / 黑白名单 / 高危审批 / 全量审计 / 输出脱敏，七层防护
- 📦 **加密备份** — 手动导出/恢复（合并 / 覆盖两种模式）+ 定时自动备份（保留 N 份）
- 🖥️ **多平台** — macOS / Windows / Linux 原生打包（Linux 提供 .deb / .rpm / AppImage）

## 🏗️ 架构

```
┌─────────────────────────────┐        stdio (MCP)        ┌──────────────────┐
│  ServerDesk 桌面应用          │◄────────────────────────►│  AI Agent         │
│  Vue3 前端 + Rust 核心        │      16 个受限工具         │  (Claude/DSH/…)  │
│  ┌───────────────────────┐  │                           └──────────────────┘
│  │ 保险库 vault/crypto    │  │
│  │ 资产存储 store (SQLite)│  │
│  │ 安全管线 guard         │──┼──► 黑白名单 → 审批 → 审计 → 脱敏
│  │ SSH 通道 ssh/sftp     │  │
│  └───────────────────────┘  │
└─────────────┬───────────────┘
              │ SSH
   ┌──────────▼──────────┐   ┌──────────────┐
   │ 服务器 A (root@host) │   │ 服务器 B / C  │
   └─────────────────────┘   └──────────────┘
```

## 🤖 AI 运维能力（16 个 MCP 工具）

| 分类 | 工具 | 说明 |
|------|------|------|
| 资产 | `list_servers` | 列出已录入服务器（绝不返回密码/私钥） |
| 观测 | `server_status` | uptime / CPU / 内存 / 磁盘 / 负载 |
| 观测 | `list_files` / `read_file` | 目录列表、读取文件（敏感路径保护、自动脱敏） |
| 观测 | `tail_log` / `log_search` | 日志尾部 / 日志检索 |
| 观测 | `list_services` / `port_check` | systemd 服务状态、端口监听检查 |
| 执行 | `exec_command` | 单机执行（安全管线判定，高危需人工批准） |
| 执行 | `exec_batch` | 多机批量执行（一次审批、并发 5、逐台审计） |
| 文件 | `upload_file` / `download_file` | 上传 / 下载（大小与路径受限） |
| 文件 | `append_file` / `write_file` | 追加 / 覆盖写入（写前自动备份，需人工批准） |
| 回滚 | `list_file_backups` / `restore_file` | 列出写前备份 / 一键回滚 |

所有工具均经过 `guard.rs` 七层安全管线：凭据泄露防护 → 黑名单拦截 → 白名单校验 →
人工审批 → 执行审计 → 输出脱敏 → 限速限流。

## 🚀 快速开始（开发模式）

```bash
cd apps/desktop
pnpm install
pnpm tauri dev
```

生产构建：

```bash
cd apps/desktop
pnpm tauri build     # 安装包输出于 apps/desktop/src-tauri/target/release/bundle/
```

Rust 侧检查（workspace 根目录）：

```bash
cargo check
```

## 📁 目录结构

```
crates/shared/            # 跨进程共享类型（资产模型、MCP 工具契约）
apps/desktop/             # Tauri 主应用
  ├─ src/                 # Vue3 前端（api / stores / views）
  ├─ src-tauri/src/       # Rust 核心：store / crypto / vault / guard /
  │                       #   approvals / ssh / observe / ops / backup / commands
  └─ scripts/             # check_vue_imports.py
apps/mcp-sidecar/         # stdio MCP Server（AI 工具入口）
docs/                     # 方案与验收文档（plan.md、m1~m5、r1-r5、release.md）
.github/workflows/        # release.yml（发布 CI）
```

## 🗺️ 里程碑

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| **M1** | 资产管理 CRUD + 连接测试 | ✅ |
| **M2** | 主密码保险库 + SSH 连接/保活/重连/命令执行 | ✅ |
| **M3** | AI 安全执行管线 + MCP 工具（list_servers / exec_command / server_status） | ✅ |
| **M4** | 加密备份导出/恢复 + 定时自动备份 | ✅ |
| **M5** | 双平台打包 + 验收 | ✅ |
| **R1** | 观测五件套：list_files / tail_log / log_search / list_services / port_check | ✅ |
| **R2** | 文件能力：上传/下载/追加 + write_file 自动备份 + restore_file 回滚 | ✅ |
| **R3** | exec_batch 多机批量执行 | ✅ |
| **R4** | 安全治理：黑白名单双模式、只读模式、AI 限速、严格分组全审批 | ✅ |
| **R5** | AI 会话视图 + 变更记录回滚页 | ✅ |

## 🛡️ 安全基线

- 凭据 **AES-256-GCM** 加密落盘，数据库中无明文（M2 起升级为主密码保险库 + OS Keychain）
- 日志与工具输出**统一脱敏**，AI 结果中严禁出现凭据
- 高危命令**黑名单 + 人工审批流**（120s 超时自动拒绝）+ 全量审计日志
- 每台服务器独立「允许 AI 操作」开关；支持只读模式与 AI 限速
- AI 写文件前自动备份，支持一键回滚（R2/R5）

## 📚 文档

- 完整方案：[docs/plan.md](docs/plan.md)
- 里程碑验收：[docs/m1-acceptance.md](docs/m1-acceptance.md) ~ [docs/m4-acceptance.md](docs/m4-acceptance.md)、[docs/acceptance-final.md](docs/acceptance-final.md)
- AI 扩展验收：[docs/r1-r5-acceptance.md](docs/r1-r5-acceptance.md)
- 发布说明：[docs/release.md](docs/release.md)
- AI 协作指南：[AGENTS.md](AGENTS.md)

---

ServerDesk · 让 AI 安全地帮你管服务器。
