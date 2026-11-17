# AGENTS.md — ServerDesk 项目 AI 协作指南

本文件供 AI Agent（Claude Code / DSH / Cursor 等）在本仓库工作时使用。
项目本体见 [README.md](README.md)，完整方案见 [docs/plan.md](docs/plan.md)。

## 项目是什么

**ServerDesk**：运行在个人电脑上的本地服务器管理工具（macOS 14+ / Windows 10+ / Debian 12+），
集中管理服务器资产（账密 / SSH 密钥），并通过标准 MCP 协议向 AI Agent 暴露
**受限的**运维能力 —— AI 能做日常运维，但拿不到凭据、执行不了高危操作。

- 技术栈：Tauri 2（Rust）+ Vue 3 / TypeScript + SQLite + AES-256-GCM
- 语言约定：代码、注释、文档、commit message 均使用**中文**

## 目录结构

```
crates/shared/            # 跨进程共享类型（资产模型、MCP 工具契约）——单文件 lib.rs
apps/desktop/             # Tauri 主应用
  ├─ src/                 # Vue3 前端（api/ + stores/ + views/）
  │   └─ views/           # ServersView / ApprovalsView / AuditView / BackupView
  │                       #   / ChangeLogView / SettingsView / LockGate
  ├─ src-tauri/src/       # Rust 核心逻辑（见下「关键模块」）
  └─ scripts/             # check_vue_imports.py（Vue 导入检查）
apps/mcp-sidecar/         # stdio MCP Server（AI 工具入口，当前单文件 main.rs）
docs/                     # 方案与各里程碑验收文档（plan.md、m1~m5、r1-r5、release.md）
.github/workflows/        # release.yml（发布 CI）
```

## 关键模块（apps/desktop/src-tauri/src/）

| 模块 | 职责 | 改动须知 |
|------|------|---------|
| `store.rs` | SQLite 资产存储 | 凭据字段必须密文落盘，禁止存明文 |
| `crypto.rs` / `vault.rs` | AES-256-GCM 加密；主密码保险库（Argon2 + OS Keychain + 自动锁） | 加密逻辑改动需重新评估备份兼容性 |
| `guard.rs` | AI 安全管线核心（泄露防护/黑名单拦截/审批/审计/脱敏，七层） | **新增任何 MCP 能力必须先过这里** |
| `approvals.rs` | 高危命令人工审批流（120s 超时自动拒绝） | |
| `ssh.rs` | SSH 连接/保活/重连/命令执行（russh） | |
| `observe.rs` / `ops.rs` | 观测五件套与文件操作（R1/R2） | 输出统一脱敏、限行数/限大小 |
| `backup.rs` / `fbackups.rs` | 加密备份导出/恢复；AI 写文件前的自动备份与回滚 | |
| `commands.rs` / `ipc.rs` | Tauri IPC 命令层 | |

`apps/mcp-sidecar/src/main.rs`：stdio MCP Server，共 **16 个工具**
（list_servers / server_status / exec_command / exec_batch / list_files /
read_file / tail_log / log_search / list_services / port_check /
upload_file / download_file / append_file / write_file / list_file_backups / restore_file）。
新增工具时同步更新：sidecar `main.rs` → `crates/shared` 契约 → `guard.rs` 管线 → README 工具数 → docs 验收文档。

## 常用命令

```bash
# 前端开发模式（含 Tauri 窗口）
cd apps/desktop && pnpm install && pnpm tauri dev

# 仅构建前端 + TS 类型检查
cd apps/desktop && pnpm build        # = vue-tsc --noEmit && vite build

# Rust 侧检查 / 构建（workspace 根目录）
cargo check
cargo build --release

# 生产打包（输出 src-tauri/target/release/bundle/）
cd apps/desktop && pnpm tauri build

# Vue 导入静态检查
python3 apps/desktop/scripts/check_vue_imports.py
```

注意：Rust workspace 包含 `crates/shared`、`apps/desktop/src-tauri`、`apps/mcp-sidecar` 三个成员，
`target/` 在仓库根目录；不要在子目录里单独 `cargo init` 或引入新的 workspace。

## 安全红线（违反即回滚）

1. **任何代码路径都不得输出明文凭据**：密码、私钥、主密码不得进入日志、审计、AI 返回结果、错误信息。
2. AI 可达的每条命令/文件路径必须经过 `guard.rs` 安全管线（黑/白名单、脱敏、审计），不允许绕过。
3. 高危命令（rm -rf、mkfs、shutdown、凭据读取类等）必须走人工审批，不得默认放行。
4. 每台服务器独立 `allow_ai` 开关；关闭时 AI 一律拒绝。
5. 新增 MCP 工具必须：有只读/受限的默认行为、限流限大小、逐次审计、输出脱敏。
6. 保险库加密结构变更（`crypto.rs`/`vault.rs`）必须同步设计备份导出/恢复的兼容策略。

## 验证清单（交付前）

- [ ] `cargo check` 通过（workspace 根目录）
- [ ] `pnpm build` 通过（含 vue-tsc 类型检查）
- [ ] 涉及 MCP 工具的改动：用真实服务器（本地测试机）跑一遍工具调用，确认审批/脱敏/审计行为正常
- [ ] 涉及加密/备份的改动：验证旧备份文件能否恢复（或明确声明不兼容及迁移方案）
- [ ] 新功能补验收文档到 `docs/`（沿用 m1-acceptance.md / r1-r5-acceptance.md 的格式）

## 其他约定

- 文档一律放 `docs/`，里程碑验收按 `mx-acceptance.md` / 功能验收按 `rN-rM-acceptance.md` 命名。
- `target/`、`node_modules/`、`dist/`、`*.db` 为构建产物，勿提交、勿作为分析依据。
- 修改前端后若窗口未热更，重启 `pnpm tauri dev`（部分 Rust 侧改动必须重启）。
