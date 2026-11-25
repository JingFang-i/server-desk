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
///
/// 判定前会先剥掉「写到 /dev/null」「合并 stderr」这类重定向（见 [`strip_null_redirections`]），
/// 否则只读模板里的 `2>/dev/null` 会被 `>` 误判成写入。
/// 每个命令词都要求位于命令起始或分隔符之后，避免 `ls ln` 这类参数被误伤。
const WRITEISH_RE: &str = concat!(
    // 重定向（已排除 /dev/null）
    r"(?:>>|>)",
    // 写文件 / 改权限 / 改系统
    r"|(?:^|[;&|(]\s*|\s)(?:cp|ln|install|mv|rm|rmdir|mkdir|touch|mkfifo|mknod|dd|truncate|shred|wipefs|chmod|chown|chattr|setfacl|rsync|scp|sftp|patch)\b",
    // 就地修改类
    r"|(?:^|[;&|(]\s*|\s)sed\b[^|;]*-i",
    r"|(?:^|[;&|(]\s*|\s)perl\b[^|;]*-i",
    r"|(?:^|[;&|(]\s*|\s)find\b[^|;]*-(?:delete|exec)\b",
    r"|(?:^|[;&|(]\s*|\s)(?:python[0-9.]*|node|ruby|php)\s+-[ce]\b",
    // 服务与包管理
    r"|(?:^|[;&|(]\s*|\s)systemctl\s+(?:start|restart|stop|try-restart|reload|reload-or-restart|enable|disable|mask|unmask)\b",
    r"|(?:^|[;&|(]\s*|\s)service\s+\S+\s+\S+",
    r"|(?:^|[;&|(]\s*|\s)apt(?:-get)?\s+(?:install|remove|purge|upgrade|dist-upgrade|autoremove)\b",
    r"|(?:^|[;&|(]\s*|\s)(?:yum|dnf|pacman|zypper)\s+(?:install|remove|erase|update|upgrade)\b",
    r"|(?:^|[;&|(]\s*|\s)pip[0-9.]*\s+install\b",
    r"|(?:^|[;&|(]\s*|\s)npm\s+(?:install|i|uninstall|remove|ci)\b",
    // 关机 / 杀进程 / 账号
    r"|(?:^|[;&|(]\s*|\s)(?:shutdown|reboot|halt|poweroff)\b",
    r"|(?:^|[;&|(]\s*|\s)init\s+[06]\b",
    r"|(?:^|[;&|(]\s*|\s)(?:killall|pkill)\b",
    r"|(?:^|[;&|(]\s*|\s)kill\s+-",
    r"|(?:^|[;&|(]\s*|\s)(?:useradd|userdel|usermod|groupadd|groupdel|passwd)\b",
    r"|(?:^|[;&|(]\s*|\s)crontab\b",
);

/// 剥掉不产生写入的重定向：`2>/dev/null`、`>/dev/null`、`2>&1`、`&>/dev/null`。
fn strip_null_redirections(cmd: &str) -> String {
    use std::sync::OnceLock;
    static NULL_REDIRECT: OnceLock<Regex> = OnceLock::new();
    static MERGE_STDERR: OnceLock<Regex> = OnceLock::new();
    let null = NULL_REDIRECT
        .get_or_init(|| Regex::new(r"(?:\d?>>?|&>)\s*/dev/null").expect("null redirect regex"));
    let merge = MERGE_STDERR
        .get_or_init(|| Regex::new(r"\d?>&\d+").expect("stderr merge regex"));
    let s = null.replace_all(cmd, " ");
    merge.replace_all(&s, " ").to_string()
}

/// 按 shell 分隔符切分子命令（`;`、`&&`、`||`、单/双管道、换行）。
fn split_segments(cmd: &str) -> Vec<&str> {
    use std::sync::OnceLock;
    static SEG: OnceLock<Regex> = OnceLock::new();
    let re = SEG.get_or_init(|| Regex::new(r"\s*(?:\|\||&&|[;|\n])\s*").expect("segment regex"));
    re.split(cmd).map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// 受限模式（只读 / 白名单）下无法安全判定的 shell 构造：命令替换与进程替换。
fn has_substitution(cmd: &str) -> bool {
    cmd.contains("$(") || cmd.contains("<(") || cmd.contains(">(") || cmd.contains('`')
}

/// 规则匹配：普通规则按子串匹配；以 `/` 结尾的规则（如 `rm -rf /`）要求其后是边界，
/// 否则 `rm -rf /tmp/build` 会被误判成「删根」而直接硬拒（它本该走审批）。
fn matches_rule(haystack_lower: &str, rule: &str) -> bool {
    let rule = rule.trim().to_lowercase();
    if rule.is_empty() {
        return false;
    }
    if !rule.ends_with('/') {
        return haystack_lower.contains(&rule);
    }
    let mut from = 0;
    while let Some(i) = haystack_lower[from..].find(&rule) {
        let end = from + i + rule.len();
        let boundary = haystack_lower[end..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | '*' | ';' | '&' | '|' | ')' | '>'));
        if boundary {
            return true;
        }
        from = end;
    }
    false
}

