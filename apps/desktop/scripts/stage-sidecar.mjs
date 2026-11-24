#!/usr/bin/env node
// 把 MCP sidecar 复制为 Tauri 一等公民 sidecar（bundle.externalBin）：
// src-tauri/binaries/serverdesk-mcp-<target-triple>[.exe]
// bundler 会把它装到主程序旁边并随整包自动签名/公证（macOS 含 hardened runtime
// 与安全时间戳——此前放在 Resources 资源目录里的裸二进制需要手工签名，
// 公证还会因缺 hardened runtime 而 Invalid）。
// 路径按脚本自身位置解析，与调用方 CWD 无关。
import { copyFileSync, existsSync, mkdirSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url)); // apps/desktop/scripts
const appDir = resolve(here, ".."); // apps/desktop
const workspace = resolve(appDir, "..", ".."); // 仓库根
const isWindows = process.platform === "win32";
const ext = isWindows ? ".exe" : "";

// 目标三元组：优先用 tauri CLI 导出的环境变量，否则取宿主机原生三元组。
// 注意：本文件是 ESM（.mjs），不能用 require()——之前在 Linux runner 上
// require 未定义被 catch 吞掉，回退成硬编码的 aarch64-apple-darwin，
// 导致 externalBin 文件名与目标 triple 不符、cargo test 的 build.rs 报缺失。
let triple = process.env.TAURI_ENV_TARGET_TRIPLE || "";
if (!triple) {
  try {
    triple = execSync("rustc -vV", { encoding: "utf8" }).match(/host:\s*(\S+)/)[1];
  } catch {
    const arm = process.arch === "arm64";
    if (isWindows) triple = arm ? "aarch64-pc-windows-msvc" : "x86_64-pc-windows-msvc";
    else if (process.platform === "darwin") triple = arm ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
    else triple = arm ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu";
  }
}

const src = join(workspace, "target", process.env.TAURI_ENV_TARGET_DIR_BASE || "release", `serverdesk-mcp${ext}`);
const destDir = join(appDir, "src-tauri", "binaries");
mkdirSync(destDir, { recursive: true });
const dest = join(destDir, `serverdesk-mcp-${triple}${ext}`);

// externalBin 要求打包时文件必须存在（tauri-build 的 build.rs 会校验），
// 所以即使 sidecar 还没编译过也要落一个占位文件让 cargo check 通过；
// 正式发版（CI）一定先构建了真实 sidecar，这里是给本地快速检查用的兜底。
if (!existsSync(src)) {
  console.warn(
    `警告：sidecar 尚未编译（${src}），写入占位文件让构建通过——` +
      "该安装包的 MCP 入口不可用，正式发版前请先 cargo build --release -p serverdesk-mcp"
  );
  writeFileSync(dest, "placeholder: sidecar not built\n");
} else {
  copyFileSync(src, dest);
}
console.log(`sidecar 已暂存：${dest}`);
