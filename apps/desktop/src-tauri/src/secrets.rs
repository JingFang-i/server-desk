//! 敏感数据映射库（R9）：句柄间接引用（Secret Indirection）。
//!
//! 三条铁律（设计文档 docs/r9-secret-ref-design.md §2）：
//! 1. 明文永不进入 AI 可见面——命令文本 / 返回输出 / 审计 / 日志只出现句柄；
//! 2. 句柄即权限——TTL 过期即失效、绑定单台服务器、撤销立即生效；
//! 3. 明文副本只落服务器 tmpfs（/run/serverdesk-secrets/，0600，重启即失），
//!    权威映射（密文）在桌面端 vault DEK 加密落库。
//!
//! 解引用方式（D6 已拍板）：**环境变量注入**。AI 命令中写 `$SD_SEC_xxxx`，
//! 执行时由 ServerDesk 生成 `export SD_SEC_xxxx="$(cat '<远端路径>')" && …` 前缀——
//! 明文走环境变量（`ps` 不可见），不进命令行参数、不进 AI 视野。
//! stdin 模式（暴露面更小但要求程序支持）留作二期增强。

use crate::crypto::{self};
use crate::state::AppState;
use crate::store::SecretRefRow;
use rand::RngCore;
use std::time::Duration;
use zeroize::Zeroizing;

pub const META_ENABLED: &str = "secret_refs_enabled";
pub const META_TTL_MINUTES: &str = "secret_ttl_minutes";
/// 服务器侧明文副本目录：tmpfs（/run），重启即失。
pub const RUN_DIR: &str = "/run/serverdesk-secrets";
pub const HANDLE_PREFIX: &str = "SD_SEC_";

/// 是否启用句柄注入（D4：默认关，显式开启才生效）。
pub fn enabled(store: &crate::store::Store) -> bool {
    store
        .get_meta(META_ENABLED)
        .ok()
        .flatten()
        .map(|s| s == "1")
        .unwrap_or(false)
}

/// TTL（分钟），D5 默认 15；钳制 1–1440。
pub fn ttl_minutes(store: &crate::store::Store) -> u64 {
    store
        .get_meta(META_TTL_MINUTES)
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
        .clamp(1, 1440)
}

pub fn set_config(store: &crate::store::Store, enabled: bool, ttl_minutes: u64) -> Result<(), String> {
    store.set_meta(META_ENABLED, if enabled { "1" } else { "0" })?;
    store.set_meta(META_TTL_MINUTES, &ttl_minutes.clamp(1, 1440).to_string())
}

