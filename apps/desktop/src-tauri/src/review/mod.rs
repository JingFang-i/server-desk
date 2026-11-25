//! AI 智能审查（R8）模块入口。
//!
//! 分期落地（见 docs/r8-ai-review-design.md）：
//! - R8.1：配置/Key 存储、Reviewer 运行时、设置用 IPC 命令；
//! - R8.2：`verdict::merge` 合并判定 + `ops.rs` 管线接入 + 缓存/熔断/限流；
//! - R8.3：prompt v2（few-shot + categories）；
//! - R8.4：AI 意见上审批卡（ReviewOutcome.ai_note）；
//! - R8.5（本次）：`ai_review_log` 审查日志——审计之外再落一行结构化记录，
//!   供设置页「最近审查记录」与后续命中率/降级率统计。
//!
//! 安全不变式（方案 §1，verdict.rs 的穷举测试守护）：
//! 最终判定 = max(静态规则, AI 判定)，序关系 Allow < NeedsApproval < Blocked；
//! 静态 Blocked 的命令**不送审**——模型没有任何机会否决硬规则。

pub mod cache;
pub mod client;
pub mod config;
pub mod prompt;
pub mod verdict;

use crate::guard::Decision;
use crate::review::cache::{cache_key, VerdictCache};
use crate::review::config::{load_config, save_config};
pub use crate::review::config::{ReviewConfig, ReviewInfo};
use crate::store::{ReviewLogRow, Store};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// 审查调用独立限流（次/分钟）：与工具调用限流分开，保护审查 API 成本。
/// 超限时应用配置的失败策略，并标记降级审计。
const REVIEW_RATE_PER_MIN: u32 = 60;
/// 熔断：连续失败 N 次后冷却（已拍板 D2 的一部分）。
const BREAKER_THRESHOLD: u32 = 3;
const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);
/// 结论缓存容量（LRU）。
const CACHE_CAP: usize = 512;
/// 审查调用并发上限（批量 20 台时不打爆供应商）。
const REVIEW_CONCURRENCY: usize = 4;

/// 送审上下文。是否真的送出由 `cfg.send_server_context` 决定（D3）。
pub struct ReviewContext<'a> {
    pub group: Option<&'a str>,
    pub server_name: Option<&'a str>,
    /// 批量执行的目标台数（exec_batch 用）
    pub batch_size: Option<usize>,
}

/// 熔断器：连续失败计数 + 冷却截止。冷却期内不发起 HTTP（走失败策略）。
#[derive(Debug, Default)]
struct Breaker {
    fails: u32,
    open_until: Option<Instant>,
}

impl Breaker {
    fn is_open(&self) -> bool {
        self.open_until.is_some_and(|t| Instant::now() < t)
    }

    fn on_fail(&mut self) {
        self.fails = self.fails.saturating_add(1);
        if self.fails >= BREAKER_THRESHOLD {
            self.open_until = Some(Instant::now() + BREAKER_COOLDOWN);
        }
    }

    fn on_success(&mut self) {
        self.fails = 0;
        self.open_until = None;
    }
}

/// 审查器运行时：配置热更新 + 复用 HTTP 连接池 + 缓存 + 熔断 + 并发闸门。
/// API Key 不在此常驻——每次使用时经 `vault.dek()` 现解（见 config.rs 模块注释）。
pub struct Reviewer {
    cfg: RwLock<(ReviewConfig, u64)>,
    http: Option<reqwest::Client>,
    local_http: Option<reqwest::Client>,
    cache: Mutex<VerdictCache>,
    breaker: Mutex<Breaker>,
    sem: Arc<Semaphore>,
}

