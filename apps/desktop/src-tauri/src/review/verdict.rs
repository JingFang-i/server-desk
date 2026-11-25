//! AI 审查结论：输出解析（容错）与判定合并（安全不变式的唯一实现点）。
//!
//! 不变式（docs/r8-ai-review-design.md §1，由下方穷举测试守护）：
//! `final = max(static, ai)`，序关系 `Allow < NeedsApproval < Blocked`。
//! AI 只能收紧、永不放宽；静态 `Blocked` 在上游已短路不送审——模型永远碰不到。

use crate::guard::Decision;
use crate::review::config::{DenyMode, FailurePolicy, ReviewConfig};
use crate::text::truncate;

/// AI 风险分级（展示与日志用，不参与合并序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

impl Risk {
    fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            "critical" => Self::Critical,
            // 缺省 / 非法 → medium（偏保守，仅影响展示）
            _ => Self::Medium,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// AI 三态结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiVerdict {
    Allow,
    Review,
    Deny,
}

impl AiVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Review => "review",
            Self::Deny => "deny",
        }
    }
}

/// 一次审查的结论（成功或格式降级）。
#[derive(Debug, Clone)]
pub struct AiReview {
    pub verdict: AiVerdict,
    pub risk: Risk,
    pub reason: String,
    /// 风险类别（模型输出，最多取 4 个）：destructive/credential/network/
    /// privilege/service/data/unknown；缺失为空。仅展示与日志用，不参与合并序。
    /// 消费方：R8.4 审批卡展示、R8.5 审查日志（当前先采集）。
    #[allow(dead_code)]
    pub categories: Vec<String>,
    pub cached: bool,
    pub latency_ms: u64,
    pub model: String,
}

/// 解析 categories：字符串数组，最多取 4 个，单项截断 24 字；缺失/类型错 → 空。
fn parse_categories(v: &serde_json::Value) -> Vec<String> {
    v.get("categories")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str())
                .map(|s| truncate(s.trim(), 24))
                .filter(|s| !s.is_empty())
                .take(4)
                .collect()
        })
        .unwrap_or_default()
}

