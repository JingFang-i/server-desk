# SFTP 文件传输 + AI 文件工具 验收

> 前置：完成 M3 验收（AI 接入已开启）。需要一台真实服务器。

## 界面入口

服务器行操作 → ⋮ 下拉 → 「文件传输」：

| 操作 | 步骤 | 预期 |
|---|---|---|
| 上传 | 填远端目录（默认 /root/）→ 选择本地文件 | 提示上传字节数；到服务器 ls -l 验证 |
| 上传覆盖 | 再传同名文件 | 覆盖成功（整文件覆盖语义） |
| 下载 | 输入远端路径 → 保存到本地 | 本地文件内容与远端一致（diff 对比） |
| 大文件 | 大于 512MB 上传 / 大于 1GB 下载 | 明确拒绝（上限提示） |
| 断线恢复 | 断网后直接传输 | 自动重连成功（或报错后重试一次成功） |
| 审计 | 审计日志页 | sftp_upload / sftp_download 记录含路径与字节数 |

## AI 工具（read_file / write_file）

MCP 客户端可见 5 个工具（新增 2 个）：

| 场景 | 调用 | 预期 |
|---|---|---|
| 正常读 | read_file {server, path:"/etc/hostname"} | 返回内容（已脱敏）+ size |
| 读敏感 | path 指向 id_ed25519 / /etc/shadow / .env | 直接拒绝：路径命中凭据/敏感文件保护规则 |
| 读大文件 | path 指向大日志文件 | 内容截断到 512KB，truncated=true |
| 写普通文件 | write_file {path:"/tmp/hello.txt", content:"hi"} | 审批中心弹卡片，批准后写入成功 |
| 写等 120s | 同上但不处理审批 | 审批被拒绝或已超时 |
| 写系统路径 | path 指向 sshd_config / .bashrc / authorized_keys | 直接拒绝（禁写名单），不进审批 |

## 安全设计要点

- 读防护：AI 读文件的路径黑名单独立于命令黑名单（防止借 SFTP 绕过 cat id_rsa 的命令拦截）；返回内容仍过脱敏管线
- 写防护：禁写名单（sshd_config / authorized_keys / shell 配置 / crontab / systemd / bin 目录等）直接拒绝；其余路径一律需人工审批
- 手动（界面）传输不经拦截/审批，但全部写审计