impl Reviewer {
    /// 启动时从 meta 加载；未配置 = 审查关闭（行为与 R7 完全一致）。
    pub fn load(store: &Store) -> Self {
        let cfg = load_config(store);
        let builder = || reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1500))
            .redirect(reqwest::redirect::Policy::none());
        Self {
            cfg: RwLock::new((cfg, 0)),
            http: builder().build().ok(),
            local_http: builder().no_proxy().build().ok(),
            cache: Mutex::new(VerdictCache::new(CACHE_CAP)),
            breaker: Mutex::new(Breaker::default()),
            sem: Arc::new(Semaphore::new(REVIEW_CONCURRENCY)),
        }
    }

    pub fn config(&self) -> ReviewConfig {
        self.cfg.read().unwrap_or_else(|p| p.into_inner()).0.clone()
    }

    /// 配置和凭据来自同一次读取，防止热更新时混用不同供应商的设置。
    pub fn request_settings(
        &self,
        store: &Store,
        vault: &crate::vault::Vault,
    ) -> (ReviewConfig, u64, Result<Option<zeroize::Zeroizing<String>>, String>) {
        let settings = self.cfg.read().unwrap_or_else(|p| p.into_inner());
        let key = if settings.0.enabled { config::load_api_key(store, vault) } else { Ok(None) };
        (settings.0.clone(), settings.1, key)
    }

    pub fn set_config(
        &self,
        store: &Store,
        vault: &crate::vault::Vault,
        cfg: ReviewConfig,
        key: Option<&str>,
    ) -> Result<(), String> {
        let mut settings = self.cfg.write().unwrap_or_else(|p| p.into_inner());
        let previous_endpoint = settings.0.endpoint().ok();
        let same_endpoint = settings.0.base_url == cfg.base_url
            || (previous_endpoint.is_some() && previous_endpoint == cfg.endpoint().ok());
        let key_blob = match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(key) => {
                cfg.endpoint()?;
                let dek = vault.dek().map_err(|e| e.to_string())?;
                Some(config::encode_key_blob(&dek, key)?)
            }
            None if !same_endpoint => Some(String::new()),
            None => None,
        };
        save_config(store, &cfg, key_blob.as_deref())?;
        self.replace_settings(&mut settings, cfg);
        Ok(())
    }

    pub fn reset(&self, store: &Store, vault: &crate::vault::Vault) -> Result<(), String> {
        let mut settings = self.cfg.write().unwrap_or_else(|p| p.into_inner());
        store.wipe_all()?;
        self.replace_settings(&mut settings, ReviewConfig::default());
        vault.reset().map_err(|e| e.to_string())
    }

    fn replace_settings(&self, settings: &mut (ReviewConfig, u64), cfg: ReviewConfig) {
        settings.0 = cfg;
        settings.1 += 1;
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
        *self.breaker.lock().unwrap_or_else(|p| p.into_inner()) = Breaker::default();
    }

    pub fn info(&self, store: &Store, vault: &crate::vault::Vault) -> ReviewInfo {
        let settings = self.cfg.read().unwrap_or_else(|p| p.into_inner());
        ReviewInfo::of(store, vault, &settings.0)
    }

    /// 复用的 HTTP 连接池（连接超时 1.5s；总超时按每次请求的配置）。
    pub fn http(&self, cfg: &ReviewConfig) -> Result<&reqwest::Client, String> {
        let url = cfg.endpoint()?;
        let client = if config::is_loopback(&url) { &self.local_http } else { &self.http };
        client.as_ref().ok_or_else(|| "无法初始化审查 HTTP 客户端".to_string())
    }

    async fn permit(&self) -> tokio::sync::SemaphorePermit<'_> {
        // 信号量从不 close，acquire 只会在应用关闭时失败
        self.sem
            .acquire()
            .await
            .expect("审查并发信号量被意外关闭")
    }

    fn cache_get(&self, key: &[u8; 32], ttl: Duration) -> Option<verdict::AiReview> {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key, ttl)
    }

    fn cache_put(&self, key: [u8; 32], value: verdict::AiReview) {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .put(key, value);
    }

    fn breaker_open(&self) -> bool {
        self.breaker.lock().unwrap_or_else(|p| p.into_inner()).is_open()
    }

    fn breaker_fail(&self) {
        self.breaker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .on_fail();
    }

    fn breaker_success(&self) {
        self.breaker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .on_success();
    }
}

#[cfg(test)]
#[test]
fn endpoint_change_clears_legacy_key_without_replacement() {
    let path = std::path::Path::new(":memory:");
    let store = Store::open(path).unwrap();
    let vault = crate::vault::Vault::open(std::path::Path::new("."), path).unwrap();
    let mut cfg = ReviewConfig {
        base_url: "https://first.example.invalid/v1".into(),
        ..ReviewConfig::default()
    };
    config::save_config(&store, &cfg, Some("fixture-blob")).unwrap();
    let reviewer = Reviewer::load(&store);
    reviewer.set_config(&store, &vault, cfg.clone(), None).unwrap();
    assert!(config::has_key(&store));
    cfg.base_url = "https://second.example.invalid/v1".into();
    reviewer.set_config(&store, &vault, cfg.clone(), None).unwrap();
    assert!(!config::has_key(&store));
    assert_eq!(config::load_config(&store), cfg);
}

