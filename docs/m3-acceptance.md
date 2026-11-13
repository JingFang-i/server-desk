# M3 验收：AI 安全执行管线 + MCP 工具

> 前置：完成 M2 验收（已设置主密码）。本里程碑后，AI 客户端即可安全接入。

## 架构与安全模型

```
AI 客户端(Claude/Cursor) → serverdesk-mcp (stdio JSON-RPC)
                              │  127.0.0.1 + Bearer token（每次启动换新，0600）
                              ▼
                     主应用 IPC → ops 管线
                     ① MCP 总开关 → ② 保险库锁定检查 → ③ 服务器 allowAi
                     ④ 泄露防护（env/私钥/.pem/shadow… 固定正则，直接拒）
                     ⑤ 高危黑名单（可编辑，直接拒）
                     ⑥ 审批名单（可编辑 → 审批中心人工批准，120s 超时拒）
                     ⑦ 执行 → 输出脱敏（PEM/password/token/AKIA…）→ 审计
```

凭据零暴露：sidecar 不读数据库、不持凭据；`list_servers` 只返回元信息；
工具输出与审计内容统一脱敏。

## 自动化验证（已全部通过）

```bash
cargo test -p serverdesk     # 10 项：泄露防护/黑名单/审批名单/放行/脱敏/审批流
# MCP 协议冒烟：
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_servers","arguments":{}}}' \
  | ./target/debug/serverdesk-mcp
# → initialize 应答 / 3 个工具 / 应用未运行时报错（fail-closed）
```

## 验收步骤（需 GUI + 真实服务器）

| # | 步骤 | 预期 |
|---|---|---|
| 1 | 启动应用解锁，进「设置」 | 「AI 安全管线」卡片可见，MCP 配置 JSON 已生成 |
| 2 | 开启「允许 AI 通过 MCP 操作服务器」 | 开关生效；关闭时 sidecar 调用报「MCP 工具已被禁用」 |
| 3 | 复制 MCP 配置 JSON，加入 Claude Desktop / Cursor 的 mcpServers | 客户端识别 serverdesk，可见 3 个工具 |
| 4 | AI 调用 `list_servers` | 返回服务器元信息，**无任何密码/私钥字段** |
| 5 | AI 调用 `exec_command {server, command:"df -h /"}` | 正常返回 stdout + exit 0 |
| 6 | AI 调用 `exec_command {command:"env"}` | 直接拒绝：「命中凭据泄露防护规则」 |
| 7 | AI 调用 `exec_command {command:"rm -rf /tmp/x && rm -rf /"}` | 直接拒绝：「命中高危命令黑名单」 |
| 8 | AI 调用 `exec_command {command:"systemctl restart nginx"}` | 应用「审批中心」出现待审批卡片（剩余秒数倒计时） |
| 9 | 第 8 步点「批准执行」 | 命令继续执行并返回结果；审计中心记录 approval_granted |
| 10 | 重复第 8 步但等 120s 不处理 | 返回「审批被拒绝或已超时」；审计记录 approval_denied_or_timeout |
| 11 | 关闭某服务器「允许 AI」后 AI 调用该服务器 | 拒绝：「未开放 AI 操作」 |
| 12 | 锁定保险库后 AI 调用任意工具 | 拒绝：「保险库已锁定」 |
| 13 | 审计日志页筛选 source=ai | 第 4-12 步全部操作与拦截记录在案，内容无敏感信息 |
| 14 | 设置页修改黑名单（删掉 shutdown）保存后 AI 再试 `shutdown` | 变为需审批（规则编辑生效）；可再加回 |
| 15 | 全库检查 | `sqlite3 servers.db "SELECT hex(secret_blob) FROM servers;"` 仍为密文；`strings target/debug/serverdesk-mcp` 无凭据路径访问逻辑 |

## 已知边界

- `read_file` / `write_file` 工具暂缓（P1）：需路径白名单设计，避免 AI 任意写文件；如需要将在 M4 补充
- 断线重连上限 2 次（exec 内自动），更适合长连接场景的「网络恢复主动重连」留给 M4 评估
- sidecar 打包路径目前取 `current_exe` 同目录（dev 即 target/debug），M5 打包时随应用分发
