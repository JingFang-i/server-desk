#!/usr/bin/env node
// 把 MCP sidecar 复制到 src-tauri/resources/，由 bundle.resources 打进安装包。
//
// 背景：release 流程一直只 `cargo build -p serverdesk-mcp`，但 tauri.conf.json 的
// bundle 段没有 resources，安装版里找不到 sidecar，AI 入口实际不可用
// （mcp_info 会回退成裸文件名）。此脚本由 beforeBuildCommand 调用，路径按自身位置解析，
// 与调用方 CWD 无关。
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url)); // apps/desktop/scripts
const appDir = resolve(here, ".."); // apps/desktop
const workspace = resolve(appDir, "..", ".."); // 仓库根
const isWindows = process.platform === "win32";
const binName = isWindows ? "serverdesk-mcp.exe" : "serverdesk-mcp";
const src = join(workspace, "target", "release", binName);

if (!existsSync(src)) {
  console.error(
    `找不到 sidecar 可执行文件：${src}\n` +
      "请先运行 cargo build --release -p serverdesk-mcp（本脚本只在打包时调用）"
  );
  process.exit(1);
}

const destDir = join(appDir, "src-tauri", "resources");
mkdirSync(destDir, { recursive: true });
const dest = join(destDir, binName);
copyFileSync(src, dest);
if (!isWindows) chmodSync(dest, 0o755);

// macOS 发版：sidecar 必须与主应用一样用 Developer ID 签名（含 hardened runtime
// 与安全时间戳），否则公证判定 Invalid、下载后报「已损坏」。
// tauri-action 会在执行 beforeBuildCommand 之前把 APPLE_CERTIFICATE 导入临时钥匙串，
// 因此这里可以直接引用 APPLE_SIGNING_IDENTITY。
if (process.platform === "darwin" && process.env.APPLE_SIGNING_IDENTITY) {
  const { execFileSync } = await import("node:child_process");
  execFileSync(
    "codesign",
    ["--force", "--options", "runtime", "--timestamp", "--sign", process.env.APPLE_SIGNING_IDENTITY, dest],
    { stdio: "inherit" }
  );
  console.log("sidecar 已用 Developer ID 签名");
}
console.log(`sidecar 已暂存：${dest}`);
