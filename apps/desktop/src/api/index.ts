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
  /** 读取输出超时：远端可能仍在执行，stdout 只是部分输出 */
  timedOut?: boolean;
}

export interface PendingApproval {
  id: string;
  serverId: string;
  serverName: string;
  command: string;
  /** AI 审查意见（R8.4）：审查未介入时为 null，不占位 */
  aiNote: string | null;
  createdAt: string;
  expiresAt: string;
}

/** 后端写入的审计动作（字面量）；未列出的值仍可透传（自定义/新增动作） */
export type AuditAction =
  | "mcp_exec_command"
  | "mcp_exec_batch"
  | "mcp_read_file"
  | "mcp_write_file"
  | "mcp_append_file"
  | "mcp_upload_file"
  | "mcp_download_file"
  | "mcp_restore_file"
  | "mcp_list_file_backups"
  | "mcp_list_files"
  | "mcp_tail_log"
  | "mcp_log_search"
  | "mcp_list_services"
  | "mcp_port_check"
  | "mcp_server_status"
  | "mcp_approval_requested"
  | "mcp_approval_granted"
  | "ssh_exec"
  | "sftp_read"
  | "sftp_write"
  | "sftp_upload"
  | "sftp_download"
  | "save_server"
  | "delete_server"
  | "backup_export"
  | "backup_restore"
  | "file_backup_restore"
  | "guard_set_rules"
  | "mcp_set_enabled"
  | "vault_unlock"
  | "rate_limited"
  | "ssh_test"
  | "ai_review"
  | "ai_review_save"
  | "ai_review_test";

export interface AuditPage {
  entries: AuditEntry[];
  total: number;
  page: number;
  pageSize: number;
}

export interface AuditEntry {
  id: number;
  ts: string;
  source: string;
  serverId: string | null;
  /** 写入时的服务器名称快照；老记录可能为空，需回退显示 serverId */
  serverName: string;
  action: AuditAction | (string & {});
  detail: string;
  outcome: string;
  session: string;
  /** 由后端 SQL 判定的成功标志（不再由前端猜 outcome 前缀） */
  ok: boolean;
}

export interface FileBackupMeta {
  id: string;
  serverId: string;
  path: string;
  sizeBytes: number;
  ts: string;
  /** 内容是否用保险库密钥加密（旧备份为 false，读取时按明文处理） */
  encrypted?: boolean;
}

export interface GuardInfo {
  deny: string[];
  approval: string[];
  aiEnabled: boolean;
  whitelistMode: boolean;
  whitelist: string[];
  readonly: boolean;
  ratePerMin: number;
  strictGroups: string[];
}

export interface GuardRules {
  deny: string[];
  approval: string[];
  whitelistMode: boolean;
  whitelist: string[];
  readonly: boolean;
  ratePerMin: number;
  strictGroups: string[];
}

export interface McpInfo {
  port: number;
  token: string;
  sidecarPath: string;
  configJson: string;
}

// ---------- AI 智能审查（R8） ----------

export type ReviewProtocol = "openaicompat";
export type ReviewFailurePolicy = "escalate" | "allow" | "block";
export type ReviewDenyMode = "escalate" | "reject";

export interface ReviewConfig {
  enabled: boolean;
  baseUrl: string;
  model: string;
  protocol: ReviewProtocol;
  /** 单次审查请求总超时（毫秒） */
  timeoutMs: number;
  /** 送审文本（脱敏后）长度上限（字节） */
  maxInputBytes: number;
  /** 输出 token 预算（推理模型建议 ≥512） */
  maxTokens: number;
  failurePolicy: ReviewFailurePolicy;
  denyMode: ReviewDenyMode;
  reviewFiles: boolean;
  /** 是否把服务器分组/名称一并发给模型（D3：默认 false） */
  sendServerContext: boolean;
  cacheTtlSecs: number;
  promptVersion: string;
}

export interface ReviewInfo extends ReviewConfig {
  hasKey: boolean;
  /** sk-****abcd 形态掩码；明文 Key 永不出后端 */
  keyMask: string;
}

export interface ReviewTestResult {
  ok: boolean;
  latencyMs: number;
  model: string;
  /** 模型原始输出（截断，仅诊断用） */
  content: string;
  error: string;
}

