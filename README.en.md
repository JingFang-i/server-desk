# ServerDesk — Local Server Management with AI-Safe Operations

[简体中文](README.md) | **English**

A local desktop application that runs on your personal computer: centrally manage server
assets (credentials / SSH keys), keep stable SSH connections, and expose **restricted**
ops capabilities to AI Agents via the standard **MCP protocol** — AI can handle day-to-day
operations, but it can never see credentials or run dangerous commands.

`macOS 14+ (Apple Silicon)` · `Windows 10/11 (amd64)` · `Debian 12 / Ubuntu 22.04+ (amd64)` · `Tauri 2 (Rust)` · `Vue 3 / TypeScript` · `SQLite` · `AES-256-GCM`

---

## ✨ Key Features

- 🗂️ **Server asset management** — hosts / ports / passwords / SSH keys in one place, one-click TCP reachability test
- 🔐 **Master-password vault** — Argon2-derived keys + OS Keychain + auto-lock; credentials encrypted at rest with AES-256-GCM, never stored in plaintext
- 🔌 **Stable SSH connections** — keep-alive, automatic reconnect, remote command execution
- 🤖 **AI safety pipeline** — leak protection / black & white lists / approval for dangerous commands / full audit / output redaction, seven layers of defense; optional **AI review** (LLM-based semantic verdict that can only tighten decisions, works fully offline with local Ollama)
- 🔔 **Approval alerts** — system notification + chime + sidebar badge when a dangerous command needs human approval; auto-rejected after 120s
- 📦 **Encrypted backups** — manual export & restore (merge / overwrite) + scheduled automatic backups (keep N copies)
- 🖥️ **Multi-platform** — native builds for macOS / Windows / Linux (Linux ships .deb / .rpm / AppImage)

## 🏗️ Architecture

```
┌─────────────────────────────┐        stdio (MCP)        ┌──────────────────┐
│  ServerDesk desktop app      │◄────────────────────────►│  AI Agent         │
│  Vue3 frontend + Rust core   │    16 restricted tools    │  (Claude/DSH/…)  │
│  ┌───────────────────────┐  │                           └──────────────────┘
│  │ Vault (crypto/keychain)│  │
│  │ Asset store (SQLite)   │  │
│  │ Safety pipeline guard  │──┼──► black/white list → AI review (optional) → approval → audit → redaction
│  │ SSH / SFTP channel     │  │
│  └───────────────────────┘  │
└─────────────┬───────────────┘
              │ SSH
   ┌──────────▼──────────┐   ┌──────────────┐
   │  Server A (root@host)│   │  Server B / C │
   └─────────────────────┘   └──────────────┘
```

## 🤖 AI Operations (16 MCP tools)

| Category | Tool | Description |
|----------|------|-------------|
| Assets | `list_servers` | List registered servers (never returns passwords or keys) |
| Observe | `server_status` | uptime / CPU / memory / disk / load |
| Observe | `list_files` / `read_file` | Directory listing, file reading (sensitive-path protection, auto redaction) |
| Observe | `tail_log` / `log_search` | Log tailing / log search |
| Observe | `list_services` / `port_check` | systemd service status, port listening check |
| Execute | `exec_command` | Run on one server (safety pipeline verdict; dangerous commands require human approval) |
| Execute | `exec_batch` | Batch execution across servers (one approval, concurrency 5, per-host audit) |
| Files | `upload_file` / `download_file` | Upload / download (size and path restricted) |
| Files | `append_file` / `write_file` | Append / overwrite write (auto backup before write, human approval required) |
| Rollback | `list_file_backups` / `restore_file` | List pre-write backups / one-click rollback |

Every tool goes through the seven-layer pipeline in `guard.rs`:
credential-leak protection → blacklist → whitelist → human approval →
execution audit → output redaction → rate limiting.

An optional **AI review** layer (the `review/` module) re-checks AI-issued commands
with an LLM you configure (allow / send to human approval / recommend reject).
Its verdict can only tighten the decision, never loosen it. Works fully offline
with a local Ollama endpoint; command text is sanitized before leaving the app,
and every verdict is logged. See [docs/r8-ai-review-design.md](docs/r8-ai-review-design.md).

## 🚀 Quick Start (development mode)

```bash
cd apps/desktop
pnpm install
pnpm tauri dev
```

Production build:

```bash
cd apps/desktop
pnpm tauri build     # installers output to apps/desktop/src-tauri/target/release/bundle/
```

Rust-side check (workspace root):

```bash
cargo check
```

## 📁 Project Layout

```
crates/shared/            # Cross-process shared types (asset model, MCP tool contract)
apps/desktop/             # Tauri main app
  ├─ src/                 # Vue3 frontend (api / stores / views)
  ├─ src-tauri/src/       # Rust core: store / crypto / vault / guard /
  │                       #   approvals / ssh / observe / ops / backup / commands
  └─ scripts/             # check_vue_imports.py
apps/mcp-sidecar/         # stdio MCP server (AI tool entry point)
docs/                     # Design & acceptance docs (plan.md, m1~m5, r1-r6, release.md)
.github/workflows/        # release.yml (release CI)
```

## 🗺️ Milestones

| Milestone | Scope | Status |
|-----------|-------|--------|
| **M1** | Asset CRUD + connection test | ✅ |
| **M2** | Master-password vault + SSH connect / keep-alive / reconnect / exec | ✅ |
| **M3** | AI safety pipeline + MCP tools (list_servers / exec_command / server_status) | ✅ |
| **M4** | Encrypted backup export / restore + scheduled automatic backups | ✅ |
| **M5** | Dual-platform packaging + acceptance | ✅ |
| **R1** | Observation set: list_files / tail_log / log_search / list_services / port_check | ✅ |
| **R2** | File ops: upload / download / append + write_file auto backup + restore_file | ✅ |
| **R3** | exec_batch multi-server batch execution | ✅ |
| **R4** | Governance: black/white-list modes, read-only mode, AI rate limiting, strict group approval | ✅ |
| **R5** | AI session view + change-log rollback page | ✅ |

## 🛡️ Security Baseline

- Credentials encrypted at rest with **AES-256-GCM**; no plaintext in the database (upgraded to master-password vault + OS Keychain since M2)
- Logs and tool output are **uniformly redacted**; credentials must never appear in AI results
- Dangerous commands go through **blacklist + human approval flow** (auto-reject after 120s timeout) with full audit logging
- Per-server "allow AI" switch; read-only mode and AI rate limiting supported
- Automatic backup before every AI file write, with one-click rollback (R2/R5)

## 📚 Documentation

- If you used the experimental Secret Indirection feature, check the original SSH host for credential files before re-enabling AI access. See the [migration notes (Chinese)](docs/r9-secret-ref-design.md).
- Full design: [docs/plan.md](docs/plan.md) (Chinese)
- Milestone acceptance: [docs/m1-acceptance.md](docs/m1-acceptance.md) ~ [docs/m4-acceptance.md](docs/m4-acceptance.md), [docs/acceptance-final.md](docs/acceptance-final.md)
- AI extension acceptance: [docs/r1-r5-acceptance.md](docs/r1-r5-acceptance.md)
- Approval alerts & audit server names: [docs/r6-acceptance.md](docs/r6-acceptance.md)
- Release notes: [docs/release.md](docs/release.md)
- AI agent guide: [AGENTS.md](AGENTS.md)

---

ServerDesk · Let AI manage your servers, safely.
