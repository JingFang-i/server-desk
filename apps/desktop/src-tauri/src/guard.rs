//! AI 安全执行管线：高危命令拦截（黑名单硬拒绝）、敏感操作人工审批、
//! 凭据泄露防护（固定正则）、输出与审计脱敏。仅作用于 AI 来源的操作；
//! 用户手动操作不受拦截，但审计 detail 一律脱敏。

use crate::store::{group_matches, Store};
use regex::Regex;
use serde::Serialize;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const META_GUARD_CONFIG: &str = "guard_config";
const META_MCP_ENABLED: &str = "mcp_enabled";
pub const APPROVAL_TIMEOUT_SECS: u64 = 120;

/// 用户可维护的拦截/审批规则（子串匹配，大小写不敏感）。
///
/// 序列化统一 camelCase：前端 `GuardRules` 按 camelCase 提交（whitelistMode / ratePerMin /
/// strictGroups），此前结构体是 snake_case 且开了 `serde(default)`，这些字段会被静默当成
/// 未知字段丢弃——于是「白名单模式 / 速率限制 / 严格分组」永远存不进去（只读模式是单词，
/// 恰好命中，才显得「有的能存有的不能」）。旧库里已存的 JSON 是 snake_case，靠 alias 兼容。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GuardRules {
    /// 硬拒绝（AI 不可执行，也不可申请审批）
    pub deny: Vec<String>,
    /// 需人工审批（AI 提交 → 应用弹窗 → 用户批准后执行；超时自动拒绝）
    pub approval: Vec<String>,
    /// 白名单模式：true 时 AI 命令必须命中 whitelist 才放行
    #[serde(alias = "whitelist_mode")]
    pub whitelist_mode: bool,
    /// 白名单条目（子串匹配）
    pub whitelist: Vec<String>,
    /// 只读模式：拦截 AI 的一切写入类命令
    pub readonly: bool,
    /// AI 调用速率限制（次/分钟，0 = 不限）
    #[serde(alias = "rate_per_min")]
    pub rate_per_min: u32,
    /// 严格分组：这些分组的服务器上，AI 的一切 exec 都需审批
    #[serde(alias = "strict_groups")]
    pub strict_groups: Vec<String>,
}

#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuardRuleScope {
    #[default]
    All,
    Rules,
    Policy,
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

