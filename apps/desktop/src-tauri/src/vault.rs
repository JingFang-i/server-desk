//! 主密码保险库。
//!
//! 安全模型：随机 32 字节 DEK 加密一切凭据；DEK 由「主密码 Argon2id 派生的 KEK」
//! AES-GCM 包裹后落盘；DEK 仅在解锁期间存在于内存（Zeroizing，释放即清零）。
//! 空闲自动锁；可选 OS Keychain（macOS Keychain / Windows DPAPI Credential Manager）
//! 镜像 DEK 以支持启动自动解锁（默认关闭）。

use crate::crypto::{self, KEY_LEN};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::{rngs::OsRng, RngCore};
use rusqlite::Connection;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;
use zeroize::{Zeroize, Zeroizing};

const KEYCHAIN_SERVICE: &str = "com.qsmx.serverdesk";
const KEYCHAIN_USER: &str = "vault-dek";
const DEFAULT_AUTOLOCK_MINUTES: u64 = 10;
const MIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub initialized: bool,
    pub unlocked: bool,
    pub auto_lock_minutes: u64,
    pub keychain_auto_unlock: bool,
}

#[derive(Debug)]
pub enum VaultError {
    NotInitialized,
    AlreadyInitialized,
    WeakPassword(String),
    WrongPassword,
    Locked,
    Keychain(String),
    Io(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotInitialized => write!(f, "保险库尚未初始化"),
            VaultError::AlreadyInitialized => write!(f, "保险库已初始化，不可重复设置"),
            VaultError::WeakPassword(m) => write!(f, "{m}"),
            VaultError::WrongPassword => write!(f, "主密码错误"),
            VaultError::Locked => write!(f, "保险库已锁定"),
            VaultError::Keychain(m) => write!(f, "系统钥匙串操作失败: {m}"),
            VaultError::Io(m) => write!(f, "{m}"),
        }
    }
}

/// 旧数据迁移结果（M1 过渡密钥 → 保险库 DEK）。
#[derive(Debug)]
pub struct MigrationOutcome {
    pub migrated: usize,
    pub failed: Vec<String>,
}

impl MigrationOutcome {
    pub fn summary(&self) -> Option<String> {
        if self.migrated == 0 && self.failed.is_empty() {
            return None;
        }
        if self.failed.is_empty() {
            Some(format!("已将 {0} 条 M1 旧凭据迁移进主密码保险库", self.migrated))
        } else {
            Some(format!(
                "迁移完成 {0} 条，失败 {1} 条（{2}），旧密钥文件已保留以便重试",
                self.migrated,
                self.failed.len(),
                self.failed.join(", ")
            ))
        }
    }
}

struct Inner {
    dek: Option<Zeroizing<[u8; KEY_LEN]>>,
    last_activity: Instant,
    salt_hex: String,
    wrapped_dek: Option<Vec<u8>>,
    auto_lock_minutes: u64, // 0 = 永不自动锁
    keychain_auto_unlock: bool,
}

pub struct Vault {
    dir: PathBuf,
    conn: Mutex<Connection>,
    inner: Mutex<Inner>,
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("hex 长度异常".into());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| "hex 解码失败".to_string()))
        .collect()
}

fn argon2_kek(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    use argon2::{Algorithm, Argon2, Params, Version};
    // OWASP 推荐：m=19MiB, t=2, p=1
    let params = Params::new(19_456, 2, 1, Some(KEY_LEN))
        .map_err(|e| VaultError::Io(format!("Argon2 参数非法: {e}")))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut kek = Zeroizing::new([0u8; KEY_LEN]);
    a2.hash_password_into(password.as_bytes(), salt, &mut kek[..])
        .map_err(|e| VaultError::Io(format!("Argon2 派生失败: {e}")))?;
    Ok(kek)
}

fn wrap_dek(kek: &[u8; KEY_LEN], dek: &[u8; KEY_LEN]) -> Result<Vec<u8>, VaultError> {
    crypto::encrypt(kek, dek).map_err(VaultError::Io)
}

fn unwrap_dek(kek: &[u8; KEY_LEN], blob: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let pt = crypto::decrypt(kek, blob).map_err(|_| VaultError::WrongPassword)?;
    if pt.len() != KEY_LEN {
        return Err(VaultError::Io("DEK 长度异常".into()));
    }
    let mut dek = Zeroizing::new([0u8; KEY_LEN]);
    dek.copy_from_slice(&pt);
    Ok(dek)
}