export interface ReviewEntry {
  id: number;
  ts: string;
  tool: string;
  serverName: string;
  /** allow | review | deny | error | skipped */
  aiVerdict: string;
  /** allow | approval | blocked */
  finalVerdict: string;
  risk: string;
  reason: string;
  /** 风险类别（destructive/credential/network/privilege/service/data/unknown） */
  categories: string[];
  model: string;
  latencyMs: number;
  cached: boolean;
  error: string;
}

export interface BackupSettings {
  enabled: boolean;
  keep: number;
  intervalHours: number;
  lastRun: string | null;
}

export interface BackupFileMeta {
  name: string;
  sizeBytes: number;
  createdAt: string;
}

export interface BackupInfo {
  settings: BackupSettings;
  files: BackupFileMeta[];
}

export interface ExportResult {
  count: number;
  path: string;
}

export interface RestoreResult {
  imported: number;
  mode: string;
}

export interface ListServersResult {
  servers: ServerConfig[];
  errors: string[];
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
  listServers: () => invoke<ListServersResult>("list_servers"),
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
  // AI 安全管线 / 审批 / 审计 / MCP
  guardInfo: () => invoke<GuardInfo>("guard_info"),
  guardSetRules: (rules: GuardRules) =>
    invoke<GuardInfo>("guard_set_rules", { rules }),
  mcpSetEnabled: (enabled: boolean) => invoke<GuardInfo>("mcp_set_enabled", { enabled }),
  approvalsPending: () => invoke<PendingApproval[]>("approvals_pending"),
  approvalDecide: (id: string, approve: boolean) =>
    invoke<void>("approval_decide", { id, approve }),
  auditList: (limit?: number, source?: string, session?: string, action?: string) =>
    invoke<AuditEntry[]>("audit_list", { limit, source, session, action }),
  auditPage: (page: number, pageSize: number, source?: string, session?: string, action?: string) =>
    invoke<AuditPage>("audit_page", { page, pageSize, source, session, action }),
  auditSessions: () => invoke<string[]>("audit_sessions"),
  auditFailures: () => invoke<number>("audit_failures"),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),
  // AI 智能审查（R8）
  aiReviewInfo: () => invoke<ReviewInfo>("ai_review_info"),
  aiReviewSave: (config: ReviewConfig, apiKey?: string) =>
    invoke<ReviewInfo>("ai_review_save", { config, apiKey: apiKey ?? null }),
  aiReviewTest: () => invoke<ReviewTestResult>("ai_review_test"),
  aiReviewRecent: (limit?: number) => invoke<ReviewEntry[]>("ai_review_recent", { limit }),
  // 备份与恢复
  backupInfo: () => invoke<BackupInfo>("backup_info"),
  backupExport: (path: string, password: string) =>
    invoke<ExportResult>("backup_export", { path, password }),
  backupRestore: (path: string, password: string, mode: string) =>
    invoke<RestoreResult>("backup_restore", { path, password, mode }),
  autoBackupNow: () => invoke<string>("auto_backup_now"),
  autoBackupSaveSettings: (enabled: boolean, keep: number, intervalHours: number) =>
    invoke<BackupInfo>("auto_backup_save_settings", { enabled, keep, intervalHours }),
  autoBackupRestore: (name: string, mode: string) =>
    invoke<RestoreResult>("auto_backup_restore", { name, mode }),
  autoBackupDelete: (name: string) => invoke<void>("auto_backup_delete", { name }),
  // 私钥文件导入
  readKeyFile: (path: string) => invoke<string>("read_key_file", { path }),
  // 变更记录
  fileBackupsList: (server?: string) =>
    invoke<FileBackupMeta[]>("file_backups_list", { server }),
  fileBackupRestore: (serverId: string, backupId: string) =>
    invoke<number>("file_backup_restore", { serverId, backupId }),
  // SFTP 文件传输（返回字节数）
  sftpUpload: (serverId: string, localPath: string, remotePath: string) =>
    invoke<number>("sftp_upload", { serverId, localPath, remotePath }),
  sftpDownload: (serverId: string, remotePath: string, savePath: string) =>
    invoke<number>("sftp_download", { serverId, remotePath, savePath }),
};
