# ServerDesk 技术方案（已确认）

> 本地服务器管理工具 · AI 安全运维助手
> 状态：方案已确认，按里程碑实施中

## 1. 总体架构

**Tauri 2（Rust 后端）+ Vue 3 / TypeScript 前端**，外加一个独立的 **MCP sidecar 二进制**（Rust）。

```
┌─────────────────────────────────────────────────┐
│  桌面应用（Tauri 2）                              │
│  ┌───────────┐  ┌──────────────────────────────┐ │
│  │ Vue3 前端  │─▶│ Rust 核心                     │ │
│  │ 服务器管理 │  │  vault   主密码+Argon2+AES-GCM │ │
│  │ 审批弹窗   │  │  store   SQLite（凭据密文落盘） │ │
│  │ 审计/备份 │  │  ssh     russh 连接池/保活/重连 │ │
│  └───────────┘  │  guard   高危拦截/审批/审计/脱敏│ │
│                 │  ipc     本地 IPC（鉴权）       │ │
│                 └──────────┬───────────────────┘ │
└────────────────────────────┼─────────────────────┘
                             │ 127.0.0.1 IPC + token
┌────────────────────────────▼─────────────────────┐
│  mcp-sidecar（stdio MCP Server，独立二进制）        │
│  list_servers / exec_command / server_status ...  │
│  ◀── Claude Desktop / Cursor 等外部 AI 客户端接入   │
└───────────────────────────────────────────────────┘
```

**关键设计决策：凭据只存在于主应用进程内。**
sidecar 是薄协议桥：AI 调用工具 → sidecar 通过带 token 鉴权的本地 IPC 转发给主应用 →
主应用经安全管线（拦截 → 审批 → 审计 → 脱敏）执行 → 返回结果。
AI 全程接触不到任何凭据。

## 2. 安全设计（P0）

| 层 | 机制 |
|---|---|
| 存储 | SQLite 存元数据 + AES-256-GCM 密文；数据密钥 DEK 由主密码 Argon2id 派生 KEK 包裹后落盘；DEK 镜像存 OS Keychain/DPAPI（可选自动解锁）；空闲自动锁库；`zeroize` 清内存 |
| 命令拦截 | 内置高危黑名单（rm -rf /、mkfs、dd、shutdown/reboot、userdel、sshd 配置、关防火墙、chmod -R 777、curl\|bash 等，用户可维护）；每服务器「允许 AI」开关 + 命令白/黑名单 |
| 审批流 | 黑名单未覆盖的破坏性操作 → 主应用弹窗人工确认（外部 MCP 调用超时自动拒绝） |
| 防泄露 | 工具返回与日志统一脱敏（PEM 私钥块、password= 等模式）；默认拦截 `env` / `printenv` / 读取私钥文件类命令 |
| 审计 | SQLite 审计表：时间、来源（AI/人工）、服务器、命令、结果摘要、拦截/审批记录 |

## 3. 项目结构

```
server-mangement/
├── Cargo.toml              # workspace
├── crates/shared/          # 共享类型：ServerConfig、工具 schema、IPC 协议
├── apps/desktop/           # Tauri 主应用
│   ├── src-tauri/          # Rust：store / crypto / (M2: vault, ssh) / (M3: guard, ipc) / commands
│   └── src/                # Vue3：服务器管理、审批中心、审计、备份、设置
├── apps/mcp-sidecar/       # stdio MCP Server（Rust 独立 bin）
└── docs/                   # 方案文档 + 各里程碑验收步骤
```

核心依赖：`tauri 2`、`russh`、`rusqlite(bundled)`、`argon2`、`aes-gcm`、`keyring`、`zeroize`、`tokio`；
MCP 用官方 Rust SDK（rmcp），不适用则手写 JSON-RPC。

## 4. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| M1 | workspace 骨架 + 服务器资产 CRUD（账密/密钥录入、分组、搜索、连接测试 UI） | 可运行，资产可增删改查 |
| M2 | 主密码保险库（Argon2+AES-GCM+Keychain+自动锁）+ russh 连接/保活/断线重连/命令执行 | 真实 Linux 服务器密码、密钥两种方式连通并执行命令，断网自动恢复 |
| M3 | 安全执行管线（拦截/审批/审计/脱敏）+ MCP sidecar 工具 | AI 执行 `df -h` 成功；执行 `rm -rf /` 被拦截并留审计；全量搜索无明文凭据 |
| M4 | 加密备份导出/恢复（覆盖/合并）+ 定时自动备份保留 N 份 | 备份文件无口令不可读，另一机器可恢复 |
| M5 | 双平台打包（macOS dmg / Windows msi-nsis）+ 交付验收清单 | 按需求 6 条验收标准逐项核对 |

## 5. 已确认的关键决策

- 桌面技术栈：**Tauri 2 + Vue 3 + TypeScript**
- AI 接入：**独立 stdio MCP Server**（外部客户端 Claude Desktop / Cursor 等配置接入）
- 内嵌终端：**暂不做**（P1，M5 后按需追加）
- M1 连接测试为 TCP 层探测；完整 SSH 认证测试在 M2 提供
