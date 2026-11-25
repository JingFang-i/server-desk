//! AI 高危操作审批队列：MCP 提交 → 应用弹窗（前端轮询/事件）→ 用户批准或拒绝；
//! 超时（120s）或队列清理时丢弃发送端，等待方收到 Err 一律按拒绝处理（fail-closed）。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub const APPROVAL_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub command: String,
    /// AI 审查意见（R8.4）：审查层给出的理由/降级说明，审批卡直接展示。
    /// 静态规则触发且审查未介入时为 None——不占位、不误导。
    pub ai_note: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

struct Entry {
    pending: PendingApproval,
    tx: oneshot::Sender<bool>,
    deadline: Instant,
}

#[derive(Clone, Default)]
pub struct Approvals {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Approvals {
    /// 登记一条待审批操作；调用方持有 rx 等待决定。
    pub fn create(
        &self,
        server_id: &str,
        server_name: &str,
        command: &str,
        ai_note: Option<&str>,
        tx: oneshot::Sender<bool>,
    ) -> PendingApproval {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let pending = PendingApproval {
            id: id.clone(),
            server_id: server_id.to_string(),
            server_name: server_name.to_string(),
            command: command.to_string(),
            ai_note: ai_note.map(|s| s.to_string()),
            created_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(APPROVAL_TTL.as_secs() as i64)).to_rfc3339(),
        };
        let entry = Entry {
            pending: pending.clone(),
            tx,
            deadline: Instant::now() + APPROVAL_TTL,
        };
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).insert(id, entry);
        pending
    }

    /// 审批可能已被处理，只有有效待审批项才更新意见。
    pub fn set_ai_note(&self, id: &str, note: String) -> bool {
        let mut entries = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = entries.get_mut(id).filter(|e| e.deadline > Instant::now()) else {
            return false;
        };
        entry.pending.ai_note = Some(note);
        true
    }

    /// 用户决定；返回是否成功送达（不存在/已过期 → false）。
    pub fn decide(&self, id: &str, approve: bool) -> bool {
        match self.inner.lock().unwrap_or_else(|p| p.into_inner()).remove(id) {
            Some(entry) if entry.deadline > Instant::now() => entry.tx.send(approve).is_ok(),
            _ => false,
        }
    }

    /// 当前待审批列表（惰性清理过期项：丢弃 tx → 等待方按拒绝处理）。
    pub fn pending(&self) -> Vec<PendingApproval> {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| e.deadline <= Instant::now())
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            if let Some(e) = map.remove(&k) {
                let _ = e.tx.send(false);
            }
        }
        let mut list: Vec<PendingApproval> =
            map.values().map(|e| e.pending.clone()).collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }
}

#[cfg(test)]
#[test]
fn review_note_updates_pending_entry_only() {
    let approvals = Approvals::default();
    let (tx, mut rx) = oneshot::channel();
    let pending = approvals.create("server", "服务器", "uptime", None, tx);
    assert!(approvals.pending()[0].ai_note.is_none());
    assert!(approvals.set_ai_note(&pending.id, "建议人工确认".into()));
    assert_eq!(approvals.pending()[0].ai_note.as_deref(), Some("建议人工确认"));
    assert!(approvals.decide(&pending.id, true));
    assert_eq!(rx.try_recv(), Ok(true));
    assert!(!approvals.set_ai_note(&pending.id, "结果晚于审批".into()));
}
