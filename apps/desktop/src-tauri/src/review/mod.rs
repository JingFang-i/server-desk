//! AI 智能审查（R8）模块入口。
//!
//! 分期落地（见 docs/r8-ai-review-design.md）：
//! - R8.1：配置/Key 存储、Reviewer 运行时、设置用 IPC 命令；
//! - R8.2（本次）：`verdict::merge` 合并判定 + `ops.rs` 两条 AI 路径接入 +
//!   缓存（LRU+TTL）+ 失败策略 + 熔断 + 独立限流 + 审计；
//! - R8.3：审查 prompt few-shot 与解析增强（`prompt.rs` 已就位）；
//! - R8.5：`ai_review_log` 审查日志（`ReviewEntry` 契约已定义）。
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
use crate::store::Store;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// 审查调用独立限流（次/分钟）：与工具调用限流分开，保护审查 API 成本。
/// 超限时跳过审查、维持静态判定（审计记 skipped:rate_limited）。
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
    cfg: RwLock<ReviewConfig>,
    http: reqwest::Client,
    cache: Mutex<VerdictCache>,
    breaker: Mutex<Breaker>,
    sem: Arc<Semaphore>,
}

impl Reviewer {
    /// 启动时从 meta 加载；未配置 = 审查关闭（行为与 R7 完全一致）。
    pub fn load(store: &Store) -> Self {
        let cfg = load_config(store);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1500))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            cfg: RwLock::new(cfg),
            http,
            cache: Mutex::new(VerdictCache::new(CACHE_CAP)),
            breaker: Mutex::new(Breaker::default()),
            sem: Arc::new(Semaphore::new(REVIEW_CONCURRENCY)),
        }
    }

    pub fn config(&self) -> ReviewConfig {
        self.cfg.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// 保存并热生效（无需重启）。模型/策略/超时变更后旧结论不可复用：
    /// 同步清空缓存并复位熔断。
    pub fn set_config(&self, store: &Store, cfg: ReviewConfig) -> Result<(), String> {
        save_config(store, &cfg)?;
        *self.cfg.write().unwrap_or_else(|p| p.into_inner()) = cfg;
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
        *self.breaker.lock().unwrap_or_else(|p| p.into_inner()) = Breaker::default();
        Ok(())
    }

    pub fn info(&self, store: &Store, vault: &crate::vault::Vault) -> ReviewInfo {
        ReviewInfo::of(store, vault, &self.config())
    }

    /// 复用的 HTTP 连接池（连接超时 1.5s；总超时按每次请求的配置）。
    pub fn http(&self) -> &reqwest::Client {
        &self.http
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

/// AI 审查步骤：输入静态判定，输出最终判定（只会更严，见 verdict::merge）。
///
/// 流程：关闭/Blocked 短路 → 独立限流 → 缓存 → 熔断 → Key → 并发闸门 →
/// 调模型（命令先过 `guard.sanitize`）→ 解析 → 审计 → 合并。
/// 任何失败都按 `failure_policy`（D2 默认 Escalate=转人工审批）兜底并写审计。
pub(crate) async fn review_and_merge(
    state: &crate::state::AppState,
    tool: &str,
    command: &str,
    static_decision: Decision,
    ctx: &ReviewContext<'_>,
    server_id: Option<&str>,
) -> Decision {
    let cfg = state.reviewer.config();

    // 1) 静态硬拒：不送审（模型无权否决硬规则），审计一条 skip 便于追溯
    if matches!(static_decision, Decision::Blocked(_)) {
        audit_review(state, server_id, tool, command, "ok:ai_review:skipped:blocked_by_static");
        return static_decision;
    }
    // 2) 审查关闭：零开销直通（行为与 R7 完全一致）
    if !cfg.enabled {
        return static_decision;
    }
    let static_str = if matches!(static_decision, Decision::NeedsApproval(_)) {
        "approval"
    } else {
        "allow"
    };

    // 3) 独立限流：超限跳过审查、维持静态判定（静态规则仍在保护）
    if state.review_rate.check(REVIEW_RATE_PER_MIN).is_err() {
        audit_review(state, server_id, tool, command, "ok:ai_review:skipped:rate_limited");
        return static_decision;
    }

    // 4) 缓存
    let key = cache_key(tool, command, static_str, &cfg);
    let ttl = Duration::from_secs(cfg.cache_ttl_secs);
    if ttl > Duration::ZERO {
        if let Some(hit) = state.reviewer.cache_get(&key, ttl) {
            audit_review(
                state,
                server_id,
                tool,
                command,
                &format!("ok:ai_review:cached:{}", hit.verdict.as_str()),
            );
            return verdict::merge(&static_decision, &Ok(hit), &cfg);
        }
    }

    // 5) 熔断：冷却期内不发起 HTTP，直接走失败策略
    if state.reviewer.breaker_open() {
        audit_review(state, server_id, tool, command, "ai_review:degraded:breaker");
        return verdict::merge(
            &static_decision,
            &Err("审查熔断中：上游连续失败，冷却期内暂不调用模型".to_string()),
            &cfg,
        );
    }

    // 6) Key（保险库锁定 → 与工具自身失败路径一致；未配置 → 降级）
    let api_key = match config::load_api_key(&state.store, &state.vault) {
        Ok(k) => k,
        Err(e) => {
            let msg = format!("读取审查 API Key 失败：{e}");
            audit_review(state, server_id, tool, command, "ai_review:degraded:key_error");
            return verdict::merge(&static_decision, &Err(msg), &cfg);
        }
    };
    let Some(api_key) = api_key.filter(|k| !k.trim().is_empty()) else {
        audit_review(state, server_id, tool, command, "ai_review:degraded:no_key");
        return verdict::merge(
            &static_decision,
            &Err("未配置审查 API Key".to_string()),
            &cfg,
        );
    };

    // 7) 送审（强制脱敏 + 限长 + 定界符）→ 调模型 → 解析
    let payload = prompt::build_payload(&cfg, tool, &state.guard.sanitize(command), ctx, static_str);
    let system = prompt::system_prompt();
    let _permit = state.reviewer.permit().await;
    let res = client::chat_json(state.reviewer.http(), &cfg, Some(&api_key), &system, &payload).await;
    drop(_permit);

    let ai: Result<verdict::AiReview, String> = match res {
        Ok(out) => match verdict::parse(&out.content) {
            Ok(mut v) => {
                v.latency_ms = out.latency_ms;
                v.model = out.model;
                state.reviewer.breaker_success();
                if ttl > Duration::ZERO {
                    state.reviewer.cache_put(key, v.clone());
                }
                audit_review(
                    state,
                    server_id,
                    tool,
                    command,
                    &format!("ok:ai_review:{}", v.verdict.as_str()),
                );
                Ok(v)
            }
            Err(e) => {
                state.reviewer.breaker_fail();
                audit_review(state, server_id, tool, command, "ai_review:degraded:parse");
                Err(e)
            }
        },
        Err(e) => {
            state.reviewer.breaker_fail();
            audit_review(state, server_id, tool, command, "ai_review:degraded:http");
            Err(crate::text::summary(&e, 120))
        }
    };
    verdict::merge(&static_decision, &ai, &cfg)
}

/// 审查步骤的独立审计行：action=ai_review，detail=脱敏摘要，
/// outcome 约定：`ok:ai_review:*` 为正常路径；`ai_review:degraded:*` 为降级
/// （审计页 ok 标志只认 ok 前缀，降级会显红——这是有意的：值得被看见）。
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

/// 审查日志条目。R8.5 由 `ai_review_log` 表填充；契约先行供前端联调。
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
    pub model: String,
    pub latency_ms: i64,
    pub cached: bool,
    pub error: String,
}
