# M2 验收：主密码保险库 + SSH 连接

> M1 的资产数据将自动迁移进保险库（首次设置主密码时），无需手工操作。

## 运行说明

```bash
cd apps/desktop
pnpm install        # 首次
pnpm tauri dev
```

数据目录：`~/Library/Application Support/com.qsmx.serverdesk/`
（macOS；Windows 为 `%APPDATA%/com.qsmx.serverdesk/`）

自动化验证（无需 GUI）：

```bash
cargo test -p serverdesk
# ssh::tests::github_wrong_password_reaches_auth_rejection —— 真实 SSH 握手 + 认证被拒分支
```

## 本里程碑新增

| 模块 | 说明 |
|---|---|
| 保险库 vault | 主密码（Argon2id，19MiB/t=2/p=1）派生 KEK 包裹随机 DEK；AES-256-GCM 加密凭据；内存密钥 Zeroizing 清零 |
| 空闲自动锁 | 默认 10 分钟无凭据访问自动锁定（设置页可调 5/10/30/60/从不）；锁定时前端实时回到锁屏 |
| 钥匙串自动解锁 | 可选：DEK 镜像存入 macOS Keychain / Windows 凭据管理器，启动免输主密码（默认关闭） |
| M1 数据迁移 | 设置主密码/首次解锁时自动将 M1 过渡密钥加密的旧凭据重加密入库，成功后删除旧密钥文件 |
| SSH 连接池 | russh 0.65；30s keepalive；断线自动重连（最多 2 次退避重试）；命令执行（默认 30s 超时、输出 512KB 截断） |
| TOFU 主机密钥 | 首次连接记录 SHA256 指纹，指纹变更即拒绝连接（防中间人），UI 提供显式「重置指纹」 |
| 前端 | 锁屏（设置/解锁/重置流程）、设置页、连接状态列（5s 刷新）、连接/断开、命令执行器（stdout/stderr/exit code）、SSH 完整认证测试 |

## 验收步骤

| # | 步骤 | 预期 |
|---|---|---|
| 1 | 启动应用（已有 M1 数据） | 出现锁屏「欢迎使用」：设置主密码（≥8 位） |
| 2 | 设置主密码后进入主界面 | 提示「已将 N 条 M1 旧凭据迁移进主密码保险库」；服务器列表完整 |
| 3 | `ls "$HOME/Library/Application Support/com.qsmx.serverdesk/"` | `m1-interim.key` 已删除 |
| 4 | 锁定（设置页「立即锁定」）或等待自动锁 | 全屏回到锁屏，需输主密码 |
| 5 | 输错主密码解锁 | 提示「主密码错误」 |
| 6 | 正确密码解锁 | 进入主界面 |
| 7 | 「编辑」任意服务器，查看密码/私钥 | 正确回显（DEK 解密正常） |
| 8 | 对真实 Linux 服务器点「SSH 测试」 | 提示「SSH 认证成功 + 延迟」；首次连接记录指纹 |
| 9 | 对同一服务器点「连接」→「命令」执行 `uptime` | 已连接标签变绿；stdout/exit=0 返回 |
| 10 | 执行 `sleep 40` | 60s 内返回或按超时提示（默认命令超时 60s，后端单命令上限 300s） |
| 11 | 服务器重装场景模拟：点「重置指纹」后重连 | 指纹重新记录，连接成功 |
| 12 | 设置页开启「钥匙串自动解锁」→ 完全退出重开 | 免密自动进入主界面；关闭开关后恢复需输主密码 |
| 13 | 忘记密码路径：锁屏「忘记主密码」→ 输入 RESET 确认 | 全部数据清空，回到初始设置（危险操作，验证后重新录入或从测试数据恢复） |
| 14 | 数据库检查 | `sqlite3 servers.db "SELECT hex(secret_blob) FROM servers;"` 仍为密文；meta 表含 `vault_salt`/`vault_wrapped_dek` |

## 已知边界（M3 补齐）

- `ssh_exec` 目前记录审计（source=manual），M3 起承接 AI 调用并叠加高危拦截/审批/脱敏管线
- 断线自动重连上限 2 次；更长退避与「网络恢复主动重连」可按需在 M3 调整
