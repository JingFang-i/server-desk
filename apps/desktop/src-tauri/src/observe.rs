//! R1 观测五件套：list_files / tail_log / log_search / list_services / port_check。
//! 每个工具都是**固定安全命令模板**（参数仅经 sh_quote 注入），不走自由命令黑名单，
//! 但保留：allow_ai 闸门 → 文件路径泄露防护（READ_PATH_DENY）→ 执行 → 输出脱敏 → 审计。

use crate::ops::{ai_gate, audit, conn_params, load_server_config};
use crate::state::AppState;
use crate::ssh::ExecResult;
use std::time::Duration;

/// POSIX 安全引用：单引号包裹，内部单引号转义。
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------- 命令模板（纯函数，可单测） ----------

pub fn build_list_files_cmd(path: &str) -> String {
    format!("ls -lah {} 2>&1 | head -200", sh_quote(path))
}

pub fn build_tail_log_cmd(path: &str, lines: u32) -> String {
    let lines = lines.clamp(1, 2000);
    format!("tail -n {} {} 2>&1", lines, sh_quote(path))
}

pub fn build_log_search_cmd(
    path: &str,
    pattern: &str,
    ignore_case: bool,
    max_matches: u32,
    context: u32,
) -> String {
    let max_matches = max_matches.clamp(1, 200);
    let context = context.clamp(0, 5);
    let flag = if ignore_case { " -i" } else { "" };
    format!(
        "grep -n{} -m {} -C {} -e {} {} 2>&1 | head -600",
        flag,
        max_matches,
        context,
        sh_quote(pattern),
        sh_quote(path)
    )
}

pub fn build_list_services_cmd() -> String {
    "systemctl list-units --type=service --all --no-pager --plain 2>&1 | head -150 || service --status-all 2>&1 | head -100".to_string()
}

pub fn build_port_check_cmd(port: u16) -> String {
    format!(
        "(ss -tlnp 2>/dev/null || netstat -tlnp 2>/dev/null) | grep -E '[:.]{0}\\b' || echo 'no listener on port {0}'",
        port
    )
}

// ---------- 执行与审计 ----------

fn cmd_digest(c: &str) -> String {
    crate::commands::truncate(c, 200)
}

async fn observe_exec(
    state: &AppState,
    cfg: &shared::ServerConfig,
    action: &str,
    command: &str,
    timeout_secs: u64,
) -> Result<ExecResult, String> {
    let res = state
        .ssh
        .exec(
            &state.store,
            &cfg.id,
            &conn_params(cfg),
            command,
            Duration::from_secs(timeout_secs.clamp(5, 120)),
        )
        .await;
    match &res {
        Ok(r) => audit(
            state,
            "ai",
            Some(&cfg.id),
            action,
            &cmd_digest(command),
            &format!("exit={:?} bytes={}", r.exit_code, r.stdout.len()),
        ),
        Err(e) => audit(state, "ai", Some(&cfg.id), action, &cmd_digest(command), &crate::commands::truncate(e, 200)),
    }
    match res {
        Ok(r) => Ok(ExecResult {
            stdout: state.guard.sanitize(&r.stdout),
            stderr: state.guard.sanitize(&r.stderr),
            exit_code: r.exit_code,
            truncated: r.truncated,
        }),
        Err(e) => Err(e),
    }
}

fn gate_path(state: &AppState, cfg: &shared::ServerConfig, action: &str, path: &str) -> Result<(), String> {
    ai_gate(state, cfg, action, path)?;
    if let crate::guard::Decision::Blocked(reason) = state.guard.evaluate_read_path(path) {
        audit(state, "ai", Some(&cfg.id), action, path, "blocked");
        return Err(format!("已拦截：{reason}"));
    }
    Ok(())
}

// ---------- 对外入口 ----------

pub async fn list_files(state: &AppState, server_ref: &str, path: &str) -> Result<ExecResult, String> {
    let cfg = load_server_config(state, server_ref)?;
    gate_path(state, &cfg, "mcp_list_files", path)?;
    observe_exec(state, &cfg, "mcp_list_files", &build_list_files_cmd(path), 30).await
}

pub async fn tail_log(state: &AppState, server_ref: &str, path: &str, lines: u32) -> Result<ExecResult, String> {
    let cfg = load_server_config(state, server_ref)?;
    gate_path(state, &cfg, "mcp_tail_log", path)?;
    observe_exec(state, &cfg, "mcp_tail_log", &build_tail_log_cmd(path, lines), 30).await
}

pub async fn log_search(
    state: &AppState,
    server_ref: &str,
    path: &str,
    pattern: &str,
    ignore_case: bool,
    max_matches: u32,
    context: u32,
) -> Result<ExecResult, String> {
    let cfg = load_server_config(state, server_ref)?;
    gate_path(state, &cfg, "mcp_log_search", path)?;
    let cmd = build_log_search_cmd(path, pattern, ignore_case, max_matches, context);
    observe_exec(state, &cfg, "mcp_log_search", &cmd, 60).await
}

pub async fn list_services(state: &AppState, server_ref: &str) -> Result<ExecResult, String> {
    let cfg = load_server_config(state, server_ref)?;
    ai_gate(state, &cfg, "mcp_list_services", "列出服务")?;
    observe_exec(state, &cfg, "mcp_list_services", &build_list_services_cmd(), 30).await
}

pub async fn port_check(state: &AppState, server_ref: &str, port: u16) -> Result<ExecResult, String> {
    let cfg = load_server_config(state, server_ref)?;
    ai_gate(state, &cfg, "mcp_port_check", &format!("port {port}"))?;
    observe_exec(state, &cfg, "mcp_port_check", &build_port_check_cmd(port), 30).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh_quote_wraps_and_escapes() {
        assert_eq!(sh_quote("/var/log/app.log"), "'/var/log/app.log'");
        let evil = "a'; rm -rf /; echo '";
        let q = sh_quote(evil);
        assert!(q.starts_with('\'') && q.ends_with('\''));
        assert!(q.contains(r"'\''"), "内部单引号必须转义: {q}");
    }

    #[test]
    fn templates_embed_quoted_paths() {
        let c = build_list_files_cmd("/etc/nginx");
        assert!(c.contains("'/etc/nginx'"), "{c}");
        let t = build_tail_log_cmd("/var/log/a b.log", 100);
        assert!(t.contains("'/var/log/a b.log'"), "{t}");
        let s = build_log_search_cmd("/var/log/x", "Err'; drop", true, 50, 2);
        assert!(s.contains(" -i"), "{s}");
        assert!(s.contains("'Err'\\''; drop'"), "{s}");
        let p = build_port_check_cmd(8080);
        assert!(p.contains("8080") && p.contains("[:.]"), "{p}");
        assert!(build_list_services_cmd().contains("systemctl"));
    }

    #[test]
    fn params_clamped() {
        let c = build_tail_log_cmd("/x", 99_999);
        assert!(!c.contains("99999"), "行数必须被夹紧: {c}");
        assert!(c.contains("tail -n 2000"), "{c}");
        let s = build_log_search_cmd("/x", "p", false, 9999, 99);
        assert!(!s.contains("-m 9999") && !s.contains("-C 99"), "{s}");
        assert!(s.contains("-m 200") && s.contains("-C 5"), "{s}");
    }
}
