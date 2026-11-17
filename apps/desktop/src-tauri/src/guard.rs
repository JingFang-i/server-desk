//! AI 安全执行管线：高危命令拦截（黑名单硬拒绝）、敏感操作人工审批、
//! 凭据泄露防护（固定正则）、输出与审计脱敏。仅作用于 AI 来源的操作；
//! 用户手动操作不受拦截，但审计 detail 一律脱敏。

use crate::store::Store;
use regex::Regex;
use serde::Serialize;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const META_GUARD_CONFIG: &str = "guard_config";
const META_MCP_ENABLED: &str = "mcp_enabled";
pub const APPROVAL_TIMEOUT_SECS: u64 = 120;

/// 用户可维护的拦截/审批规则（子串匹配，大小写不敏感）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GuardRules {
    /// 硬拒绝（AI 不可执行，也不可申请审批）
    pub deny: Vec<String>,
    /// 需人工审批（AI 提交 → 应用弹窗 → 用户批准后执行；超时自动拒绝）
    pub approval: Vec<String>,
    /// 白名单模式：true 时 AI 命令必须命中 whitelist 才放行
    pub whitelist_mode: bool,
    /// 白名单条目（子串匹配）
    pub whitelist: Vec<String>,
    /// 只读模式：拦截 AI 的一切写入类命令
    pub readonly: bool,
    /// AI 调用速率限制（次/分钟，0 = 不限）
    pub rate_per_min: u32,
    /// 严格分组：这些分组的服务器上，AI 的一切 exec 都需审批
    pub strict_groups: Vec<String>,
}

