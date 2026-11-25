# ServerDesk 发布（macOS arm64 + Windows amd64）

macOS 安装包可本机构建；Windows 安装包必须在 Windows 环境（本机或 CI）构建。
仓库提供 GitHub Actions 工作流：推送到 GitHub 后打 tag（如 `v0.1.0`）即可自动产出双平台安装包。

## 本机构建（macOS Apple Silicon）

```bash
# 1) 构建 MCP sidecar（会随应用打包）
cargo build --release -p serverdesk-mcp

# 2) 构建应用 + dmg（首次 release 全量编译约 5-10 分钟）
cd apps/desktop
pnpm tauri build
# 产物（workspace 模式下在仓库根 target/）：
#   target/release/bundle/macos/ServerDesk.app
#   target/release/bundle/dmg/ServerDesk_0.1.0_aarch64.dmg
```

> 未签名构建在他人机器上首次打开需「右键 → 打开」绕过 Gatekeeper；
> 正式分发请在 Xcode 中配置 Developer ID 证书并在 tauri.conf.json 配置 signingIdentity。

## Windows amd64 构建

方式 A（推荐）：GitHub Actions
```bash
git remote add origin <你的仓库>
git push -u origin main
git tag v0.1.0 && git push origin v0.1.0
# Actions「release」工作流自动构建并上传：
#   ServerDesk_0.1.0_x64-setup.exe (NSIS) / .msi
#   ServerDesk_0.1.0_aarch64.dmg / .app.tar.gz
```

方式 B：任意 Windows 10/11 x64 机器
```powershell
git clone <仓库> && cd server-mangement
cargo build --release -p serverdesk-mcp
cd apps/desktop
pnpm install
pnpm tauri build
# 产物在仓库根 target\release\bundle\{nsis,msi}
```

## MCP sidecar 分发说明

- sidecar 已通过 `bundle.resources` 打进应用（macOS: `ServerDesk.app/Contents/Resources/serverdesk-mcp`；Windows: 安装目录下）
- 用户在「设置 → MCP 客户端配置」复制配置 JSON 时，路径会自动指向打包后的 sidecar

## 版本升级

统一改三处：根 `Cargo.toml` workspace version、`apps/desktop/src-tauri/tauri.conf.json` version、`apps/desktop/package.json` version。

### 从使用过实验性敏感引用的版本升级

首次打开旧数据库时，ServerDesk 会关闭可能留有远端凭据副本的资产的“允许 AI 操作”开关。请在原主机核对并清理后，再按需重新开启。具体范围及步骤见[敏感引用退役与迁移说明](r9-secret-ref-design.md)。
