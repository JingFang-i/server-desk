# 审查问题修复记录（2026-11-23）

> 对应审查报告：[code-review-2026-11-23.md](code-review-2026-11-23.md)（Standards 12 条 + Spec 16 条）。
> 本轮把两轴发现全部落成代码，并把「需要真服务器才能确认」的部分单列成手工验收清单。

## 验证结果

| 验证 | 结果 |
|---|---|
| `cargo check --workspace` | 通过，0 警告 |
| `cargo test --workspace` | **36 项通过**（主应用 35 + sidecar 契约 1；新增 guard 5 项、text 2 项、store 会话/成功判定 1 项、fbackups 加密 1 项、下载留存 1 项） |
| `pnpm build`（含 `vue-tsc`） | 通过 |
| `python3 apps/desktop/scripts/check_vue_imports.py` | 从仓库根与 `apps/desktop` 均可运行，实测检查 9 个 .vue |
| `pnpm tauri build --bundles app` | 通过；sidecar 位于 `Contents/Resources/resources/serverdesk-mcp`（1.2MB，可执行） |
| CI 发布 v0.1.4 | 三平台矩阵全绿；Release 产物 7 件：dmg / app.tar.gz / AppImage / deb / rpm / x64-setup.exe / x64_en-US.msi |
| sidecar MCP 握手（新旧两个二进制） | `initialize` 正常；`tools/list` 返回 16 个工具且顺序与 `shared::MCP_TOOL_NAMES` 一致 |
| 真机只读冒烟（本地服务器 192.168.110.120） | 通过正在运行的 0.1.3 应用调用 `server_status`，exitCode=0、输出正常（验证 MCP→IPC→SSH 链路与脱敏） |

## Standards 轴

| # | 问题 | 修复 | 位置 |
|---|---|---|---|
| 1 | 审批摘要未脱敏就进系统通知 | 入队前统一 `guard.sanitize` + 限长；新增短选项/URL 内嵌口令的脱敏规则；审批卡、系统通知、应用内提醒、事件广播共用同一份摘要 | `ops::register_approval`、`guard::LeakGuard::build`、`notify.rs` |
| 2 | 只读模式误杀只读工具（`2>/dev/null` 被当写入） | 判定前剥离 `/dev/null` 重定向与 `2>&1`；写命令正则改为「命令词 + 边界」，补 cp/ln/install/python -c/find -delete 等 | `guard.rs::strip_null_redirections` / `WRITEISH_RE` |
| 3 | `backup_restore` 不校验解锁与可解密 | 恢复前要求解锁并取 DEK，逐条试解凭据，失败即拒绝、库不变 | `commands::backup_restore`、`backup::restore_payload` |
| 4 | 审计失败被静默吞掉 | 计数 + stderr；`audit_failures` 命令 + 审计页红色告警条 | `ops::write_audit`、`AuditView.vue` |
| 5 | 分组名三套口径 | 分组匹配统一 `lower(trim(...))`；白名单按子命令判定 | `store::list_by_group`、`guard::evaluate` |
| 6 | `truncate` 两份重复、摘要预算散落 | 新增 `text.rs`（truncate / fold_ws / summary + 统一预算常量），全部调用点改用 | `text.rs` |
| 7 | `GroupRow` 伪造 ServerConfig | 拆成 `GroupRow \| ServerConfig` 联合类型 + `isGroupRow` 收窄 + `serverCell` 公共占位 | `ServersView.vue` |
| 8 | 批量执行复制凭据解密 | 抽出 `conn_params_from_row` 复用 | `ops.rs` |
| 9 | action/outcome 裸字符串 | 动作名收进 `shared::action::*`（单一来源，Rust 全部改用常量）；成功与否由后端 SQL 计算 `ok`；前端改用 `AuditEntry.ok` 并给 action 加类型联合 | `shared/lib.rs`、`store.rs`、`api/index.ts`、`AuditView.vue` |
| 10 | 变更记录页仍是裸 id | 拉取服务器列表做 id→名称映射，备份/变更两张表都显示名称（小字 id） | `ChangeLogView.vue` |
| 11 | Vue 导入检查空跑 | 脚本按自身路径定位 `src/**/*.vue`，无文件时报错退出（exit 2） | `scripts/check_vue_imports.py` |
| 12 | sidecar 头注释写 3 个工具、契约未落 shared | 头注释改为 16 个；`shared::MCP_TOOL_NAMES` + sidecar 单测逐一比对 | `shared/lib.rs`、`apps/mcp-sidecar` |

## Spec 轴