impl Default for GuardRules {
    fn default() -> Self {
        Self {
            deny: [
                "rm -rf /",
                "rm -fr /",
                "mkfs",
                "dd if=",
                "shutdown",
                "reboot",
                "init 0",
                "init 6",
                "halt",
                "poweroff",
                "userdel",
                "> /dev/sd",
                "chmod -r 777 /",
                ":(){",
                "history -c",
                "crontab -r",
                "ufw disable",
                "iptables -f",
                "iptables --flush",
                "systemctl stop ssh",
                "systemctl disable ssh",
                "/etc/ssh/sshd_config",
                "visudo",
                "/etc/shadow",
                "swapoff",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            approval: [
                "rm -rf",
                "rm -fr",
                "dd ",
                "mkswap",
                "systemctl restart",
                "systemctl stop",
                "systemctl disable",
                "kill -9",
                "killall",
                "apt remove",
                "apt purge",
                "apt autoremove",
                "yum remove",
                "dnf remove",
                "useradd",
                "usermod",
                "mount",
                "umount",
                "chmod -r",
                "chown -r",
                "curl",
                "wget",
                "pip install",
                "npm install -g",
                "tar -x",
                "mv /etc",
                "tee /etc",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        whitelist_mode: false,
        whitelist: Vec::new(),
        readonly: false,
        rate_per_min: 30,
        strict_groups: Vec::new(),
        }
    }
}

/// 只读模式下被拦截的写入类命令特征（合并正则，(?i) 由构造处添加）。
const WRITEISH_RE: &str = r"(>|>>|\btee\b|\bsed\b[^|;]*-i|\brm\b|\bmv\b|\bchmod\b|\bchown\b|\bdd\b|\bmkfs\b|\btruncate\b|\bshutdown\b|\breboot\b|\bkill\b|\bkillall\b|\buseradd\b|\buserdel\b|\bcrontab\b|\bapt\b|\byum\b|\bdnf\b|\bpip\b|\bnpm\b|\btouch\b|\bmkdir\b|\brsync\b|\bscp\b|\bsystemctl\s+(start|restart|stop|try-restart|reload-or-restart|disable|enable)\b)";

/// 凭据泄露防护（固定规则，不开放编辑）：AI 命令包含即硬拒绝，输出命中即脱敏。
struct LeakGuard {
    cmd_blockers: Vec<Regex>,
    out_scrubbers: Vec<(Regex, &'static str)>,
    writeish: Regex,
}

impl LeakGuard {
    fn build() -> Self {
        let cmd_blockers = [
            r"\benv\b",
            r"\bprintenv\b",
            r"(^|[|;&])\s*set\s*(\s|$)",
            r"\bid_rsa\b",
            r"\bid_ed25519\b",
            r"\bid_ecdsa\b",
            r"\bid_dsa\b",
            r"\.pem\b",
            r"/etc/shadow",
            r"authorized_keys",
            r"\bhistory\b",
        ]
        .iter()
        .map(|p| Regex::new(&format!("(?i){p}")).expect("leak regex"))
        .collect();
        let out_scrubbers = vec![
            (
                Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
                    .expect("pem regex"),
                "[REDACTED-PRIVATE-KEY]",
            ),
            (
                Regex::new(r"(?i)\b(pass(?:word)?|pwd|secret|api[_-]?key|access[_-]?key|token|credential)s?\b\s*[=:]\s*\S+")
                    .expect("cred regex"),
                "$1=[REDACTED]",
            ),
            (Regex::new(r"AKIA[0-9A-Z]{16}").expect("aws regex"), "[REDACTED-AWS-KEY]"),
            (
                Regex::new(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._\-]{16,}").expect("auth regex"),
                "$1 [REDACTED]",
            ),
        ];
        let writeish = Regex::new(&format!("(?i){WRITEISH_RE}")).expect("writeish regex");
        Self { cmd_blockers, out_scrubbers, writeish }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardInfo {
    pub deny: Vec<String>,
    pub approval: Vec<String>,
    pub ai_enabled: bool,
    pub whitelist_mode: bool,
    pub whitelist: Vec<String>,
    pub readonly: bool,
    pub rate_per_min: u32,
    pub strict_groups: Vec<String>,
}

/// AI 调用速率限制：60 秒滑动窗口。
pub struct AiRate {
    hits: Mutex<Vec<Instant>>,
}

impl AiRate {
    pub fn new() -> Self {
        Self { hits: Mutex::new(Vec::new()) }
    }

    /// 返回 Ok(()) 表示放行；Err 为被限流（附提示）。
    pub fn check(&self, limit_per_min: u32) -> Result<(), String> {
        if limit_per_min == 0 {
            return Ok(()); // 0 = 不限
        }
        let mut v = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        v.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if v.len() >= limit_per_min as usize {
            return Err(format!(
                "AI 调用触发速率限制（{limit_per_min} 次/分钟），请稍后再试"
            ));
        }
        v.push(now);
        Ok(())
    }
}

impl Default for AiRate {
    fn default() -> Self {
        Self::new()
    }
}

pub enum Decision {
    Allow,
    /// 硬拒绝（附命中规则说明）
    Blocked(String),
    /// 需人工审批（附原因）
    NeedsApproval(String),
}

/// AI 读取远程文件的路径黑名单（防绕过命令拦截、借道泄露服务器凭据）。
const READ_PATH_DENY: &[&str] = &[
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".pem",
    "/etc/shadow",
    "authorized_keys",
    ".env",
    "credential",
    "secret",
    ".gnupg",
    ".kube/config",
    ".aws/",
    ".netrc",
    ".git-credentials",
    ".docker/config.json",
];

/// AI 写入远程文件的路径黑名单（写入会破坏系统安全基线的位置）。
const WRITE_PATH_DENY: &[&str] = &[
    "/etc/shadow",
    "authorized_keys",
    "sshd_config",
    "crontab",
    "/etc/systemd",
    "/etc/init.d",
    "/etc/profile",
    "/etc/profile.d",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    "/etc/environment",
    ".ssh/",
    "/bin/",
    "/sbin/",
    "/usr/bin",
    "/usr/sbin",
    "/usr/local/bin",
    ".pem",
];

struct GuardInner {
    rules: GuardRules,
    leak: LeakGuard,
    ai_enabled: bool,
}

/// 可克隆的安全管线句柄。
#[derive(Clone)]
pub struct Guard {
    inner: Arc<RwLock<GuardInner>>,
}

impl Guard {
    /// 启动时从 meta 加载（无配置则用默认规则）。
    pub fn load(store: &Store) -> Self {
        let rules = store
            .get_meta(META_GUARD_CONFIG)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<GuardRules>(&s).ok())
            .unwrap_or_default();
        let ai_enabled = store
            .get_meta(META_MCP_ENABLED)
            .ok()
            .flatten()
            .map(|s| s == "1")
            .unwrap_or(false);
        Self {
            inner: Arc::new(RwLock::new(GuardInner {
                rules,
                leak: LeakGuard::build(),
                ai_enabled,
            })),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, GuardInner> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn info(&self) -> GuardInfo {
        let g = self.read();
        GuardInfo {
            deny: g.rules.deny.clone(),
            approval: g.rules.approval.clone(),
            ai_enabled: g.ai_enabled,
            whitelist_mode: g.rules.whitelist_mode,
            whitelist: g.rules.whitelist.clone(),
            readonly: g.rules.readonly,
            rate_per_min: g.rules.rate_per_min,
            strict_groups: g.rules.strict_groups.clone(),
        }
    }

    pub fn is_readonly(&self) -> bool {
        self.read().rules.readonly
    }

    pub fn rate_per_min(&self) -> u32 {
        self.read().rules.rate_per_min
    }

    /// 服务器分组是否命中严格分组（需全部审批）。
    pub fn is_strict_group(&self, group: Option<&str>) -> bool {
        let g = self.read();
        match group.map(|s| s.trim()).filter(|s| !s.is_empty()) {
            Some(gname) => g
                .rules
                .strict_groups
                .iter()
                .any(|s| s.trim().eq_ignore_ascii_case(gname)),
            None => false,
        }
    }

    pub fn ai_enabled(&self) -> bool {
        self.read().ai_enabled
    }

    /// 保存用户规则（覆盖式，含默认值回填：UI 编辑时先展示完整列表）。
    pub fn set_rules(&self, store: &Store, rules: GuardRules) -> Result<(), String> {
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            g.rules = rules.clone();
        }
        store.set_meta(
            META_GUARD_CONFIG,
            &serde_json::to_string(&rules).map_err(|e| e.to_string())?,
        )
    }

    pub fn set_ai_enabled(&self, store: &Store, enabled: bool) -> Result<(), String> {
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            g.ai_enabled = enabled;
        }
        store.set_meta(META_MCP_ENABLED, if enabled { "1" } else { "0" })
    }

    /// 对 AI 命令做安全判定：泄露防护 → 只读模式 → 硬黑名单 → 白名单模式 → 审批名单 → 放行。
    pub fn evaluate(&self, command: &str) -> Decision {
        let g = self.read();
        let lower = command.to_lowercase();
        for re in &g.leak.cmd_blockers {
            if re.is_match(command) {
                return Decision::Blocked(format!(
                    "命中凭据泄露防护规则（{}）——AI 不得读取或回显敏感信息",
                    re.as_str()
                ));
            }
        }
        // 只读模式：写入类命令直接拒（不提供审批逃生口）
        if g.rules.readonly && g.leak.writeish.is_match(command) {
            return Decision::Blocked(
                "只读模式已开启：AI 不得执行写入类命令".to_string(),
            );
        }
        for p in &g.rules.deny {
            if !p.trim().is_empty() && lower.contains(&p.trim().to_lowercase()) {
                return Decision::Blocked(format!("命中高危命令黑名单「{p}」"));
            }
        }
        // 白名单模式：未命中任何白名单条目 → 硬拒绝（无审批逃生口）
        if g.rules.whitelist_mode {
            let listed = g
                .rules
                .whitelist
                .iter()
                .filter(|w| !w.trim().is_empty())
                .any(|w| lower.contains(&w.trim().to_lowercase()));
            if !listed {
                return Decision::Blocked(
                    "白名单模式已开启：命令不在允许清单内".to_string(),
                );
            }
        }
        for p in &g.rules.approval {
            if !p.trim().is_empty() && lower.contains(&p.trim().to_lowercase()) {
                return Decision::NeedsApproval(format!("命中需审批规则「{p}」"));
            }
        }
        Decision::Allow
    }

    /// AI 读取远程路径的判定：命中泄露黑名单即拒绝，否则放行。
    pub fn evaluate_read_path(&self, path: &str) -> Decision {
        let lower = path.to_lowercase();
        for p in READ_PATH_DENY {
            if lower.contains(p) {
                return Decision::Blocked(format!(
                    "路径命中凭据/敏感文件保护规则「{p}」——AI 不得读取该文件"
                ));
            }
        }
        Decision::Allow
    }

    /// AI 写入远程路径的判定：命中禁写黑名单即拒绝；其余一律需人工审批。
    pub fn evaluate_write_path(&self, path: &str) -> Decision {
        let lower = path.to_lowercase();
        for p in WRITE_PATH_DENY {
            if lower.contains(p) {
                return Decision::Blocked(format!(
                    "路径命中系统安全禁写规则「{p}」——AI 不得写入该文件"
                ));
            }
        }
        Decision::NeedsApproval("AI 文件写入需人工审批".to_string())
    }

    /// 输出/审计文本脱敏（对 AI 与人工一视同仁）。
    pub fn sanitize(&self, text: &str) -> String {
        let g = self.read();
        let mut s = text.to_string();
        for (re, repl) in &g.leak.out_scrubbers {
            s = re.replace_all(&s, *repl).to_string();
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approvals::Approvals;

    fn guard() -> Guard {
        Guard {
            inner: Arc::new(RwLock::new(GuardInner {
                rules: GuardRules::default(),
                leak: LeakGuard::build(),
                ai_enabled: true,
            })),
        }
    }

    #[test]
    fn blocks_dangerous_root_removal() {
        assert!(matches!(guard().evaluate("rm -rf /"), Decision::Blocked(_)));
        assert!(matches!(guard().evaluate("shutdown -h now"), Decision::Blocked(_)));
    }

    #[test]
    fn needs_approval_for_service_restart() {
        assert!(matches!(
            guard().evaluate("systemctl restart nginx"),
            Decision::NeedsApproval(_)
        ));
        assert!(matches!(
            guard().evaluate("apt remove nginx"),
            Decision::NeedsApproval(_)
        ));
    }

    #[test]
    fn blocks_dd_disk_write() {
        assert!(matches!(
            guard().evaluate("dd if=/dev/zero of=/dev/sda"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn read_path_protection() {
        assert!(matches!(
            guard().evaluate_read_path("/root/.ssh/id_ed25519"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/etc/shadow"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/home/u/app/.env"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/var/log/app.log"),
            Decision::Allow
        ));
    }

    #[test]
    fn write_path_protection() {
        assert!(matches!(
            guard().evaluate_write_path("/etc/ssh/sshd_config"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_write_path("/root/.bashrc"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_write_path("/etc/nginx/conf.d/app.conf"),
            Decision::NeedsApproval(_)
        ));
        assert!(matches!(
            guard().evaluate_write_path("/tmp/notes.txt"),
            Decision::NeedsApproval(_)
        ));
    }

    #[test]
    fn allows_readonly_commands() {
        assert!(matches!(guard().evaluate("uptime && df -h /"), Decision::Allow));
        assert!(matches!(guard().evaluate("tail -n 50 /var/log/syslog"), Decision::Allow));
    }

    #[test]
    fn blocks_credential_leak_commands() {
        assert!(matches!(guard().evaluate("env | grep TOKEN"), Decision::Blocked(_)));
        assert!(matches!(guard().evaluate("cat ~/.ssh/id_rsa"), Decision::Blocked(_)));
        assert!(matches!(guard().evaluate("printenv"), Decision::Blocked(_)));
    }

    #[test]
    fn sanitizes_sensitive_output() {
        let g = guard();
        let out = g.sanitize(
            "password=hunter2\n\
             -----BEGIN OPENSSH PRIVATE KEY-----\nabc\ndef\n-----END OPENSSH PRIVATE KEY-----\n\
             aws: AKIAIOSFODNN7EXAMPLE\n\
             Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9xxxx",
        );
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("BEGIN OPENSSH"), "{out}");
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
        assert!(!out.contains("eyJhbGciOiJIUzI1NiIs"), "{out}");
        assert!(out.contains("[REDACTED"), "{out}");
    }

    #[tokio::test]
    async fn approval_decide_flow() {
        let q = Approvals::default();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending = q.create("s1", "srv-01", "dd if=/dev/zero of=/tmp/x", tx);
        assert_eq!(q.pending().len(), 1);
        assert!(q.decide(&pending.id, true));
        assert_eq!(rx.await, Ok(true));
        assert!(q.pending().is_empty());
    }

    #[tokio::test]
    async fn approval_reject_flow() {
        let q = Approvals::default();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending = q.create("s1", "srv-01", "reboot", tx);
        assert!(q.decide(&pending.id, false));
        assert_eq!(rx.await, Ok(false));
    }

    #[tokio::test]
    async fn approval_unknown_id_fails() {
        let q = Approvals::default();
        assert!(!q.decide("nonexistent", true));
    }
}

#[cfg(test)]
mod r4_tests {
    use super::*;

    fn guard_with(rules: GuardRules) -> Guard {
        Guard {
            inner: Arc::new(RwLock::new(GuardInner {
                rules,
                leak: LeakGuard::build(),
                ai_enabled: true,
            })),
        }
    }

    #[test]
    fn whitelist_mode_gate() {
        let mut r = GuardRules::default();
        r.whitelist_mode = true;
        r.whitelist = vec!["uptime".into(), "df ".into(), "free".into()];
        let g = guard_with(r);
        assert!(matches!(g.evaluate("uptime && df -h /"), Decision::Allow));
        assert!(matches!(g.evaluate("free -m"), Decision::Allow));
        assert!(matches!(g.evaluate("systemctl status nginx"), Decision::Blocked(_)));
    }

    #[test]
    fn readonly_blocks_writes_allows_reads() {
        let mut r = GuardRules::default();
        r.readonly = true;
        let g = guard_with(r);
        assert!(matches!(g.evaluate("touch /tmp/x"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("rm -rf /tmp/x"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("systemctl restart nginx"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("uptime && df -h"), Decision::Allow));
        assert!(matches!(g.evaluate("tail -n 5 /var/log/syslog"), Decision::Allow));
    }

    #[test]
    fn rate_window_cap() {
        let rate = AiRate::new();
        assert!(rate.check(2).is_ok());
        assert!(rate.check(2).is_ok());
        assert!(rate.check(2).is_err(), "第三次应被限流");
        // 0 = 不限
        let unlimited = AiRate::new();
        for _ in 0..5 {
            assert!(unlimited.check(0).is_ok());
        }
    }

    #[test]
    fn strict_group_match_case_insensitive() {
        let mut r = GuardRules::default();
        r.strict_groups = vec!["prod".into()];
        let g = guard_with(r);
        assert!(g.is_strict_group(Some("prod")));
        assert!(g.is_strict_group(Some("Prod")));
        assert!(g.is_strict_group(Some(" PROD ")));
        assert!(!g.is_strict_group(Some("dev")));
        assert!(!g.is_strict_group(None));
    }
}