/// 纯标签输出（`echo '=== uptime ==='`）：白名单模式下放行，避免把固定模板里的
/// 分组标题当成未授权命令。
fn is_echo_only(segment: &str) -> bool {
    segment.starts_with("echo ")
        && !segment.contains('>')
        && !segment.contains('`')
        && !segment.contains("$(")
}

/// 凭据泄露防护：命令里出现读凭据的动作即硬拒绝（AI 与真实模板都过这一关）。
fn leak_block(g: &GuardInner, command: &str) -> Option<Decision> {
    for re in &g.leak.cmd_blockers {
        if re.is_match(command) {
            return Some(Decision::Blocked(format!(
                "命中凭据泄露防护规则（{}）——AI 不得读取或回显敏感信息",
                re.as_str()
            )));
        }
    }
    None
}

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
            // URL 内嵌账号口令：https://user:pass@host
            (
                Regex::new(r"(?i)(https?://)[^/\s:@]+:[^/\s@]+@").expect("url cred regex"),
                "$1[REDACTED]@",
            ),
            // 键值形态：password=hunter2 / token: xxx / api_key=...
            (
                Regex::new(r"(?i)\b(pass(?:word)?|pwd|secret|api[_-]?key|access[_-]?key|token|credential)s?\b\s*[=:]\s*\S+")
                    .expect("cred regex"),
                "$1=[REDACTED]",
            ),
            // 短选项口令：mysql -pHunter2 / -p 'x' / --password xxx / --token=xxx
            (
                Regex::new(r"(?i)(\s-p)(?:=|\s+)?['\x22]?[^\s;&|'\x22]{2,}").expect("short pass regex"),
                "$1 [REDACTED]",
            ),
            (
                Regex::new(r"(?i)(--(?:password|passwd|pass|token|secret|api[-_]?key)(?:=|\s+))['\x22]?[^\s;&|'\x22]+")
                    .expect("long pass regex"),
                "$1[REDACTED]",
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

/// 判定结论。`Clone` 供 AI 审查层（R8.2）按值合并：final = max(static, ai)。
#[derive(Debug, Clone)]
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
    ///
    /// 受限模式（只读 / 白名单）按**子命令**分别判定：`uptime; cp /etc/passwd /tmp/x`
    /// 不会因为前半段只读就整体放行；同时拒绝命令替换（`$(...)`、反引号），
    /// 因为那种构造无法在文本层判定真实动作。
    pub fn evaluate(&self, command: &str) -> Decision {
        let g = self.read();
        if let Some(blocked) = leak_block(&g, command) {
            return blocked;
        }
        let lower = command.to_lowercase();
        let normalized = strip_null_redirections(command);
        let restricted = g.rules.readonly || g.rules.whitelist_mode;
        if restricted && has_substitution(command) {
            return Decision::Blocked(
                "受限模式已开启：命令包含命令替换/进程替换，无法安全判定".to_string(),
            );
        }
        // 只读模式：写入类命令直接拒（不提供审批逃生口）
        if g.rules.readonly && g.leak.writeish.is_match(&normalized) {
            return Decision::Blocked(
                "只读模式已开启：AI 不得执行写入类命令".to_string(),
            );
        }
        for p in &g.rules.deny {
            if matches_rule(&lower, p) {
                return Decision::Blocked(format!("命中高危命令黑名单「{}」", p.trim()));
            }
        }
        // 白名单模式：每一条子命令都必须命中白名单（纯 echo 标签除外）
        if g.rules.whitelist_mode {
            for seg in split_segments(&normalized) {
                if is_echo_only(seg) {
                    continue;
                }
                let seg_lower = seg.to_lowercase();
                let listed = g
                    .rules
                    .whitelist
                    .iter()
                    .filter(|w| !w.trim().is_empty())
                    .any(|w| seg_lower.contains(&w.trim().to_lowercase()));
                if !listed {
                    return Decision::Blocked(format!(
                        "白名单模式已开启：子命令「{}」不在允许清单内",
                        crate::text::truncate(seg, 80)
                    ));
                }
            }
        }
        for p in &g.rules.approval {
            if matches_rule(&lower, p) {
                return Decision::NeedsApproval(format!("命中需审批规则「{}」", p.trim()));
            }
        }
        Decision::Allow
    }

    /// 固定安全模板（观测五件套 / 状态探查）的判定。
    ///
    /// 这类命令不是 AI 自由输入的，而是应用按固定模板生成、参数只经 `sh_quote` 注入，
    /// 因此照常过泄露防护、硬黑名单与只读写入检查，但不套用白名单与审批表——
    /// 否则白名单模式下「列出目录」这类只读能力会被整块封死，而它们本就不是任意命令。
    pub fn evaluate_fixed_template(&self, command: &str) -> Decision {
        let g = self.read();
        if let Some(blocked) = leak_block(&g, command) {
            return blocked;
        }
        let normalized = strip_null_redirections(command);
        if g.rules.readonly && g.leak.writeish.is_match(&normalized) {
            return Decision::Blocked(
                "只读模式已开启：AI 不得执行写入类命令".to_string(),
            );
        }
        let lower = command.to_lowercase();
        for p in &g.rules.deny {
            if matches_rule(&lower, p) {
                return Decision::Blocked(format!("命中高危命令黑名单「{}」", p.trim()));
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
        let pending = q.create("s1", "srv-01", "dd if=/dev/zero of=/tmp/x", None, tx);
        assert_eq!(q.pending().len(), 1);
        assert!(q.decide(&pending.id, true));
        assert_eq!(rx.await, Ok(true));
        assert!(q.pending().is_empty());
    }

    #[tokio::test]
    async fn approval_reject_flow() {
        let q = Approvals::default();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending = q.create("s1", "srv-01", "reboot", None, tx);
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

    #[test]
    fn readonly_ignores_dev_null_redirection() {
        let mut r = GuardRules::default();
        r.readonly = true;
        let g = guard_with(r);
        // server_status 这类只读模板含 2>/dev/null，不该被判成写入
        let status = "echo '=== cpu ==='; nproc 2>/dev/null; \
                      free -m 2>/dev/null || vm_stat 2>/dev/null; df -h / 2>/dev/null";
        assert!(matches!(g.evaluate_fixed_template(status), Decision::Allow), "{status}");
        assert!(matches!(g.evaluate(status), Decision::Allow), "{status}");
        // 真写入仍然拦：cp / 命令替换 / find -delete / 重定向
        assert!(matches!(g.evaluate("uptime && cp /etc/passwd /tmp/x"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("find /tmp -name '*.log' -delete"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("uptime > /tmp/out"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("uptime $(id)"), Decision::Blocked(_)));
    }

    #[test]
    fn deny_rule_boundary_avoids_false_positive() {
        let g = guard_with(GuardRules::default());
        // 删根：硬拒
        assert!(matches!(g.evaluate("rm -rf /"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("rm -rf /*"), Decision::Blocked(_)));
        // 删临时目录：不该硬拒，但必须走审批
        assert!(matches!(g.evaluate("rm -rf /tmp/build"), Decision::NeedsApproval(_)));
    }

    #[test]
    fn whitelist_checks_every_segment() {
        let mut r = GuardRules::default();
        r.whitelist_mode = true;
        r.whitelist = vec!["uptime".into()];
        let g = guard_with(r);
        assert!(matches!(g.evaluate("uptime"), Decision::Allow));
        // 复合命令里夹带未授权动作 → 拒绝
        assert!(matches!(g.evaluate("uptime; cp /etc/passwd /tmp/x"), Decision::Blocked(_)));
        assert!(matches!(g.evaluate("uptime && nc -l 4444"), Decision::Blocked(_)));
        // 纯标签输出仍放行
        assert!(matches!(g.evaluate("echo '--- uptime ---'; uptime"), Decision::Allow));
    }

    #[test]
    fn fixed_template_bypasses_whitelist_but_not_leak_guard() {
        let mut r = GuardRules::default();
        r.whitelist_mode = true;
        r.whitelist = vec!["uptime".into()];
        let g = guard_with(r);
        let tpl = "ls -lah '/var/log' 2>&1 | head -200";
        assert!(matches!(g.evaluate_fixed_template(tpl), Decision::Allow));
        assert!(matches!(g.evaluate(tpl), Decision::Blocked(_)), "自由命令仍受白名单约束");
        // 模板也不得读凭据
        assert!(matches!(
            g.evaluate_fixed_template("cat /root/.ssh/id_rsa"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn sanitize_masks_short_and_url_credentials() {
        let g = guard_with(GuardRules::default());
        let out = g.sanitize("mysql -uroot -pHunter2 -e 'show databases'");
        assert!(!out.contains("Hunter2"), "{out}");
        let out = g.sanitize("git clone https://bob:s3cr3t@example.com/r.git");
        assert!(!out.contains("s3cr3t") && !out.contains("bob"), "{out}");
        let out = g.sanitize("app --password=hunter2 --token abcdef");
        assert!(!out.contains("hunter2") && !out.contains("abcdef"), "{out}");
    }
}
