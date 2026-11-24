//! 文本截断与摘要预算：审计 / 审批卡 / 通知共用一套实现。
//!
//! 之前 `ops.rs`、`commands.rs` 各有一份逐字相同的 `truncate`，摘要长度预算也散落
//! 在 5 处（140/110/500/42/200）。集中到这里后，改预算只改一处。

/// 系统通知 / 应用内提醒正文上限（字符）。
pub const SUMMARY_MAX: usize = 140;
/// 审计 detail 上限（字符）。
pub const AUDIT_DETAIL_MAX: usize = 500;
/// 错误摘要上限（字符）。
pub const ERROR_MAX: usize = 200;
/// 观测工具命令摘要上限（字符）。
pub const COMMAND_DIGEST_MAX: usize = 200;
/// 审批卡保留的命令长度：够看清整条命令，又不会无限膨胀。
pub const APPROVAL_DETAIL_MAX: usize = 2000;
/// 服务器名称快照上限（字符）。
pub const SERVER_NAME_MAX: usize = 80;

/// 按**字符**截断：不会切坏 UTF-8，也不会 panic（区别于 `String::truncate` 的字节语义）。
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// 折叠为单行：换行/制表/连续空白压成一个空格（通知标题行、审计摘要用）。
pub fn fold_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim_end().to_string()
}

/// 单行摘要：先折叠空白，再按字符截断。
pub fn summary(s: &str, max: usize) -> String {
    truncate(&fold_ws(s), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_based() {
        // 中文与 emoji 都不能被切成半个字符
        let s = "生产环境重启 nginx 🚀";
        for n in 0..=s.chars().count() {
            let out = truncate(s, n);
            assert!(out.chars().count() <= n);
            assert!(s.starts_with(&out));
        }
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn fold_ws_collapses_newlines() {
        assert_eq!(fold_ws("a\n\n b\t c "), "a b c");
        assert_eq!(fold_ws("   "), "");
        assert_eq!(summary("x\n\ny", 10), "x y");
    }
}