/// 审查步骤的结构化输出：合并后的最终判定 + 给审批卡展示的 AI 意见。
pub struct ReviewOutcome {
    pub decision: Decision,
    /// None = 审查未介入（关闭或静态硬拒跳过）；Some = 有意见可展示
    pub ai_note: Option<crate::approvals::ApprovalReview>,
}

impl ReviewOutcome {
    fn plain(decision: Decision) -> Self {
        Self { decision, ai_note: None }
    }
}

/// AI 审查步骤：输入静态判定，输出合并结果（只会更严，见 verdict::merge）。
///
/// 流程：关闭/Blocked 短路 → 缓存 → 熔断 → Key → 并发闸门 → 独立限流 →
/// 调模型（命令先过 `guard.sanitize`）→ 解析 → 记录（审计+日志）→ 合并。
/// 任何失败都按 `failure_policy`（D2 默认 Escalate=转人工审批）兜底并写审计。
pub(crate) async fn review_and_merge(
    state: &crate::state::AppState,
    tool: &str,
    command: &str,
    static_decision: Decision,
    ctx: &ReviewContext<'_>,
    server_id: Option<&str>,
) -> ReviewOutcome {
    let (cfg, generation, api_key) = state.reviewer.request_settings(&state.store, &state.vault);
    if !cfg.enabled {
        return ReviewOutcome::plain(static_decision);
    }
    let redact = |text: &str| match &api_key {
        Ok(Some(key)) if !key.is_empty() => client::redact(&state.guard, key, text),
        _ => state.guard.sanitize(text),
    };
    let safe_command = redact(command);
    let safe_group = ctx.group.map(redact);
    let safe_name = ctx.server_name.map(redact);
    let safe_ctx = ReviewContext {
        group: safe_group.as_deref(),
        server_name: safe_name.as_deref(),
        batch_size: ctx.batch_size,
    };
    let static_str = decision_str(&static_decision);
    let finish = |ai: Result<verdict::AiReview, String>, tag: &str| {
        // 配置更新或重置后，旧任务不能回填审查日志、缓存或熔断状态。
        let settings = state.reviewer.cfg.read().unwrap_or_else(|p| p.into_inner());
        if settings.1 != generation {
            return ReviewOutcome::plain(Decision::Blocked("审查配置已变化，请重新提交".into()));
        }
        let decision = verdict::merge(&static_decision, &ai, &cfg);
        let (label, cached) = match &ai {
            Ok(review) => (review.verdict.as_str(), review.cached),
            Err(_) if tag == "blocked_by_static" => ("skipped", false),
            Err(_) => ("error", false),
        };
        let mut outcome = record_review(
            state, server_id, tool, command, &safe_command, static_str, label, &decision,
            ai.as_ref().ok(), tag, cached, &cfg, &safe_ctx,
        );
        if tag != "blocked_by_static" {
            outcome.ai_note = Some(ai_note_of(&ai));
        }
        outcome
    };
    if matches!(static_decision, Decision::Blocked(_)) {
        return finish(Err("静态规则硬拒".into()), "blocked_by_static");
    }

    if safe_command != command || api_key.as_ref().ok().and_then(Option::as_ref)
        .is_some_and(|key| !key.is_empty() && cfg.model.contains(key.as_str()))
    {
        // 命中凭据规则的命令由本地人工判断，避免将部分脱敏的参数送出。
        let reason = "识别到可能含凭据的命令或审查配置，需人工确认";
        return finish(Ok(verdict::AiReview::manual_confirmation(reason)), "credential_input");
    }
    let payload = prompt::build_payload(&cfg, tool, &safe_command, &safe_ctx, static_str);
    let key = cache::request_key(tool, command, static_str, &cfg, &payload, generation);
    let ttl = Duration::from_secs(cfg.cache_ttl_secs);
    let api_key = match api_key {
        Ok(Some(key)) if !key.trim().is_empty() => key,
        Ok(_) => return finish(Err("未配置审查 API Key".into()), "no_key"),
        Err(e) => return finish(Err(format!("读取审查 API Key 失败：{e}")), "key_error"),
    };
    if ttl > Duration::ZERO {
        if let Some(hit) = state.reviewer.cache_get(&key, ttl) {
            return finish(Ok(hit), "");
        }
    }
    if state.reviewer.breaker_open() {
        return finish(Err("审查熔断中，暂不调用模型".into()), "breaker");
    }

    let deadline = Instant::now() + Duration::from_millis(u64::from(cfg.timeout_ms));
    let Ok(_permit) = tokio::time::timeout_at(deadline.into(), state.reviewer.permit()).await else {
        return finish(Err("等待审查并发额度超时".into()), "queue_timeout");
    };
    {
        let settings = state.reviewer.cfg.read().unwrap_or_else(|p| p.into_inner());
        if settings.1 != generation || !state.vault.is_unlocked() || !state.guard.ai_enabled() {
            return ReviewOutcome::plain(Decision::Blocked("审查期间配置或访问权限已变化，请重新提交".into()));
        }
    }
    if state.reviewer.breaker_open() {
        return finish(Err("审查熔断中，暂不调用模型".into()), "breaker");
    }
    if state.review_rate.check(REVIEW_RATE_PER_MIN).is_err() {
        return finish(Err("审查速率已达上限".into()), "rate_limited");
    }
    let response = match state.reviewer.http(&cfg) {
        Ok(http) => client::chat_json(
            http, &cfg, Some(&api_key), &prompt::system_prompt(), &payload, deadline,
        ).await,
        Err(e) => Err(e),
    };
    let (ai, tag) = match response {
        Ok(out) => match verdict::parse_redacted(&out.content, |text| {
            client::redact(&state.guard, &api_key, text)
        }) {
            Ok(mut review) => {
                review.latency_ms = out.latency_ms;
                review.model = client::redact(&state.guard, &api_key, &out.model);
                review.tokens_in = out.tokens_in;
                review.tokens_out = out.tokens_out;
                (Ok(review), "")
            }
            Err(e) => (Err(e), "parse"),
        },
        Err(e) => (Err(crate::text::summary(&e, 120)), "call_failed"),
    };
    {
        let settings = state.reviewer.cfg.read().unwrap_or_else(|p| p.into_inner());
        if settings.1 == generation {
            match &ai {
                Ok(review) => {
                    state.reviewer.breaker_success();
                    if ttl > Duration::ZERO {
                        state.reviewer.cache_put(key, review.clone());
                    }
                }
                Err(_) => state.reviewer.breaker_fail(),
            }
        }
    }
    finish(ai, tag)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 最终判定的字符串形态（日志用）：allow | approval | blocked。
fn decision_str(d: &Decision) -> &'static str {
    match d {
        Decision::Allow => "allow",
        Decision::NeedsApproval(_) => "approval",
        Decision::Blocked(_) => "blocked",
    }
}