| # | 问题 | 修复 | 位置 |
|---|---|---|---|
| 1 | 安装包不含 sidecar | `bundle.resources` 映射到 `resources/` 子目录（映射到根目录会让 WiX 把 serverdesk-mcp.exe 登记成两个组件，MSI 打包 ICE30 失败）；`beforeBuildCommand` 编译并暂存 sidecar；release.yml 增加暂存步骤；v0.1.4 三平台产物已实测齐全 | `tauri.conf.json`、`scripts/stage-sidecar.mjs`、`release.yml` |
| 2 | 自动锁默认关、旧默认被静默迁成 0 | 默认回到 10 分钟；删除把 `"10"` 改写成 `0` 的迁移 | `vault.rs` |
| 3 | 批量并发 5 未生效 | `acquire_owned()` 后才发远程操作，许可持有到该台结束 | `ops::exec_batch` |
| 4 | `list_file_backups` 无 allow_ai 闸门 | 指定服务器时先校验 allow_ai；未指定时只返回开放 AI 的服务器的备份 | `ipc.rs` |
| 5 | 会话筛选只在前端最近 300 行 | `audit_list` 增加 session 参数（SQL 过滤）+ `audit_sessions` 供下拉 | `store.rs`、`commands.rs`、`AuditView.vue` |
| 6 | 无跨库恢复测试 | `restore_payload` 增加 DEK 校验；用例断言「另一个 DEK 恢复必须失败且不写库」 | `backup.rs` |
| 7 | 下载件明文长期留存 | 下载后与启动时清理 `ai_downloads` 中超过 7 天的文件 | `ops.rs`、`main.rs` |
| 8 | 规则整表覆盖不可追溯 | 审计写入规则差异（deny/approval/whitelist 增删、模式与限流变化） | `commands::guard_set_rules` |
| 9 | `read_key_file` 能力面过宽 | 增加 64KB 体积上限（保留 PRIVATE KEY 标记校验） | `commands.rs` |
| 10 | 跨机恢复报成功 | 同 #3：写库前校验凭据可解密 | `backup.rs` |
| 11 | 观测五件套绕过 guard | 固定模板统一走 `evaluate_fixed_template`（泄露防护/黑名单/只读写入），严格分组下追加人工审批 | `observe.rs`、`guard.rs` |
| 12 | 只读模式正则缺 cp/ln/perl/python | 写入特征补齐；受限模式下拒绝命令替换与进程替换 | `guard.rs` |
| 13 | 改配置不淘汰旧会话 | `save_server` 改为 async，更新成功后断开该服务器会话 | `commands.rs` |
| 14 | SSH 读超时仍返回成功 | `ExecResult.timed_out`；单机结果带 `timedOut`，批量 `ok=false` 并保留部分输出 | `ssh.rs`、`ops.rs`、`ipc.rs` |
| 15 | `String::truncate` 可 panic、输出先收后截 | 改为字节缓冲边收边限长（512KB），按 lossy 解码，不再字节切串 | `ssh.rs` |
| 16 | append_file 先读后整写 | SFTP `WRITE|APPEND`（不 truncate、不 create）真追加，审批期间的新内容不会被覆盖 | `ssh.rs::sftp_append_file`、`ops::ai_append_file` |
| 17 | 路径保护只判输入文本 | 远端 `readlink -f/-m` 解析真实路径，对解析结果再判黑名单（读与写都过） | `ops::resolve_remote_path` |
| 18 | 分组树混并「默认分组」、折叠被重置 | 未分组使用独立内部键 `g:__ungrouped__`；只对「新出现」的分组默认展开 | `ServersView.vue` |

## 行为变化（需要知道）

1. **白名单模式改为按子命令匹配**：`uptime; cp a b` 这类复合命令，任何一段未命中即整体拒绝；纯 `echo` 标签段放行。
2. **观测五件套与状态探查不套用白名单/审批表**（它们是应用内置只读模板），但仍受泄露防护、硬黑名单、只读写入检查；严格分组下会要求审批。
3. **受限模式拒绝命令替换**：只读/白名单开启时，`$(...)`、反引号、`<(...)` 一律拒绝。
4. **旧明文 `.bak` 仍可回滚**，新备份一律 DEK 加密（列表里标「已加密 / 旧明文」）。
5. **依赖 shell `readlink` 解析路径**：对端不支持时会退回原路径判定（行为与修复前一致，不报错）。

## 已知取舍（不是遗漏）

1. **路径真实化依赖远端 `readlink`**：GNU/BSD 都支持 `-f`（存在）/`-m`（GNU）；两者都不可用时退回原路径判定，等价于修复前行为，不会误拦。
2. **白名单按文本切分子命令**：不解析引号，`grep 'a;b'` 会被切成两段而多判一次。方向上偏严格（可能误拒），不会放宽。
3. **历史审计里的旧动作字符串**原样展示，不迁移数据；新写入一律走常量。
4. **自动锁**：新装用户默认 10 分钟；已经被历史版本迁移成「从不」的用户保持其当前设置（不再静默改写），需要时在设置页改回。

## 仍需真服务器/人工确认

本轮所有 Rust 逻辑与前端构建都有自动化验证，但下列行为无法在没有连接凭据的情况下触发：

- 系统通知与提示音的实际弹出（macOS 通知权限、勿扰模式）
- SFTP 真追加（`WRITE|APPEND`）对目标文件的追加效果
- `readlink` 解析后拦截符号链接别名（需要一台能建软链的测试机）
- 严格分组下观测工具触发审批、批量并发上限 5 的实际节奏
- 旧库升级后审计页的会话筛选项与名称兜底

## 手工验收（本地服务器，只读操作）

1. 用升级前数据库启动 → 审计页正常；会话下拉有历史会话；服务器列有名称。
2. 触发一条需审批操作（如 `systemctl status nginx` 命中审批名单）→ 系统通知正文只显示脱敏摘要（可用 `app --password=xxx` 验证出现 `[REDACTED]`）。
3. 设置里打开只读模式 → `server_status` 仍可用；`uptime; cp /etc/passwd /tmp/x` 被拒。
4. 建一个指向受保护文件的软链接，让 AI `read_file` 该链接 → 应被拦截并提示解析后的真实路径。
5. 批量执行 6 台 → 观察执行节奏不超过 5 路。
6. `pnpm tauri build --bundles app` 后，设置页 MCP 配置里的路径应指向 `ServerDesk.app/Contents/Resources/serverdesk-mcp`。