/// 生成句柄：SD_SEC_ + 8 位随机 hex（受控字符集，注入路径单引号安全）。
fn handle_new() -> String {
    let mut b = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!(
        "{}{}",
        HANDLE_PREFIX,
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// 远端明文文件路径。
pub fn remote_path(handle: &str) -> String {
    format!("{RUN_DIR}/{handle}")
}

/// 授予：登记映射 + 把明文经 SFTP 写到服务器 tmpfs（0600）。
/// 明文只走 SFTP 数据流——不进命令文本、不进审计、不进日志。
pub async fn grant(
    state: &AppState,
    server_cfg: &crate::ssh::ConnParams,
    server_id: &str,
    source: &str,
    value: &str,
) -> Result<String, String> {
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let ttl = ttl_minutes(&state.store);
    let now = chrono::Utc::now();
    let expires = (now + chrono::Duration::minutes(ttl as i64)).to_rfc3339();

    // 句柄唯一性：PK 冲突则重试（概率极低）
    let mut handle = handle_new();
    for _ in 0..3 {
        if state.store.get_secret_ref(&handle)?.is_none() {
            break;
        }
        handle = handle_new();
    }

    // 1) 服务器侧目录（权限 700，无明文参与）
    state
        .ssh
        .exec(
            &state.store,
            server_id,
            server_cfg,
            &format!("install -d -m 700 {RUN_DIR}"),
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| format!("创建远端秘密目录失败: {e}"))?;

    // 2) 明文经 SFTP 写入（不进命令文本）+ 显式 0600
    state
        .ssh
        .sftp_write_file(&state.store, server_id, server_cfg, &remote_path(&handle), value.as_bytes())
        .await
        .map_err(|e| format!("写入远端秘密文件失败: {e}"))?;
    state
        .ssh
        .exec(
            &state.store,
            server_id,
            server_cfg,
            &format!("chmod 600 {}", remote_path(&handle)),
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| format!("收紧远端秘密文件权限失败: {e}"))?;

    // 3) 桌面端权威映射（值用 DEK 加密落库）
    let blob = crypto::encrypt(&dek, value.trim().as_bytes())?;
    state.store.insert_secret_ref(&SecretRefRow {
        id: handle.clone(),
        server_id: server_id.to_string(),
        source: crate::text::summary(source, 200),
        value_blob: blob,
        created_at: now.to_rfc3339(),
        expires_at: expires,
    })?;

    crate::ops::audit_named(
        state,
        "user",
        Some(server_id),
        server_id,
        "secret_ref",
        &format!("{handle}（TTL {ttl} 分钟）来源: {}", crate::text::summary(source, 120)),
        "ok:secret_ref:granted",
    );
    Ok(handle)
}

/// 撤销：删远端文件（能连上就删，失败不阻断——tmpfs 重启即失兜底）+ 删本地映射。
pub async fn revoke(state: &AppState, handle: &str) -> Result<bool, String> {
    let Some((meta, blob)) = state.store.get_secret_ref(handle)? else {
        return Ok(false);
    };
    // 清点内存里的密文副本后丢弃
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    drop(Zeroizing::new(crypto::decrypt(&dek, &blob).unwrap_or_default()));

    if let Ok(cfg) = crate::ops::load_server_config(state, &meta.server_id) {
        let _ = state
            .ssh
            .exec(
                &state.store,
                &meta.server_id,
                &crate::ops::conn_params(&cfg),
                &format!("rm -f {}", remote_path(handle)),
                Duration::from_secs(10),
            )
            .await;
    }
    let existed = state.store.delete_secret_ref(handle)?;
    crate::ops::audit_named(
        state,
        "user",
        Some(&meta.server_id),
        &meta.server_id,
        "secret_ref",
        handle,
        if existed { "ok:secret_ref:revoked" } else { "ok:secret_ref:not_found" },
    );
    Ok(existed)
}

/// 过期清扫：删本地映射（远端文件靠 tmpfs 生命周期 + TTL 短窗兜底），逐条审计。
pub async fn sweep_expired(state: &AppState) {
    let now = chrono::Utc::now().to_rfc3339();
    let Ok(ids) = state.store.delete_expired_secret_refs(&now) else {
        return;
    };
    for id in ids {
        crate::ops::audit(
            state,
            "system",
            None,
            "secret_ref",
            &id,
            "ok:secret_ref:expired",
        );
    }
}

/// 扫描命令中的句柄引用（$SD_SEC_xxxxxxxx），去重保序。
pub fn scan_handles(command: &str) -> Vec<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\$(SD_SEC_[0-9a-f]{8})").expect("handle regex")
    });
    let mut out: Vec<String> = Vec::new();
    for cap in re.captures_iter(command) {
        let h = cap[1].to_string();
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// 构造注入前缀：为每个句柄生成 `export H="$(cat '路径')" && `。
/// 纯函数（单测对象）；句柄字符集受控，单引号安全。
pub fn build_injection(handles: &[String], command: &str) -> String {
    let mut prefix = String::new();
    for h in handles {
        prefix.push_str(&format!(
            "export {h}=\"$(cat '{}')\" && ",
            remote_path(h)
        ));
    }
    format!("{prefix}{command}")
}

/// 解引用入口：命令含句柄时校验（启用/存在/绑定服务器/未过期）并返回
/// 注入后的可执行命令；不含句柄返回 None（零行为变化）。
///
/// 校验**不需要解密值**——解引用发生在服务器侧（远端 cat 临时文件），
/// 桌面端只校验元数据；顺带做一次解密完整性检查以尽早发现密文损坏。
pub fn resolve_for_command(
    state: &AppState,
    server_id: &str,
    command: &str,
) -> Result<Option<String>, String> {
    if !enabled(&state.store) {
        return Ok(None);
    }
    let handles = scan_handles(command);
    if handles.is_empty() {
        return Ok(None);
    }
    let dek = state.vault.dek().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    for h in &handles {
        let Some((meta, blob)) = state.store.get_secret_ref(h)? else {
            return Err(format!("敏感引用 {h} 不存在或已被撤销，请重新登记"));
        };
        if meta.server_id != server_id {
            return Err(format!("敏感引用 {h} 绑定的是其他服务器，不能在本服务器使用"));
        }
        if meta.expires_at.as_str() <= now.as_str() {
            return Err(format!("敏感引用 {h} 已过期（TTL），请重新登记"));
        }
        // 密文完整性检查（明文用完即弃，Zeroizing 清内存）
        let plain = crypto::decrypt(&dek, &blob).map_err(|_| format!("敏感引用 {h} 数据损坏，请重新登记"))?;
        drop(Zeroizing::new(plain));
    }
    Ok(Some(build_injection(&handles, command)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_handles_dedup_and_ignores_bare_names() {
        let cmd = "curl -H \"B: $SD_SEC_a7b3c9d2\" && echo $SD_SEC_a7b3c9d2 $SD_SEC_00112233 SD_SEC_zzzz";
        assert_eq!(
            scan_handles(cmd),
            vec!["SD_SEC_a7b3c9d2".to_string(), "SD_SEC_00112233".to_string()]
        );
        // 裸 handle（无 $）不是引用——注入只认环境变量形态
        assert!(scan_handles("echo SD_SEC_a7b3c9d2").is_empty());
    }

    #[test]
    fn injection_prefixes_exports_and_keeps_command() {
        let out = build_injection(&["SD_SEC_a7b3c9d2".into(), "SD_SEC_00112233".into()], "curl -s http://x");
        assert_eq!(
            out,
            "export SD_SEC_a7b3c9d2=\"$(cat '/run/serverdesk-secrets/SD_SEC_a7b3c9d2')\" && \
             export SD_SEC_00112233=\"$(cat '/run/serverdesk-secrets/SD_SEC_00112233')\" && curl -s http://x"
        );
        assert_eq!(build_injection(&[], "uptime"), "uptime");
    }

    #[test]
    fn remote_path_is_under_run_dir() {
        assert_eq!(remote_path("SD_SEC_x"), "/run/serverdesk-secrets/SD_SEC_x");
    }
}
