# R1–R5 扩展总验收

> 前置：M1–M5 与 SFTP 已交付。本轮为 AI 运维扩展五连发（通知类除外）。

## 里程碑总览

| 里程碑 | 内容 | 提交 | 测试 |
|---|---|---|---|
| R1 | 观测五件套（list_files / tail_log / log_search / list_services / port_check） | fce330a | 17/17 |
| R2 | AI 文件能力（upload/download/append + write_file 自动备份 + restore_file 回滚） | 2ea9fb4 | 18/18 |
| R3 | exec_batch 多服务器批量执行（分组/列表、一次审批、并发 5、≤20 台） | b645c5c | 19/19 |
| R4 | 安全治理（黑名单/白名单双模式、只读模式、AI 限速 30/分、严格分组全审批）+ 设置 UI | 3e86a4c | 23/23 |
| R5 | 审计 session 字段（自动迁移）+ AI 会话视图 + 变更记录回滚页 | b6016cc | 23/23 |

最终：**16 个 MCP 工具**、**23 项自动化测试**、前端+Rust 全绿、CI 双平台构建通过。

## AI 工具全景（16 个）

元信息：`list_servers` `list_file_backups`
执行：`exec_command` `exec_batch`
观测：`server_status` `list_files` `tail_log` `log_search` `list_services` `port_check`
文件：`read_file` `write_file`（自动备份） `append_file` `upload_file` `download_file` `restore_file`

## 全部工具共同的安全底座

1. allow_ai 闸门（全局开关 → 保险库解锁 → 每服务器开关）
2. 凭据泄露防护（命令/路径/输出三层，固定规则不可绕过）
3. 只读模式（硬禁一切写入，无审批逃生口）
4. 白名单模式（未命中白名单直接拒）
5. 黑名单（默认 25 条）+ 审批名单（28 条，120 秒人工批准窗口）
6. 严格分组（该分组一切命令需审批，批量整批强制）
7. 速率限制（60 秒滑动窗口，防 Agent 失控）
8. 输出脱敏 → 全量审计（AI 行带会话 id，7 天保留）

## 关键验收场景

| # | 场景 | 预期 |
|---|---|---|
| 1 | AI tail_log /var/log/syslog 200 行 | 返回尾部 200 行 |
| 2 | AI log_search 命中凭据文件路径 | 直接拒 |
| 3 | AI write_file 修改配置 | 审批 → 写入 → 变更记录页出现备份条目 |
| 4 | 误改后 | 审计详情复制 backup_id → restore_file（审批）→ 内容还原 |
| 5 | 变更记录页「回滚」按钮 | 手动回滚（审计 source=user） |
| 6 | AI exec_batch group=prod df -h | 一张审批卡 → 聚合结果逐台展示 |
| 7 | 白名单模式开启后 AI 跑清单外命令 | 直接拒（无审批） |
| 8 | 只读模式开启后 AI 任何写操作 | 直接拒（含 upload/write/append/restore） |
| 9 | AI 高频调用 31 次/分钟 | 第 31 次被限流（默认 30） |
| 10 | 审计页 source=ai | 出现会话列；同一 MCP 会话共享会话 id；会话筛选生效 |

## 已知边界

- 审计会话归因基于 sidecar 进程：同一 MCP 客户端多次重启会话会分段；并发多客户端极端情况下归因可能交错
- exec_batch 上限 20 台；单台输出 4KB 截断（全量结果需逐台 exec_command）
- 自动备份仅覆盖 ≤1MB 的 write_file；超大文件写入不产生备份
