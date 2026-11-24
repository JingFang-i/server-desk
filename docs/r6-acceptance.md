# R6 验收：审批到达提醒 + 审计服务器名称

> 前置：M1–M5、R1–R5 已交付。本轮补两处日常使用体验：审批不再「静默」等待，审计不再只看到 UUID。

## 需求

1. 需要人工审批时发送**通知 + 声音提醒**（AI 提交高危操作后，用户可能不在应用窗口前）。
2. 审计日志显示**具体服务器名称**，而不是服务器 id。

## 实现要点

### 1. 审批到达提醒（Rust + 前端双层）

| 层 | 行为 |
|---|---|
| 系统通知 | `tauri-plugin-notification`；标题「ServerDesk · AI 操作待审批」，正文含服务器名与命令摘要（≤140 字、换行折叠），`.sound("Glass")` 带提示音 |
| 窗口提示 | `request_user_attention(Critical)`：macOS 程序坞跳动 / Windows 任务栏闪烁 |
| 点击通知 | 恢复并聚焦主窗口（`notify::init` 注册 `on_action`） |
| 应用内 | `ApprovalAlert.vue` 监听 `approval-requested` → Web Audio 三音提示 + 应用内通知（含「去审批中心」按钮） |
| 角标 | 侧边栏「审批中心 (N)」随待审批数量变化 |
| 到点提醒 | 待审批未处理期间每 30 秒再响一次（审批 120 秒后自动拒绝，避免静默过期） |

关键约束：

- 审批入口**统一**走 `ops::register_approval`（单机命令、文件写入/上传/追加/回滚、批量执行），
  入队 → 广播 `approvals-updated` → 广播 `approval-requested` → 发送系统通知，杜绝漏发。
- 通知正文与审批卡使用同一条已脱敏摘要，**不含凭据**：`ops::register_approval` 入队前统一
  `guard.sanitize`（含 `-pSecret`、`--password=...`、`https://user:pass@host` 形态），再限长；
  系统通知、应用内提醒、事件广播都只读这一份；发送失败只写 stderr，不影响审批流程。
- 提示音不引入音频资源：Web Audio 实时合成；首次指针/键盘交互时解锁音频上下文（自动播放策略），
  1.2 秒内重复触发合并，避免一次审批叠音。

### 2. 审计日志显示服务器名称

- `audit_log` 新增 `server_name` 列（`TEXT NOT NULL DEFAULT ''`，旧库启动时自动 `ALTER TABLE`），
  写入时保存**名称快照**：服务器改名/删除后历史记录仍能看懂。
- `ops::audit` 统一解析服务器引用（MCP 工具允许传 id 或名称）→ 规范 id + 名称；解析不到时原样保留引用。
- 删除服务器前先取名称（`ops::audit_named`），删除动作本身也能显示名称；`exec_batch` 以「批量 ×N 台」作为名称快照。
- 查询时 `LEFT JOIN servers` 兜底：升级前的历史记录名称为空但服务器仍在 → 自动补当前名称；
  服务器已删除且无快照 → 前端回退显示 id（不显示空白）。
- 前端：列表「服务器」列显示名称（小字附 id）；详情弹窗拆「服务器 / 服务器 ID」两行；复制详情包含名称。

## 改动文件

| 文件 | 内容 |
|---|---|
| `Cargo.toml` / `apps/desktop/src-tauri/Cargo.toml` | 新增 `tauri-plugin-notification = "2"` |
| `apps/desktop/src-tauri/src/notify.rs` | 新增：系统通知、声音、窗口注意力、点击唤醒 |
| `apps/desktop/src-tauri/src/main.rs` | 注册通知插件；启动时注册通知点击回调 |
| `apps/desktop/src-tauri/capabilities/default.json` | 增加 `notification:default`（插件 init 脚本会检查授权，避免控制台报错） |
| `apps/desktop/src-tauri/src/ops.rs` | `register_approval` 统一审批入口；`audit` 解析服务器名、新增 `audit_named` |
| `apps/desktop/src-tauri/src/store.rs` | `server_name` 列 + 迁移 + 写入/查询（含 `LEFT JOIN` 兜底） |
| `apps/desktop/src-tauri/src/commands.rs` | 删除服务器前取名称快照 |
| `apps/desktop/src/alert.ts` | 新增：Web Audio 提示音 |
| `apps/desktop/src/components/ApprovalAlert.vue` | 新增：全局审批提醒（声音 / 应用内通知 / 角标 / 30 秒提醒） |
| `apps/desktop/src/App.vue` | 通知 Provider、审批角标 |
| `apps/desktop/src/views/AuditView.vue` | 服务器名称列、详情、复制 |
| `apps/desktop/src/api/index.ts` | `AuditEntry.serverName` |

## 自动化测试

`cargo test --workspace`：**32/32 通过**（2026-11-23 修复轮后为 32 项，另含 sidecar 契约 1 项）。R6 新增两项：

- `store::tests::audit_server_name_snapshot_and_legacy_migration`
  —— 旧库（无 `server_name` / `session` 列）自动迁移；名称快照写入与读出一致。
- `ops::r6_tests::audit_server_reference_resolves_to_id_and_name`
  —— 按名称/按 id 引用均解析为「规范 id + 名称」；名称提示优先；解析不到保留原引用；批量操作只有名称。

前端：`pnpm build`（vue-tsc 类型检查 + vite 构建）通过；`check_vue_imports.py` 通过。

## 手工验收步骤

1. `cd apps/desktop && pnpm tauri dev`，解锁保险库并开启 AI 接入。
2. 通过 MCP 触发一条需审批操作（如 `systemctl restart nginx`，或 `write_file`）：
   - 期待：系统通知弹出并发出提示音；应用内右上角出现提醒；侧边栏「审批中心 (1)」；
     应用在后台时程序坞/任务栏出现注意力提示。
   - 点击系统通知 → 主窗口回到前台。
   - 120 秒内不处理 → 自动拒绝，角标与提醒停止。
3. 分别「拒绝」「批准」，确认命令按预期不执行/执行。
4. 打开「审计日志」：审批请求、批准/拒绝、执行记录均出现，**服务器列显示服务器名称**（小字为 id）。
5. 打开任一条详情 → 服务器 / 服务器 ID 分行展示；「复制详情」包含名称。
6. 用旧版本数据库启动（升级场景）：历史审计正常显示；能关联到现存服务器的记录自动补名称。
7. 删除一台有历史审计的服务器 → 新的「删除服务器」记录显示删除前名称；其更早的记录回退显示 id。
8. 批量执行（`exec_batch`）触发审批 → 审批卡与审计的服务器列显示「批量 ×N 台」。

## 已知边界

- 系统通知受 OS 通知权限、勿扰/专注模式影响；被系统静音时应用内提示音仍会响（前提是此前发生过一次用户交互）。
- Linux 依赖桌面通知服务（`org.freedesktop.Notifications`）；无桌面会话不会弹出。
- 升级前的历史审计没有名称快照，只能按 `server_id` 关联当前服务器名兜底；服务器已删除则回退显示 id。
- 30 秒到点提醒只在应用运行时生效；应用未运行时 MCP 通道同样不可用，不存在「AI 在后台等审批」的情况。
