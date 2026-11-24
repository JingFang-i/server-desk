#!/usr/bin/env bash
# 把 APPLE_CERTIFICATE（legacy .p12 的 base64）导入临时钥匙串，
# 供 workflow 步骤里的 codesign（如 MCP sidecar）使用。
#
# 时序说明：tauri-action 的钥匙串初始化发生在 build 内部、晚于 beforeBuildCommand，
# 所以 beforeBuildCommand 里的 codesign 拿不到证书——必须在独立 workflow 步骤先建好。
# 用完后请把本钥匙串从 search list 移除，避免 bundler 自建钥匙串出现重复身份。
set -euo pipefail
: "${APPLE_CERTIFICATE:?需要 APPLE_CERTIFICATE（legacy p12 的 base64）}"
: "${APPLE_CERTIFICATE_PASSWORD:?需要 APPLE_CERTIFICATE_PASSWORD}"
: "${APPLE_SIGNING_IDENTITY:?需要 APPLE_SIGNING_IDENTITY}"

TMPD=$(mktemp -d)
P12="$TMPD/cert.p12"
echo "$APPLE_CERTIFICATE" | base64 --decode > "$P12"

KB="$TMPD/build.keychain-db"
KC_PWD=$(openssl rand -hex 12)
security create-keychain -p "$KC_PWD" "$KB"
security unlock-keychain -p "$KC_PWD" "$KB"
security import "$P12" -k "$KB" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$KC_PWD" "$KB"
security list-keychains -s "$KB" ~/Library/Keychains/login.keychain-db
echo "macOS 签名钥匙串已就绪: $KB"
