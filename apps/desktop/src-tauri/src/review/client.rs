//! OpenAI 兼容 `/chat/completions` 客户端（最小实现）。
//!
//! R8.1 只承载「测试连接」；R8.3 在其上叠加审查 prompt、JSON 解析容错。
//! 「`response_format` 不支持时自动退化重试一次」的语义在这里实现。
//!
//! 失败语义（关键路径约定）：**不重试** 429/5xx 与网络错误——审查插在每条
//! AI 命令的关键路径上，等不起退避；失败直接上抛，由失败策略
//! （默认 `escalate`，转人工审批）兜底。

use crate::review::config::{Protocol, ReviewConfig};
use serde_json::{json, Value};
use std::time::Instant;

/// 一次成功调用的产物。
/// `tokens_in/out` 在 R8.1 仅采集不消费（R8.5 落入审查日志做成本观测），先显式豁免 dead_code。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ChatOutcome {
    pub content: String,
    pub model: String,
    pub latency_ms: u64,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

/// 单轮 chat 调用，要求 JSON 输出。
/// 供应商不支持 `response_format`（HTTP 400）时自动去掉该字段重试一次；
/// 错误仅返回状态或通用说明，避免第三方错误正文携带凭据。
pub async fn chat_json(
    http: &reqwest::Client,
    cfg: &ReviewConfig,
    api_key: Option<&str>,
    system: &str,
    user: &str,
    deadline: Instant,
) -> Result<ChatOutcome, String> {
    debug_assert!(matches!(cfg.protocol, Protocol::OpenAiCompat));
    let url = cfg.endpoint()?;

    let started = Instant::now();
    for with_response_format in [true, false] {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("审查请求超时".into());
        }
        let mut body = json!({
            "model": cfg.model,
            "temperature": 0,
            "max_tokens": cfg.max_tokens,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        if with_response_format {
            body["response_format"] = json!({ "type": "json_object" });
        }
        let mut req = http.post(url.clone()).timeout(remaining).json(&body);
        if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // 网络/超时：换 response_format 也没用，直接失败
                return Err(format!(
                    "请求失败（timeout={}ms）：{}",
                    cfg.timeout_ms,
                    if e.is_timeout() { "超时" } else { "无法连接审查服务或发送请求" }
                ));
            }
        };
        let status = resp.status();
        if status.as_u16() == 400 && with_response_format {
            continue;
        }
        if !status.is_success() {
            return Err(format!("审查服务返回 HTTP {}", status.as_u16()));
        }
        let body = read_response(resp).await?;
        let text = std::str::from_utf8(&body).map_err(|_| "审查响应编码无效".to_string())?;
        return parse_ok(text, started.elapsed().as_millis() as u64);
    }
    Err("审查服务未返回有效响应".into())
}

/// 先遮盖本次请求的已知凭据，再复用通用脱敏。
pub(crate) fn redact(guard: &crate::guard::Guard, key: &str, text: &str) -> String {
    guard.sanitize(&text.replace(key, "[REDACTED]"))
}

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

async fn read_response(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        return Err("审查响应超过 64 KiB".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取审查响应失败".to_string())? {
        append_response_chunk(&mut bytes, &chunk)?;
    }
    Ok(bytes)
}

fn append_response_chunk(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<(), String> {
    if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
        return Err("审查响应超过 64 KiB".into());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

/// 解析成功响应：取模型输出 + model + usage。
///
/// 输出字段回退链（推理模型兼容，实测 openbmb/minicpm5-2b 经 Ollama 时
/// `content` 在思维链结束前恒为空、正文在 `reasoning` 字段）：
/// `content` → `reasoning_content`（DeepSeek-R1 风格）→ `reasoning`（Ollama 风格）。
/// 审查解析器（verdict::parse_redacted）扫描全部 JSON 对象，思维链里的最终结论同样可解析。
fn parse_ok(text: &str, latency_ms: u64) -> Result<ChatOutcome, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|_| "响应不是合法 JSON".to_string())?;
    let choice = v
        .pointer("/choices/0/message")
        .ok_or_else(|| "响应缺少 choices[0].message".to_string())?;
    let content = ["content", "reasoning_content", "reasoning"]
        .iter()
        .find_map(|k| choice.get(*k).and_then(|c| c.as_str()).filter(|c| !c.trim().is_empty()))
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        return Err("模型输出为空（可能被 max_tokens 截断，可调大输出预算）".to_string());
    }
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = |pointer: &str| {
        v.pointer(pointer)
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32
    };
    Ok(ChatOutcome {
        content,
        model,
        latency_ms,
        tokens_in: tokens("/usage/prompt_tokens"),
        tokens_out: tokens("/usage/completion_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_limit_applies_to_accumulated_chunks() {
        let mut bytes = Vec::new();
        let chunk = vec![0; MAX_RESPONSE_BYTES / 2];
        append_response_chunk(&mut bytes, &chunk).unwrap();
        append_response_chunk(&mut bytes, &chunk).unwrap();
        assert!(append_response_chunk(&mut bytes, &[0]).is_err());
        assert_eq!(bytes.len(), MAX_RESPONSE_BYTES);
    }

    #[test]
    fn parse_ok_reads_content_model_usage() {
        let raw = r#"{
            "model": "deepseek-chat",
            "choices": [ { "message": { "role": "assistant", "content": "{\"verdict\":\"allow\"}" } } ],
            "usage": { "prompt_tokens": 300, "completion_tokens": 24 }
        }"#;
        let out = parse_ok(raw, 321).unwrap();
        assert_eq!(out.content, r#"{"verdict":"allow"}"#);
        assert_eq!(out.model, "deepseek-chat");
        assert_eq!(out.latency_ms, 321);
        assert_eq!(out.tokens_in, 300);
        assert_eq!(out.tokens_out, 24);
    }

    #[test]
    fn parse_ok_rejects_missing_content() {
        assert!(parse_ok(r#"{"choices":[]}"#, 1).is_err());
        assert!(parse_ok("not json", 1).is_err());
    }

    #[test]
    fn parse_ok_skips_blank_content() {
        let out = parse_ok(
            r#"{"choices":[{"message":{"content":" ","reasoning_content":"","reasoning":"结论"}}]}"#,
            1,
        ).unwrap();
        assert_eq!(out.content, "结论");
    }
}