/// 命令是否涉及 `.env` 环境文件（`.env` / `.env.local` / `--env-file=.env` 等）。
/// 命中不硬拒，转人工审批——AI 排查配置时仍可读，但必须经过用户确认。
fn mentions_dotenv(lower_cmd: &str) -> bool {
    lower_cmd.contains(".env")
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
///
/// 例外：命令涉及 `.env` 环境文件时，`env` 关键字的硬拒让位给「转人工审批」
/// （见 `evaluate` 的 dotenv 分支）——否则 `cat /opt/app/.env` 会被
/// `\benv\b` 当环境变量导出拦死，永远到不了审批卡。文件里的凭据仍由
/// 输出脱敏兜底，且用户必须在审批卡上确认。
fn leak_block(g: &GuardInner, command: &str) -> Option<Decision> {
    let mentions_dotenv = command.to_lowercase().contains(".env");
    if !mentions_dotenv && g.leak.env_word.is_match(command) {
        return Some(Decision::Blocked("命中凭据泄露防护规则「env」".into()));
    }
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
    env_word: Regex,
    cmd_blockers: Vec<Regex>,
    out_scrubbers: Vec<(Regex, &'static str)>,
    writeish: Regex,
}

impl LeakGuard {
    fn build() -> Self {
        let cmd_blockers = [
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
            // 旧版凭据副本探测——句柄明文所在目录与进程环境嗅探，一律硬拒
            r"serverdesk-secrets",
            r"strings\s+/proc",
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
                Regex::new(r"(?i)((?:https?|rediss?)://)[^/\s:@]*:[^/\s@]+@").expect("url cred regex"),
                "$1[REDACTED]@",
            ),
            // 键值形态：password=hunter2 / token: xxx / api_key=...
            (
                Regex::new(r"(?i)\b(pass(?:word)?|pwd|secret|api[_-]?key|access[_-]?key|token|credential)s?\b\s*[=:]\s*\S+")
                    .expect("cred regex"),
                "$1=[REDACTED]",
            ),
            (
                Regex::new(r#"(?i)(\bredis-cli\b[^\r\n]*?(?:\s-a(?:=|\s+)|\s--pass(?:=|\s+)))(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s;&|]+)"#)
                    .expect("redis password regex"),
                "$1[REDACTED]",
            ),
            (
                Regex::new(r#"(?i)(\bredis-cli\b[^\r\n]*?\s--user(?:=|\s+))(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s;&|]+)"#)
                    .expect("redis user regex"),
                "$1[REDACTED]",
            ),
            (
                Regex::new(r#"(?i)(\bREDISCLI_AUTH\s*=\s*)(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s;&|]+)"#)
                    .expect("redis auth env regex"),
                "$1[REDACTED]",
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
        Self {
            env_word: Regex::new(r"(?i)\benv\b").expect("env regex"),
            cmd_blockers, out_scrubbers, writeish,
        }
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
    // SSH 密钥目录整体硬拒：自建密钥名（deploy_key 等）不在具体文件名黑名单里，
    // 读出来就是私钥泄漏（输出脱敏挡不住单行 base64 私钥）
    ".ssh/",
    // 注意：`.env` 不在子串表里——由 evaluate_read_path 按 basename 判定为「转人工审批」，
    // 避免子串匹配把 /etc 下 `app.env` 之类也一并硬拒
    "credential",
    "secret",
    ".gnupg",
    ".kube/config",
    ".aws/",
    ".netrc",
    ".git-credentials",
    ".docker/config.json",
    // 旧版凭据副本目录（tmpfs），读取一律硬拒
    "/run/serverdesk-secrets",
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
    // 旧版凭据目录不得写入/覆盖
    "/run/serverdesk-secrets",
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

    /// 按当前资产分组判定。持锁读取分组，避免改名时拿旧分组匹配新策略。
    pub fn server_requires_approval(&self, store: &Store, id: &str) -> Result<bool, String> {
        let g = self.read();
        if g.rules.strict_groups.is_empty() {
            return Ok(false);
        }
        Ok(store.server_group(id)?.is_some_and(|name| {
            !name.trim().is_empty() && g
                .rules
                .strict_groups
                .iter()
                .any(|group| group_matches(&name, group))
        }))
    }

    pub fn ai_enabled(&self) -> bool {
        self.read().ai_enabled
    }

    /// 保存指定卡片的字段。使用当前规则补齐未提交的字段，避免旧表单覆盖新策略。
    pub fn set_rules(&self, store: &Store, rules: GuardRules, scope: GuardRuleScope) -> Result<(), String> {
        let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let mut next = g.rules.clone();
        match scope {
            GuardRuleScope::All => next = rules,
            GuardRuleScope::Rules => {
                next.deny = rules.deny;
                next.approval = rules.approval;
                next.whitelist_mode = rules.whitelist_mode;
                next.whitelist = rules.whitelist;
            }
            GuardRuleScope::Policy => {
                next.readonly = rules.readonly;
                next.rate_per_min = rules.rate_per_min;
                next.strict_groups = rules.strict_groups;
            }
        }
        store.set_meta(
            META_GUARD_CONFIG,
            &serde_json::to_string(&next).map_err(|e| e.to_string())?,
        )?;
        g.rules = next;
        Ok(())
    }

    pub fn rename_group(&self, store: &Store, from: &str, to: &str) -> Result<usize, String> {
        let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if !g.rules.strict_groups.iter().any(|name| group_matches(name, from)) {
            return store.rename_group(from, to, &[]);
        }
        let mut next = g.rules.clone();
        next.strict_groups.retain(|name| !group_matches(name, from) && !group_matches(name, to));
        next.strict_groups.push(to.to_string());
        let json = serde_json::to_string(&next).map_err(|e| e.to_string())?;
        let updated = store.rename_group(from, to, &[(META_GUARD_CONFIG, &json)])?;
        if updated > 0 {
            g.rules = next;
        }
        Ok(updated)
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
        // `.env` 环境文件：不硬拒（AI 可能需要排查配置），一律转人工审批。
        // 放在白名单校验之前——白名单模式下也不该由「不在清单内」直接拒掉，
        // 而是交给用户判断。
        if mentions_dotenv(&lower) {
            return Decision::NeedsApproval(
                "命令涉及 .env 环境文件（可能含凭据），需人工确认".to_string(),
            );
        }
        // SSH 密钥目录：读/写/拷贝都转审批（命令级不硬拒，可能与密钥生成等正常运维混用）
        if lower.contains("/.ssh") || lower.starts_with(".ssh") {
            return Decision::NeedsApproval(
                "命令涉及 SSH 密钥目录（~/.ssh），需人工确认".to_string(),
            );
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

    /// AI 读取远程路径的判定。
    ///
    /// - 凭据/敏感文件（私钥、`/etc/shadow`、`authorized_keys` 等）→ **硬拒**；
    /// - `.env` 家族与 systemd 环境文件 → **转人工审批**（可能含凭据，但用户可决定是否放行，
    ///   内容仍由输出脱敏兜底）。
    pub fn evaluate_read_path(&self, path: &str) -> Decision {
        let lower = path.to_lowercase();
        for p in READ_PATH_DENY {
            if lower.contains(p) {
                return Decision::Blocked(format!(
                    "路径命中凭据/敏感文件保护规则「{p}」——AI 不得读取该文件"
                ));
            }
        }
        let basename = lower.rsplit('/').next().unwrap_or("");
        // 目录本身（如 list_files /root/.ssh）：子串 ".ssh/" 匹配不到，单独判一次
        if basename == ".ssh" || lower == "~/.ssh" {
            return Decision::Blocked(
                "路径命中凭据保护规则「.ssh/」——AI 不得列出或读取 SSH 密钥目录".to_string(),
            );
        }
        // `.env` 家族：按 basename 精确匹配（`.env` / `.env.local` / `.env.production`）
        if basename == ".env" || basename.starts_with(".env.") {
            return Decision::NeedsApproval(
                "读取 .env 环境文件（可能含凭据）需人工确认".to_string(),
            );
        }
        if lower.starts_with("/etc/systemd/")
            && (lower.ends_with("/env") || lower.ends_with(".env"))
        {
            return Decision::NeedsApproval(
                "读取 systemd 环境文件（可能含凭据）需人工确认".to_string(),
            );
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

    /// 按给定规则构造 Guard（本模块测试用；r4_tests 里另有一份同名 guard_with）。
    fn guard_rules(rules: GuardRules) -> Guard {
        Guard {
            inner: Arc::new(RwLock::new(GuardInner {
                rules,
                leak: LeakGuard::build(),
                ai_enabled: true,
            })),
        }
    }

    #[test]
    fn guard_rules_accept_camel_case_and_legacy_snake_case() {
        // 前端按 camelCase 提交：白名单模式 / 速率 / 严格分组再也不能被静默丢弃
        let camel: GuardRules = serde_json::from_str(
            r#"{"deny":[],"approval":[],"whitelistMode":true,"whitelist":["uptime"],
                "readonly":true,"ratePerMin":7,"strictGroups":["prod"]}"#,
        )
        .unwrap();
        assert!(camel.whitelist_mode);
        assert!(camel.readonly);
        assert_eq!(camel.rate_per_min, 7);
        assert_eq!(camel.strict_groups, vec!["prod"]);

        // 升级前写在 meta 里的 JSON 是 snake_case，仍要能读回来（否则规则会被重置）
        let legacy: GuardRules = serde_json::from_str(
            r#"{"deny":[],"approval":[],"whitelist_mode":true,"whitelist":[],
                "readonly":true,"rate_per_min":9,"strict_groups":["db"]}"#,
        )
        .unwrap();
        assert!(legacy.whitelist_mode);
        assert_eq!(legacy.rate_per_min, 9);
        assert_eq!(legacy.strict_groups, vec!["db"]);

        // 落库用 camelCase
        let json = serde_json::to_string(&camel).unwrap();
        assert!(json.contains("whitelistMode") && json.contains("ratePerMin"));
        assert!(!json.contains("whitelist_mode"));
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
        // .env 家族：转人工审批（不再硬拒）
        assert!(matches!(
            guard().evaluate_read_path("/home/u/app/.env"),
            Decision::NeedsApproval(_)
        ));
        // systemd 环境文件同样可能存放凭据。
        assert!(matches!(
            guard().evaluate_read_path("/opt/app/.env.production"),
            Decision::NeedsApproval(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/etc/systemd/system/app.env"),
            Decision::NeedsApproval(_)
        ));
        // 普通 unit 文件不受影响
        assert!(matches!(
            guard().evaluate_read_path("/etc/systemd/system/app.service"),
            Decision::Allow
        ));
        // SSH 密钥目录：文件名不在黑名单的自建密钥也硬拒，目录本身也拒
        assert!(matches!(
            guard().evaluate_read_path("/root/.ssh/deploy_key"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/home/u/.ssh"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/root/.ssh/known_hosts"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate_read_path("/var/log/app.log"),
            Decision::Allow
        ));
    }

    #[test]
    fn ssh_dir_commands_require_approval() {
        let g = guard();
        for cmd in [
            "cat ~/.ssh/deploy_key",
            "cp ~/.ssh/deploy_key /tmp/x",
            "ls -la /root/.ssh",
        ] {
            assert!(
                matches!(g.evaluate(cmd), Decision::NeedsApproval(_)),
                "{cmd} 应转人工审批"
            );
        }
        // 明确命中私钥文件名的命令仍硬拒（泄露防护优先于审批）
        assert!(matches!(
            g.evaluate("cat ~/.ssh/id_rsa"),
            Decision::Blocked(_)
        ));
        // 与密钥目录无关的 ssh 命令不受影响
        assert!(matches!(
            g.evaluate("systemctl status sshd"),
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
    fn blocks_legacy_credential_directory_probing() {
        let g = guard();
        // 旧版凭据目录与进程环境嗅探必须硬拒
        assert!(matches!(
            g.evaluate("cat /run/serverdesk-secrets/SD_SEC_a7b3c9d2"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            g.evaluate("ls -la /run/serverdesk-secrets"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            g.evaluate("strings /proc/1/environ | grep -i key"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            g.evaluate_read_path("/run/serverdesk-secrets/SD_SEC_a7b3c9d2"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            g.evaluate_write_path("/run/serverdesk-secrets/evil"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn dotenv_command_requires_approval() {
        let g = guard();
        // 读、写、拷贝、指定 env 文件的命令都转人工审批（不硬拒）
        for cmd in [
            "cat /opt/app/.env",
            "grep -i token /opt/app/.env.production",
            "echo NEW=1 >> /opt/app/.env",
            "cp .env .env.bak",
            "docker run --env-file=.env alpine env",
        ] {
            assert!(
                matches!(g.evaluate(cmd), Decision::NeedsApproval(_)),
                "{cmd} 应转人工审批"
            );
        }
        // 不含 .env 的普通命令不受影响
        assert!(matches!(g.evaluate("uptime && df -h /"), Decision::Allow));
        // 白名单模式：.env 命令仍走审批而不是「不在清单内」的硬拒
        let mut r = GuardRules::default();
        r.whitelist_mode = true;
        r.whitelist = vec!["uptime".into()];
        let gw = guard_rules(r);
        assert!(matches!(
            gw.evaluate("cat /opt/app/.env"),
            Decision::NeedsApproval(_)
        ));
        // 只读模式：写 .env 仍被硬拒（只读无审批逃生口）
        let mut ro = GuardRules::default();
        ro.readonly = true;
        let gro = guard_rules(ro);
        assert!(matches!(
            gro.evaluate("echo A=1 > /opt/app/.env"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn blocks_credential_leak_commands() {
        assert!(matches!(guard().evaluate("env | grep TOKEN"), Decision::Blocked(_)));
        assert!(matches!(guard().evaluate("cat ~/.ssh/id_rsa"), Decision::Blocked(_)));
        assert!(matches!(guard().evaluate("printenv"), Decision::Blocked(_)));
        assert!(matches!(
            guard().evaluate("cat /etc/systemd/env"),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            guard().evaluate("sudo env"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn systemd_service_inspection_escalates_environment_files() {
        let g = guard();
        // 命令级：`.env` 不再硬拒，转人工审批（用户显式要求「不直接拒绝」）
        assert!(matches!(
            g.evaluate("cat /etc/systemd/system/app.env"),
            Decision::NeedsApproval(_)
        ));
        assert!(matches!(g.evaluate("systemctl status example.service"), Decision::Allow));
        assert!(matches!(g.evaluate("cat /opt/app/app.env"), Decision::NeedsApproval(_)));
        // 文件工具级：`.env` 家族同样转人工审批（与命令执行策略一致）
        assert!(matches!(
            g.evaluate_read_path("/opt/app/.env"),
            Decision::NeedsApproval(_)
        ));
        // 其它凭据文件仍然硬拒
        assert!(matches!(
            g.evaluate_read_path("/root/.ssh/id_rsa"),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn redacts_redis_authentication_values() {
        let g = guard();
        for text in [
            r#"redis-cli -a "test value" ping"#,
            "redis-cli -a fixture-password --user=fixture-user ping",
            "REDISCLI_AUTH=fixture-password redis-cli ping",
            "redis-cli -u rediss://fixture-user:fixture-password@localhost ping",
        ] {
            let redacted = g.sanitize(text);
            assert_ne!(redacted, text);
            assert!(!redacted.contains("test value"));
            assert!(!redacted.contains("fixture-"));
        }
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
    fn strict_group_rename_preserves_policy_after_reload() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        for (id, name) in [("a", "\tPrOd "), ("b", "生产"), ("c", "dev")] {
            store.insert(&crate::store::ServerRow {
                id: id.into(), name: id.into(), host: "localhost".into(), port: 22,
                username: "test".into(), auth_type: "password".into(), secret_blob: vec![],
                group_name: Some(name.into()), tags: "[]".into(), notes: None, allow_ai: true,
                created_at: String::new(), updated_at: String::new(), sort_order: 0,
            }).unwrap();
        }
        let guard = Guard::load(&store);
        let rules = GuardRules { strict_groups: vec!["prod".into()], ..GuardRules::default() };
        guard.set_rules(&store, rules, GuardRuleScope::All).unwrap();
        assert!(guard.server_requires_approval(&store, "a").unwrap());
        assert!(!guard.server_requires_approval(&store, "b").unwrap());
        assert_eq!(guard.rename_group(&store, " PROD ", "生产").unwrap(), 1);
        for guard in [guard, Guard::load(&store)] {
            assert_eq!(guard.info().strict_groups, vec!["生产"]);
            assert_eq!(store.server_group("a").unwrap().as_deref(), Some("生产"));
            assert!(guard.server_requires_approval(&store, "a").unwrap());
            assert!(guard.server_requires_approval(&store, "b").unwrap());
            assert!(!guard.server_requires_approval(&store, "c").unwrap());
        }
    }

    #[test]
    fn saving_one_card_preserves_the_other_cards_rules() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let guard = Guard::load(&store);
        let rules = GuardRules { deny: vec!["第一条".into()], readonly: true, ..GuardRules::default() };
        guard.set_rules(&store, rules.clone(), GuardRuleScope::Policy).unwrap();
        assert!(guard.info().readonly);
        assert_ne!(guard.info().deny, rules.deny);

        let rules = GuardRules { readonly: false, ..rules };
        guard.set_rules(&store, rules, GuardRuleScope::Rules).unwrap();
        let saved = Guard::load(&store).info();
        assert!(saved.readonly);
        assert_eq!(saved.deny, vec!["第一条"]);
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
