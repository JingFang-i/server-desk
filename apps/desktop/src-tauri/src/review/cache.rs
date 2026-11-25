//! 审查结论缓存：LRU + TTL。
//!
//! key 由 `tool | 命令 | 静态判定 | 提示词版本 | 模型` 决定——任何一项变化，
//! 旧结论一律不可复用（`set_config` 时还会整体清空）。只缓存成功结论，
//! 失败/降级不缓存：恢复后第一条命令就能重新拿到真实审查。

use crate::review::verdict::AiReview;
use crate::review::config::ReviewConfig;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// 缓存 key：sha256(各部分以 NUL 分隔)。
pub fn cache_key(tool: &str, command: &str, static_verdict: &str, cfg: &ReviewConfig) -> [u8; 32] {
    let mut h = Sha256::new();
    for part in [tool, command, static_verdict, cfg.prompt_version.as_str(), cfg.model.as_str()] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    h.finalize().into()
}

pub struct VerdictCache {
    entries: HashMap<[u8; 32], (Instant, AiReview)>,
    order: VecDeque<[u8; 32]>,
    cap: usize,
}

impl VerdictCache {
    pub fn new(cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// 命中返回结论（标记 cached=true）并刷新 LRU 顺序；过期即剔除。
    pub fn get(&mut self, key: &[u8; 32], ttl: Duration) -> Option<AiReview> {
        let state = self.entries.get(key).map(|(at, _)| at.elapsed() <= ttl);
        match state {
            None => None,
            Some(false) => {
                self.entries.remove(key);
                self.order.retain(|k| k != key);
                None
            }
            Some(true) => {
                let mut v = self.entries.get(key).map(|(_, v)| v.clone())?;
                self.touch(key);
                v.cached = true;
                Some(v)
            }
        }
    }

    pub fn put(&mut self, key: [u8; 32], value: AiReview) {
        self.entries.insert(key, (Instant::now(), value));
        self.touch(&key);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// 仅测试观测用（R8.5 可用于缓存命中率统计）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn touch(&mut self, key: &[u8; 32]) {
        self.order.retain(|k| k != key);
        self.order.push_back(*key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::verdict::{AiReview, AiVerdict, Risk};

    fn entry(verdict: AiVerdict) -> AiReview {
        AiReview {
            verdict,
            risk: Risk::Medium,
            reason: "r".into(),
            cached: false,
            latency_ms: 0,
            model: "m".into(),
        }
    }

    fn cfg() -> ReviewConfig {
        ReviewConfig::default()
    }

    #[test]
    fn hit_marks_cached_and_refreshes() {
        let mut c = VerdictCache::new(8);
        let k = cache_key("exec_command", "uptime", "allow", &cfg());
        c.put(k, entry(AiVerdict::Allow));
        let hit = c.get(&k, Duration::from_secs(1800)).unwrap();
        assert!(hit.cached);
        assert_eq!(hit.verdict, AiVerdict::Allow);
    }

    #[test]
    fn expires_after_ttl() {
        let mut c = VerdictCache::new(8);
        let k = cache_key("exec_command", "uptime", "allow", &cfg());
        c.put(k, entry(AiVerdict::Allow));
        assert!(c.get(&k, Duration::from_secs(0)).is_none(), "TTL=0 立即过期");
        assert!(c.is_empty(), "过期项应被剔除");
    }

    #[test]
    fn key_changes_with_every_dimension() {
        let c0 = cfg();
        let k1 = cache_key("exec_command", "uptime", "allow", &c0);
        let k2 = cache_key("exec_batch", "uptime", "allow", &c0);
        let k3 = cache_key("exec_command", "df -h", "allow", &c0);
        let k4 = cache_key("exec_command", "uptime", "approval", &c0);
        let mut c1 = c0.clone();
        c1.model = "other".into();
        let k5 = cache_key("exec_command", "uptime", "allow", &c1);
        let mut c2 = c0.clone();
        c2.prompt_version = "v2".into();
        let k6 = cache_key("exec_command", "uptime", "allow", &c2);
        let all = [k1, k2, k3, k4, k5, k6];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "维度 {i}/{j} 未参与 key");
            }
        }
    }

    #[test]
    fn lru_evicts_oldest_beyond_capacity() {
        let mut c = VerdictCache::new(2);
        let cfg0 = cfg();
        let k1 = cache_key("t", "a", "allow", &cfg0);
        let k2 = cache_key("t", "b", "allow", &cfg0);
        let k3 = cache_key("t", "c", "allow", &cfg0);
        c.put(k1, entry(AiVerdict::Allow));
        c.put(k2, entry(AiVerdict::Review));
        assert!(c.get(&k1, Duration::from_secs(60)).is_some(), "刷新 k1 为最新");
        c.put(k3, entry(AiVerdict::Deny));
        assert_eq!(c.len(), 2);
        assert!(c.get(&k2, Duration::from_secs(60)).is_none(), "最久未用的 k2 被淘汰");
        assert!(c.get(&k1, Duration::from_secs(60)).is_some());
        assert!(c.get(&k3, Duration::from_secs(60)).is_some());
    }

    #[test]
    fn clear_empties_everything() {
        let mut c = VerdictCache::new(8);
        let cfg0 = cfg();
        c.put(cache_key("t", "x", "allow", &cfg0), entry(AiVerdict::Allow));
        c.clear();
        assert!(c.is_empty());
    }
}
