# M1 验收：workspace 骨架 + 服务器资产 CRUD

## 运行说明

```bash
cd apps/desktop
pnpm install        # 首次
pnpm tauri dev      # 启动开发模式（首次 Rust 编译需数分钟）
```

生产构建（当前机器 macOS arm64）：

```bash
cd apps/desktop
pnpm tauri build    # 产出 .app / .dmg
```

MCP sidecar 二进制（M1 仅骨架）：

```bash
cargo build -p serverdesk-mcp
./target/debug/serverdesk-mcp   # 输出版本提示（stderr）
```

数据目录：`~/Library/Application Support/com.qsmx.serverdesk/`
（macOS；Windows 为 `%APPDATA%/com.qsmx.serverdesk/`）

## 验收步骤

| # | 步骤 | 预期 |
|---|---|---|
| 1 | `pnpm tauri dev` 启动 | 窗口打开，深色主题，左侧导航含 M2/M3/M4 占位项 |
| 2 | 「新增服务器」录入账密方式（名称/主机/端口/用户名/密码），保存 | 列表出现新行，认证方式显示「密码」 |
| 3 | 「新增服务器」切换为 SSH 私钥方式，粘贴私钥与口令，保存 | 列表新行显示「私钥」 |
| 4 | 编辑任意条目修改分组/标签 | 列表即时更新 |
| 5 | 搜索关键字、按分组过滤 | 结果正确过滤 |
| 6 | 行内「测试连接」（指向一台可达主机，如 1.1.1.1:22） | 提示 TCP 连接成功与延迟 |
| 7 | 「测试连接」指向不可达地址 | 提示连接失败/超时 |
| 8 | 删除条目（Popconfirm 确认） | 行消失 |
| 9 | 完全退出应用后重新打开 | 数据仍在（SQLite 持久化） |
| 10 | 检查数据库凭据无明文 | `sqlite3 "$HOME/Library/Application Support/com.qsmx.serverdesk/servers.db" "SELECT hex(secret_blob) FROM servers;"` 为密文（非 UTF-8 可读文本） |

## 已知边界（按里程碑补齐）

- 连接测试为 TCP 层探测，SSH 完整认证测试在 M2
- 凭据加密为 M1 过渡方案（本地随机密钥 + AES-256-GCM，0600 权限），M2 升级为主密码保险库 + OS Keychain
- MCP sidecar 为编译骨架，工具实现在 M3
