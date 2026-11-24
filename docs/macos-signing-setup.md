# macOS 签名与公证配置指南（修复「已损坏」）

> 背景：未签名的 app 被 macOS Gatekeeper 拦截，Apple Silicon 上浏览器下载后报「已损坏，无法打开」。
> 根治方案：**Developer ID 签名 + 公证（notarization）**。配置完成后，用户下载 dmg 双击直接打开，无任何提示。
> CI 已接线（release.yml 的 macos job 会读取下表 Secrets），你只需要把 Secrets 配上。

## 你需要准备的材料

| # | 材料 | 哪里拿 | 对应 Secret |
|---|---|---|---|
| 1 | **Developer ID Application 证书**（.p12，含私钥） | Mac 上「钥匙串访问」→ 找到 `Developer ID Application: 你的名字 (TEAMID)` 证书 → 右键导出为 .p12（勾选私钥，设一个导出口令） | `APPLE_CERTIFICATE`（.p12 的 base64）与 `APPLE_CERTIFICATE_PASSWORD`（导出口令） |
| 2 | **Team ID**（10 位字母数字） | developer.apple.com → Account → Membership details | `APPLE_TEAM_ID` |
| 3 | **签名身份名称** | 钥匙串里证书的全名，形如 `Developer ID Application: 你的名字 (TEAMID)` | `APPLE_SIGNING_IDENTITY` |
| 4a | **Apple ID + 专用密码**（公证方式一） | appleid.apple.com → 登录与安全 → App 专用密码（生成一个，格式 xxxx-xxxx-xxxx-xxxx） | `APPLE_ID`（你的 Apple ID 邮箱）与 `APPLE_PASSWORD`（专用密码） |
| 4b | **App Store Connect API Key**（公证方式二，推荐） | appstoreconnect.apple.com → 用户和访问 → 集成 → App Store Connect API → 生成密钥（角色选 Developer，下载 .p8 只有一次机会） | `APPLE_API_ISSUER`（issuer id）、`APPLE_API_KEY`（key id）、`APPLE_API_KEY_PATH`（.p8 文件路径，CI 里放到临时位置） |

公证方式一和二**二选一**：4a 简单（五分钟）；4b 不依赖账号密码、更适合 CI（但要在 CI 里把 .p8 内容写成文件——需要加一小步，配置时告诉我，我来加）。

**前提**：你需要已加入 Apple Developer Program（$99/年），并在证书页面创建过 **Developer ID Application** 证书（不是普通的 Apple Development 证书）。

## 把材料写进 GitHub Secrets

在仓库根目录执行（.p12 先转 base64；过程中不要把值贴进聊天/截图）：

```bash
# 1) 证书（导出的 .p12 转成 base64 单行）
base64 -i /path/to/证书.p12 | gh secret set APPLE_CERTIFICATE --repo JingFang-i/server-desk
# 导出 .p12 时设的口令（没设口令就回车留空）
gh secret set APPLE_CERTIFICATE_PASSWORD --repo JingFang-i/server-desk

# 2) 签名身份与 Team ID（按你的实际值替换）
gh secret set APPLE_SIGNING_IDENTITY --repo JingFang-i/server-desk --body "Developer ID Application: 你的名字 (TEAMID)"
gh secret set APPLE_TEAM_ID --repo JingFang-i/server-desk --body "你的TEAMID"

# 3) 公证凭据（方式一）
gh secret set APPLE_ID --repo JingFang-i/server-desk --body "你的AppleID@example.com"
gh secret set APPLE_PASSWORD --repo JingFang-i/server-desk --body "xxxx-xxxx-xxxx-xxxx"
```

（appstoreconnect API key 的 Secret 配置等 4b，需要时找我加 CI 步骤。）

## 配好后会发生什么

- 打 tag 发版 → macOS job 自动：导入证书 → `codesign` 用 Developer ID 签 .app → `notarytool` 公证 → staple → 打 dmg/更新包 → 产物自动挂 Release。
- 用户下载 dmg → 双击直接打开，**不再有「已损坏」或任何安全提示**。
- Secrets 未配置时 macOS job 不受影响（走 adhoc），其他平台照常——可以随时补配，不用改任何代码。

## 验证（配好 Secrets 后发一版检查）

```bash
gh run watch   # 或看 Actions 页
# 下载新 dmg 后本机检查（应显示 Authority=Developer ID Application 且 Notarization 通过）
codesign -dv --verbose=4 /path/to/ServerDesk.app | grep -E "Authority|TeamIdentifier"
spctl -a -t exec -vv /path/to/ServerDesk.app   # 期望: accepted, source=Notarized Developer ID
```

## 本地构建不受影响

日常 `pnpm tauri build` 仍走 adhoc（本机打开没问题）；只有 CI 发版才做 Developer ID 签名 + 公证。
