//! AI 安全执行管线：高危命令拦截（黑名单硬拒绝）、敏感操作人工审批、
//! 凭据泄露防护（固定正则）、输出与审计脱敏。仅作用于 AI 来源的操作；
//! 用户手动操作不受拦截，但审计 detail 一律脱敏。

use crate::store::Store;
use regex::Regex;
use serde::Serialize;
use std::sync::{Arc, RwLock};

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
        }
    }
}

/// 凭据泄露防护（固定规则，不开放编辑）：AI 命令包含即硬拒绝，输出命中即脱敏。
struct LeakGuard {
    cmd_blockers: Vec<Regex>,
    out_scrubbers: Vec<(Regex, &'static str)>,
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
        Self { cmd_blockers, out_scrubbers }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardInfo {
    pub deny: Vec<String>,
    pub approval: Vec<String>,
    pub ai_enabled: bool,
}

pub enum Decision {
    Allow,
    /// 硬拒绝（附命中规则说明）
    Blocked(String),
    /// 需人工审批（附原因）
    NeedsApproval(String),
}

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

    /// 对 AI 命令做安全判定：泄露防护 → 硬黑名单 → 审批名单 → 放行。
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
        for p in &g.rules.deny {
            if !p.trim().is_empty() && lower.contains(&p.trim().to_lowercase()) {
                return Decision::Blocked(format!("命中高危命令黑名单「{p}」"));
            }
        }
        for p in &g.rules.approval {
            if !p.trim().is_empty() && lower.contains(&p.trim().to_lowercase()) {
                return Decision::NeedsApproval(format!("命中需审批规则「{p}」"));
            }
        }
        Decision::Allow
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