/// 从模型输出解析结论。
/// - 找不到完整 JSON 对象 / 全部损坏 → `Err`（走失败策略，绝不默认放行）；
/// - 存在 JSON 但没有合法 verdict（含「诱饵对象」在前的情况）→ 逐个扫描，
///   取第一个含合法 verdict 的对象；全部不合法 → `Ok`（格式降级为 `Review`，绝不 allow）。
pub fn parse(content: &str) -> Result<AiReview, String> {
    let objects = extract_json_objects(content);
    if objects.is_empty() {
        return Err("模型未返回 JSON 对象".to_string());
    }
    let mut degraded_reason: Option<&'static str> = None;
    for obj in &objects {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) else {
            continue;
        };
        let raw = v
            .get("verdict")
            .or_else(|| v.get("decision"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let verdict = match raw.as_str() {
            "allow" | "pass" | "approve" => Some(AiVerdict::Allow),
            "review" | "manual" | "confirm" => Some(AiVerdict::Review),
            "deny" | "reject" | "block" => Some(AiVerdict::Deny),
            _ => None,
        };
        if let Some(verdict) = verdict {
            let reason = v
                .get("reason")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let reason = if reason.is_empty() {
                "（模型未给出理由）".to_string()
            } else {
                truncate(&reason, 200)
            };
            return Ok(AiReview {
                verdict,
                risk: Risk::parse(v.get("risk").and_then(|x| x.as_str()).unwrap_or("")),
                reason,
                categories: parse_categories(&v),
                cached: false,
                latency_ms: 0,
                model: String::new(),
            });
        }
        degraded_reason = Some(if raw.is_empty() {
            "模型输出缺少 verdict 字段，按需人工确认"
        } else {
            "模型输出 verdict 非法，按需人工确认"
        });
    }
    if let Some(reason) = degraded_reason {
        return Ok(AiReview {
            verdict: AiVerdict::Review,
            risk: Risk::Medium,
            reason: reason.to_string(),
            categories: Vec::new(),
            cached: false,
            latency_ms: 0,
            model: String::new(),
        });
    }
    Err("响应中未找到可解析的 JSON 对象".to_string())
}

/// 提取文本中所有平衡的 `{...}`（字符串感知：忽略引号内的花括号与转义）。
/// 命令文本可能夹带「诱饵 JSON」，必须逐个扫描而不是只取第一个。
fn extract_json_objects(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut closed = false;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if esc {
                esc = false;
                j += 1;
                continue;
            }
            match b {
                b'\\' if in_str => esc = true,
                b'"' => in_str = !in_str,
                b'{' if !in_str => depth += 1,
                b'}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&s[i..=j]);
                        closed = true;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if !closed {
            break; // 未闭合的残缺对象：整段丢弃
        }
        i = j + 1;
    }
    out
}

fn severity(d: &Decision) -> u8 {
    match d {
        Decision::Allow => 0,
        Decision::NeedsApproval(_) => 1,
        Decision::Blocked(_) => 2,
    }
}

/// 合并静态判定与 AI 结论。
///
/// 规则（设计 §2.2 判定合并表的实现）：
/// - `Blocked`：直接返回（上游本不该送审，此处为双保险）；
/// - `NeedsApproval`：流程不变，AI 结论仅增补理由；
/// - `Allow` + `Ok(Allow)` → Allow（唯一可直达执行的组合）；
/// - `Allow` + `Ok(Review)` → 审批；
/// - `Allow` + `Ok(Deny)` → 按 `deny_mode`（D1 已拍板：默认 Escalate）；
/// - `Allow` + `Err(e)` → 按 `failure_policy`（D2 已拍板：默认 Escalate）。
///
/// 末尾再取一次防御性 `max`：任何未来改动都不可能输出比静态更宽松的判定。
pub fn merge(
    static_decision: &Decision,
    ai: &Result<AiReview, String>,
    cfg: &ReviewConfig,
) -> Decision {
    let out = match static_decision {
        Decision::Blocked(_) => static_decision.clone(),
        Decision::NeedsApproval(reason) => match ai {
            Ok(r) if matches!(r.verdict, AiVerdict::Review | AiVerdict::Deny) => {
                Decision::NeedsApproval(format!("{reason}；AI 提示：{}", r.reason))
            }
            Ok(_) => static_decision.clone(),
            Err(e) => Decision::NeedsApproval(format!("{reason}；AI 审查降级：{e}")),
        },
        Decision::Allow => match ai {
            Ok(r) => match r.verdict {
                AiVerdict::Allow => Decision::Allow,
                AiVerdict::Review => {
                    Decision::NeedsApproval(format!("AI 审查建议人工确认：{}", r.reason))
                }
                AiVerdict::Deny => match cfg.deny_mode {
                    DenyMode::Escalate => {
                        Decision::NeedsApproval(format!("AI 建议拒绝（{}）：{}", r.risk.as_str(), r.reason))
                    }
                    DenyMode::Reject => Decision::Blocked(format!("AI 审查拒绝：{}", r.reason)),
                },
            },
            Err(e) => match cfg.failure_policy {
                FailurePolicy::Escalate => {
                    Decision::NeedsApproval(format!("AI 审查暂不可用，已转人工确认：{e}"))
                }
                // 降级放行：调用方必须在审计标记 degraded（review_and_merge 已处理）
                FailurePolicy::Allow => Decision::Allow,
                FailurePolicy::Block => {
                    Decision::Blocked(format!("AI 审查暂不可用，按失败策略拒绝：{e}"))
                }
            },
        },
    };
    if severity(&out) < severity(static_decision) {
        static_decision.clone()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ai(verdict: AiVerdict) -> Result<AiReview, String> {
        Ok(AiReview {
            verdict,
            risk: Risk::Medium,
            reason: "测试理由".to_string(),
            categories: Vec::new(),
            cached: false,
            latency_ms: 0,
            model: "m".into(),
        })
    }

    /// 穷举「静态 × AI 结论 × 失败 × 两种策略」全部组合，守护合并不变式。
    #[test]
    fn merge_never_loosens_static_decision() {
        let statics = [
            Decision::Allow,
            Decision::NeedsApproval("静态原因".into()),
            Decision::Blocked("静态硬拒".into()),
        ];
        let mut ais: Vec<Result<AiReview, String>> = vec![
            ai(AiVerdict::Allow),
            ai(AiVerdict::Review),
            ai(AiVerdict::Deny),
            Err("超时".into()),
        ];
        for deny_mode in [DenyMode::Escalate, DenyMode::Reject] {
            for failure_policy in [FailurePolicy::Escalate, FailurePolicy::Allow, FailurePolicy::Block] {
                let mut cfg = ReviewConfig::default();
                cfg.deny_mode = deny_mode;
                cfg.failure_policy = failure_policy;
                for s in &statics {
                    for a in &ais {
                        let out = merge(s, a, &cfg);
                        assert!(
                            severity(&out) >= severity(s),
                            "不变式被破坏：static={s:?} ai={a:?} → {out:?}"
                        );
                    }
                }
            }
        }
        ais.clear(); // 静默 unused_mut 提示来源
    }

    #[test]
    fn allow_plus_deny_follows_deny_mode() {
        let mut cfg = ReviewConfig::default();
        assert!(matches!(
            merge(&Decision::Allow, &ai(AiVerdict::Deny), &cfg),
            Decision::NeedsApproval(_)
        ));
        cfg.deny_mode = DenyMode::Reject;
        assert!(matches!(
            merge(&Decision::Allow, &ai(AiVerdict::Deny), &cfg),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn allow_plus_error_follows_failure_policy() {
        let err: Result<AiReview, String> = Err("网络错误".into());
        let mut cfg = ReviewConfig::default();
        assert!(matches!(
            merge(&Decision::Allow, &err, &cfg),
            Decision::NeedsApproval(_)
        ));
        cfg.failure_policy = FailurePolicy::Allow;
        assert!(matches!(merge(&Decision::Allow, &err, &cfg), Decision::Allow));
        cfg.failure_policy = FailurePolicy::Block;
        assert!(matches!(merge(&Decision::Allow, &err, &cfg), Decision::Blocked(_)));
    }

    #[test]
    fn approval_never_downgraded_by_ai_allow() {
        let s = Decision::NeedsApproval("静态审批".into());
        assert!(matches!(
            merge(&s, &ai(AiVerdict::Allow), &ReviewConfig::default()),
            Decision::NeedsApproval(_)
        ));
    }

    #[test]
    fn parse_reads_json_with_chatter_and_fences() {
        let a = parse("好的，结论如下：\n```json\n{\"verdict\":\"deny\",\"risk\":\"high\",\"reason\":\"删除系统文件\"}\n```").unwrap();
        assert_eq!(a.verdict, AiVerdict::Deny);
        assert_eq!(a.risk, Risk::High);
        assert_eq!(a.reason, "删除系统文件");
    }

    #[test]
    fn parse_accepts_alias_fields_and_synonyms() {
        let a = parse("{\"decision\":\"reject\",\"risk\":\"critical\",\"reason\":\"x\"}").unwrap();
        assert_eq!(a.verdict, AiVerdict::Deny);
        let b = parse("{\"verdict\":\"pass\"}").unwrap();
        assert_eq!(b.verdict, AiVerdict::Allow);
    }

    #[test]
    fn parse_invalid_verdict_degrades_to_review_never_allow() {
        let a = parse("{\"verdict\":\"当然可以\"}").unwrap();
        assert_eq!(a.verdict, AiVerdict::Review);
        let b = parse("{\"risk\":\"low\"}").unwrap();
        assert_eq!(b.verdict, AiVerdict::Review);
    }

    #[test]
    fn parse_fails_when_no_json() {
        assert!(parse("我觉得没问题").is_err());
        assert!(parse("{\"verdict\": truncated").is_err());
    }

    #[test]
    fn parse_reads_categories_and_caps_them() {
        let a = parse("{\"verdict\":\"deny\",\"risk\":\"critical\",\"reason\":\"x\",\"categories\":[\"network\",\"destructive\"]}").unwrap();
        assert_eq!(a.categories, vec!["network", "destructive"]);
        let b = parse("{\"verdict\":\"allow\",\"reason\":\"x\",\"categories\":[1,2,\"data\",\"a\",\"b\"]}").unwrap();
        assert_eq!(b.categories, vec!["data", "a", "b"], "非字符串项被过滤，最多取 4 个");
        let c = parse("{\"verdict\":\"review\",\"reason\":\"x\"}").unwrap();
        assert!(c.categories.is_empty(), "缺失 categories 不报错");
    }

    #[test]
    fn parse_ignores_braces_inside_strings() {
        let a = parse("noise {\"a\":\"}\"} {\"verdict\":\"deny\",\"reason\":\"含大括号\"}").unwrap();
        assert_eq!(a.verdict, AiVerdict::Deny);
    }
}