/// 跳过/失败场景的人读文案（日志 reason 字段）。
fn skip_reason_text(tag: &str) -> String {
    match tag {
        "blocked_by_static" => "静态规则硬拒，未送审".to_string(),
        "credential_input" => "可能包含凭据，未送审".to_string(),
        "rate_limited" => "审查限流，已应用失败策略".to_string(),
        "queue_timeout" => "等待审查并发额度超时".to_string(),
        "breaker" => "熔断冷却中，未调用模型".to_string(),
        "key_error" => "读取审查 API Key 失败".to_string(),
        "no_key" => "未配置审查 API Key".to_string(),
        "parse" => "模型输出无法解析为结论".to_string(),
        "call_failed" => "模型调用失败（网络/超时/HTTP 错误）".to_string(),
        other => format!("跳过/失败：{other}"),
    }
}

/// 每次审查统一出口：写审计行 + 落 `ai_review_log` 结构化日志。
/// 审计 outcome 约定：`ok:ai_review:*` 正常；`ai_review:degraded:*` 降级显红。
#[allow(clippy::too_many_arguments)]
fn record_review(
    state: &crate::state::AppState,
    server_id: Option<&str>,
    tool: &str,
    command: &str,
    safe_command: &str,
    static_verdict: &str,
    ai_verdict: &str,
    final_decision: &Decision,
    ai: Option<&verdict::AiReview>,
    error_tag: &str,
    cached: bool,
    cfg: &ReviewConfig,
    ctx: &ReviewContext<'_>,
) -> ReviewOutcome {
    // 审计行
    let outcome = if error_tag == "credential_input" {
        "ok:ai_review:skipped:credential_input".to_string()
    } else if ai_verdict == "skipped" {
        format!("ok:ai_review:skipped:{error_tag}")
    } else if ai_verdict == "error" {
        format!("ai_review:degraded:{error_tag}")
    } else {
        format!(
            "ok:ai_review:{}{}",
            if cached { "cached:" } else { "" },
            ai_verdict
        )
    };
    audit_review(state, server_id, tool, safe_command, &outcome);

    // 结构化日志（正文只存脱敏摘要 + hash，不落原始命令全文）
    let (risk, reason, categories, model, latency, tokens_in, tokens_out) = match ai {
        Some(v) => (
            v.risk.as_str().to_string(),
            v.reason.clone(),
            v.categories.clone(),
            v.model.clone(),
            v.latency_ms as i64,
            if cached { 0 } else { v.tokens_in as i64 },
            if cached { 0 } else { v.tokens_out as i64 },
        ),
        None => (
            String::new(),
            skip_reason_text(error_tag),
            Vec::new(),
            String::new(),
            0,
            0,
            0,
        ),
    };
    let digest = crate::text::summary(safe_command, 200);
    let row = ReviewLogRow {
        ts: chrono::Utc::now().to_rfc3339(),
        session: crate::ops::current_session(),
        tool: tool.to_string(),
        server_id: server_id.map(|s| s.to_string()),
        server_name: crate::text::summary(&state.guard.sanitize(ctx.server_name.unwrap_or("")), crate::text::SERVER_NAME_MAX),
        command_hash: to_hex(&cache_key(tool, command, static_verdict, cfg)),
        command_digest: digest.clone(),
        static_verdict: static_verdict.to_string(),
        ai_verdict: ai_verdict.to_string(),
        final_verdict: decision_str(final_decision).to_string(),
        risk,
        reason: reason.clone(),
        categories,
        model,
        latency_ms: latency,
        tokens_in,
        tokens_out,
        cached,
        error: error_tag.to_string(),
    };
    if let Err(e) = state.store.insert_review_log(&row) {
        // 日志写失败不阻断判定链路，但必须留痕（对齐审计失败计数的精神）
        eprintln!("审查日志写入失败: {e}");
    }
    ReviewOutcome::plain(final_decision.clone())
}