impl Vault {
    pub fn open(dir: &Path, db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| format!("打开保险库存储失败: {e}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("设置 WAL 失败: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|e| format!("初始化 meta 表失败: {e}"))?;

        let get = |k: &str| -> Option<String> {
            conn.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get(0))
                .ok()
        };
        let salt_hex = get("vault_salt").unwrap_or_default();
        let wrapped = get("vault_wrapped_dek").and_then(|s| B64.decode(s.as_bytes()).ok());
        let auto_lock_minutes = get("auto_lock_minutes")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_AUTOLOCK_MINUTES);
        let keychain_auto_unlock = get("keychain_auto_unlock").is_some_and(|s| s == "1");

        Ok(Self {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
            inner: Mutex::new(Inner {
                dek: None,
                last_activity: Instant::now(),
                salt_hex,
                wrapped_dek: wrapped,
                auto_lock_minutes,
                keychain_auto_unlock,
            }),
        })
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), VaultError> {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
        .map_err(|e| VaultError::Io(format!("写入 meta 失败: {e}")))?;
        Ok(())
    }

    pub fn status(&self) -> VaultStatus {
        let g = self.inner();
        VaultStatus {
            initialized: g.wrapped_dek.is_some() && !g.salt_hex.is_empty(),
            unlocked: g.dek.is_some(),
            auto_lock_minutes: g.auto_lock_minutes,
            keychain_auto_unlock: g.keychain_auto_unlock,
        }
    }

    /// 首次初始化：生成 salt 与 DEK，用主密码包裹后落盘并解锁。
    pub fn setup(&self, password: &str) -> Result<(), VaultError> {
        if password.chars().count() < MIN_PASSWORD_LEN {
            return Err(VaultError::WeakPassword(format!(
                "主密码至少 {MIN_PASSWORD_LEN} 位"
            )));
        }
        let mut g = self.inner();
        if g.wrapped_dek.is_some() {
            return Err(VaultError::AlreadyInitialized);
        }
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let kek = argon2_kek(password, &salt)?;
        let mut dek = Zeroizing::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(&mut *dek);
        let wrapped = wrap_dek(&kek, &dek)?;

        let conn = self.conn();
        Self::set_meta(&conn, "vault_salt", &to_hex(&salt))?;
        Self::set_meta(&conn, "vault_wrapped_dek", &B64.encode(&wrapped))?;

        g.salt_hex = to_hex(&salt);
        g.wrapped_dek = Some(wrapped);
        g.dek = Some(dek);
        g.last_activity = Instant::now();
        drop(g);
        self.keychain_sync()?;
        Ok(())
    }

    /// 解锁：主密码派生 KEK 解包 DEK。
    pub fn unlock(&self, password: &str) -> Result<(), VaultError> {
        let mut g = self.inner();
        let salt = from_hex(&g.salt_hex).map_err(VaultError::Io)?;
        let wrapped = g.wrapped_dek.clone().ok_or(VaultError::NotInitialized)?;
        let kek = argon2_kek(password, &salt)?;
        let dek = unwrap_dek(&kek, &wrapped)?;
        g.dek = Some(dek);
        g.last_activity = Instant::now();
        Ok(())
    }

    pub fn lock(&self) {
        self.inner().dek = None;
    }

    /// 取 DEK（同时刷新活动时间）；若已越过空闲阈值则先自动锁定。
    pub fn dek(&self) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
        let mut g = self.inner();
        self.auto_lock_if_expired(&mut g);
        if g.dek.is_some() {
            g.last_activity = Instant::now();
            Ok(g.dek.clone().unwrap())
        } else {
            Err(VaultError::Locked)
        }
    }

    fn auto_lock_if_expired(&self, g: &mut Inner) {
        if g.dek.is_some()
            && g.auto_lock_minutes > 0
            && g.last_activity.elapsed().as_secs() >= g.auto_lock_minutes * 60
        {
            g.dek = None;
        }
    }

    /// 周期任务调用：若发生自动锁返回 true。
    pub fn check_auto_lock(&self) -> bool {
        let mut g = self.inner();
        let was_unlocked = g.dek.is_some();
        self.auto_lock_if_expired(&mut g);
        was_unlocked && g.dek.is_none()
    }

    pub fn set_settings(
        &self,
        auto_lock_minutes: u64,
        keychain_auto_unlock: bool,
    ) -> Result<(), VaultError> {
        let mut g = self.inner();
        {
            let conn = self.conn();
            Self::set_meta(&conn, "auto_lock_minutes", &auto_lock_minutes.to_string())?;
            Self::set_meta(
                &conn,
                "keychain_auto_unlock",
                if keychain_auto_unlock { "1" } else { "0" },
            )?;
        }
        g.auto_lock_minutes = auto_lock_minutes;
        g.keychain_auto_unlock = keychain_auto_unlock;
        drop(g);
        self.keychain_sync()?;
        Ok(())
    }

    /// 依据开关同步 Keychain 内容（开启且已解锁时写入；关闭时删除）。
    fn keychain_sync(&self) -> Result<(), VaultError> {
        let g = self.inner();
        let enabled = g.keychain_auto_unlock;
        let dek = g.dek.clone();
        drop(g);
        let entry =
            keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| {
                VaultError::Keychain(format!("创建条目失败: {e}"))
            })?;
        if enabled {
            match dek {
                Some(d) => entry
                    .set_password(&B64.encode(d.as_slice()))
                    .map_err(|e| VaultError::Keychain(format!("写入失败: {e}")))?,
                None => return Err(VaultError::Locked),
            }
        } else {
            let _ = entry.delete_credential(); // 不存在不算错误
        }
        Ok(())
    }

    /// 启动时尝试用 Keychain 自动解锁（仅在开关开启时有效）。
    pub fn try_keychain_auto_unlock(&self) -> Result<bool, VaultError> {
        let enabled = self.inner().keychain_auto_unlock;
        if !enabled {
            return Ok(false);
        }
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
            .map_err(|e| VaultError::Keychain(format!("创建条目失败: {e}")))?;
        let b64 = match entry.get_password() {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        let raw = B64.decode(b64.as_bytes())
            .map_err(|_| VaultError::Keychain("Keychain 内容损坏".into()))?;
        if raw.len() != KEY_LEN {
            return Err(VaultError::Keychain("Keychain 内容长度异常".into()));
        }
        let mut dek = Zeroizing::new([0u8; KEY_LEN]);
        dek.copy_from_slice(&raw);
        let mut g = self.inner();
        if g.wrapped_dek.is_none() {
            return Ok(false);
        }
        g.dek = Some(dek);
        g.last_activity = Instant::now();
        Ok(true)
    }

    /// 迁移 M1 过渡密钥加密的旧凭据到当前 DEK。需已解锁。
    pub fn migrate_legacy_if_needed(&self, store: &crate::store::Store) -> Result<MigrationOutcome, VaultError> {
        let legacy_raw = match crypto::load_legacy_key(&self.dir) {
            Some(k) => k,
            None => return Ok(MigrationOutcome { migrated: 0, failed: Vec::new() }),
        };
        let mut legacy = Zeroizing::new([0u8; KEY_LEN]);
        legacy.copy_from_slice(&legacy_raw);
        let dek = self.dek()?;
        let mut out = MigrationOutcome { migrated: 0, failed: Vec::new() };
        for row in store.list().map_err(VaultError::Io)? {
            match crypto::decrypt(&legacy, &row.secret_blob) {
                Ok(plain) => match crypto::encrypt(&dek, &plain) {
                    Ok(blob) => match store.update_secret_blob(&row.id, &blob) {
                        Ok(true) => out.migrated += 1,
                        Ok(false) => out.failed.push(row.name),
                        Err(e) => out.failed.push(format!("{}（{e}）", row.name)),
                    },
                    Err(e) => out.failed.push(format!("{}（{e}）", row.name)),
                },
                Err(e) => out.failed.push(format!("{}（{e}）", row.name)),
            }
        }
        if out.failed.is_empty() && out.migrated > 0 {
            crypto::remove_legacy_key(&self.dir);
        }
        Ok(out)
    }

    /// 危险操作：清除保险库（忘记主密码时的最后手段）。数据需另行清理。
    pub fn reset(&self) -> Result<(), VaultError> {
        let conn = self.conn();
        for k in ["vault_salt", "vault_wrapped_dek"] {
            conn.execute("DELETE FROM meta WHERE key=?1", [k])
                .map_err(|e| VaultError::Io(e.to_string()))?;
        }
        drop(conn);
        let mut g = self.inner();
        g.salt_hex.clear();
        g.wrapped_dek = None;
        g.dek = None;
        drop(g);
        if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
            let _ = entry.delete_credential();
        }
        crypto::remove_legacy_key(&self.dir);
        Ok(())
    }
}
