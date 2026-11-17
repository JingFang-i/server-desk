# ServerDesk — 本地服务器管理工具（AI 安全运维助手）

运行在个人电脑上的本地应用：集中管理服务器资产（账密 / SSH 密钥）、维持稳定 SSH 连接，
并通过标准 MCP 协议向 AI Agent 暴露**受限的**运维能力 —— AI 能做日常运维，
但拿不到凭据、执行不了高危操作。

- 支持平台：macOS 14+（Apple Silicon / aarch64）、Windows 10/11（amd64）
- 技术栈：Tauri 2（Rust）+ Vue 3 / TypeScript + SQLite + AES-256-GCM
- 详细方案：[docs/plan.md](docs/plan.md)

## 快速开始（开发模式）

```bash
cd apps/desktop
pnpm install
pnpm tauri dev
```

生产构建：`pnpm tauri build`（当前平台安装包输出于 `src-tauri/target/release/bundle/`）。

## 目录结构

```
crates/shared/        # 跨进程共享类型（资产模型、工具契约）
apps/desktop/         # Tauri 主应用（Vue3 前端 + Rust 核心）
  └─ src-tauri/src/   # store(SQLite) / crypto(凭据加密) / commands(IPC)
apps/mcp-sidecar/     # stdio MCP Server（M3 实现）
docs/                 # 方案与各里程碑验收文档
```

## 里程碑

- **M1** ✅ 资产管理 CRUD + 连接测试（TCP）
- **M2** ✅ 主密码保险库（Argon2 + Keychain + 自动锁）+ SSH 连接/保活/重连/命令执行
- **M3** ✅ AI 安全执行管线（泄露防护/拦截/审批/审计/脱敏）+ MCP 工具（list_servers / exec_command / server_status）
- **M4** ✅ 加密备份导出/恢复（合并/覆盖）+ 定时自动备份（保留 N 份）
- **M5** 双平台打包 + 验收

## AI 运维扩展（R1–R5）✅

- **R1** 观测五件套：list_files / tail_log / log_search / list_services / port_check
- **R2** 文件能力：AI 上传/下载/追加 + write_file 自动备份 + restore_file 回滚
- **R3** exec_batch：多服务器批量执行（一次审批、并发 5、逐台审计）
- **R4** 安全治理：黑名单/白名单双模式、只读模式、AI 限速、严格分组全审批（设置页可配）
- **R5** AI 会话视图 + 变更记录回滚页

AI 共 **16 个 MCP 工具**，全部经七层安全管线（详见 docs/r1-r5-acceptance.md）。

## 安全基线

- 凭据 AES-256-GCM 加密落盘，数据库中无明文（M2 起升级为主密码保险库 + OS Keychain）
- 日志与工具输出统一脱敏，AI 结果中严禁出现凭据
- 高危命令黑名单 + 人工审批流 + 全量审计日志（M3 起）
- 每台服务器独立「允许 AI 操作」开关