/// 审计行：action=ai_review，detail=脱敏摘要。
fn audit_review(
    state: &crate::state::AppState,
    server_id: Option<&str>,
    tool: &str,
    command: &str,
    outcome: &str,
) {
    let digest = crate::text::summary(&state.guard.sanitize(command), 200);
    crate::ops::audit(
        state,
        "ai",
        server_id,
        shared::action::AI_REVIEW,
        &crate::text::truncate(&format!("{tool}: {digest}"), crate::text::AUDIT_DETAIL_MAX),
        outcome,
    );
}

/// 将结构化审查结论交给审批界面。
fn ai_note_of(ai: &Result<verdict::AiReview, String>) -> crate::approvals::ApprovalReview {
    match ai {
        Ok(review) => crate::approvals::ApprovalReview {
            verdict: review.verdict.as_str(),
            risk: Some(review.risk.as_str()),
            reason: review.reason.clone(),
            cached: review.cached,
        },
        Err(reason) => crate::approvals::ApprovalReview {
            verdict: "error",
            risk: None,
            reason: reason.clone(),
            cached: false,
        },
    }
}

/// 审查日志条目（`ai_review_log` 的查询形态）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEntry {
    pub id: i64,
    pub ts: String,
    pub tool: String,
    pub server_name: String,
    /// AI 原始判定：allow | review | deny | error | skipped
    pub ai_verdict: String,
    /// 合并后的最终判定：allow | approval | blocked
    pub final_verdict: String,
    pub risk: String,
    pub reason: String,
    pub categories: Vec<String>,
    pub model: String,
    pub latency_ms: i64,
    pub cached: bool,
    pub error: String,
}
