import { invoke } from "@tauri-apps/api/core";

// ---------- 类型 ----------

export interface AuthPassword {
  type: "password";
  password: string;
}
export interface AuthKey {
  type: "key";
  privateKey: string;
  passphrase: string | null;
}
export type AuthMethod = AuthPassword | AuthKey;

export interface ServerConfig {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  auth: AuthMethod;
  group: string | null;
  tags: string[];
  notes: string | null;
  allowAi: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface SaveServerInput {
  id: string | null;
  name: string;
  host: string;
  port: number;
  username: string;
  auth: AuthMethod;
  group: string | null;
  tags: string[];
  notes: string | null;
  allowAi: boolean;
}

export interface TestResult {
  ok: boolean;
  latencyMs: number | null;
  message: string;
}

export interface VaultStatus {
  initialized: boolean;
  unlocked: boolean;
  autoLockMinutes: number;
  keychainAutoUnlock: boolean;
}

export type ConnStatus = "connected" | "disconnected";

export interface ConnInfo {
  serverId: string;
  status: ConnStatus;
}

export interface ExecResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  truncated: boolean;
}

// ---------- API ----------

export const api = {
  // 保险库
  vaultStatus: () => invoke<VaultStatus>("vault_status"),
  vaultSetup: (password: string) => invoke<string | null>("vault_setup", { password }),
  vaultUnlock: (password: string) => invoke<string | null>("vault_unlock", { password }),
  vaultLock: () => invoke<void>("vault_lock"),
  vaultSetSettings: (autoLockMinutes: number, keychainAutoUnlock: boolean) =>
    invoke<VaultStatus>("vault_set_settings", { autoLockMinutes, keychainAutoUnlock }),
  vaultReset: () => invoke<void>("vault_reset"),
  // 服务器资产
  listServers: () => invoke<ServerConfig[]>("list_servers"),
  saveServer: (input: SaveServerInput) => invoke<ServerConfig>("save_server", { input }),
  deleteServer: (id: string) => invoke<void>("delete_server", { id }),
  // 连接测试
  testConnection: (host: string, port: number) =>
    invoke<TestResult>("test_connection", { host, port }),
  sshTest: (serverId: string) => invoke<TestResult>("ssh_test", { serverId }),
  // SSH 连接池
  sshConnect: (serverId: string) => invoke<void>("ssh_connect", { serverId }),
  sshDisconnect: (serverId: string) => invoke<void>("ssh_disconnect", { serverId }),
  sshStatus: () => invoke<ConnInfo[]>("ssh_status"),
  sshExec: (serverId: string, command: string, timeoutSecs?: number) =>
    invoke<ExecResult>("ssh_exec", { serverId, command, timeoutSecs }),
  sshResetHostkey: (serverId: string) => invoke<void>("ssh_reset_hostkey", { serverId }),
};
