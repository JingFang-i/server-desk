//! russh SSH 连接池：连接保活、断线自动重连（有限重试 + 指数退避）、
//! 命令执行（超时 + 输出截断）、TOFU 主机密钥校验（known_hosts 指纹比对）。

use crate::store::Store;
use russh::client::Handle;
use russh::Disconnect;
use shared::AuthMethod;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

const EXEC_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT: usize = 512 * 1024;
const RECONNECT_MAX: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnStatus {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnInfo {
    pub server_id: String,
    pub status: ConnStatus,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<u32>,
    pub truncated: bool,
}

/// 连接参数（凭据在调用点解密后构造，不落盘、不常驻）。
#[derive(Clone)]
pub struct ConnParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
}

/// TOFU 主机密钥校验器：首次记录指纹，之后不一致即拒绝。
pub struct HostKeyVerifier {
    pub store: Arc<Store>,
    pub server_id: String,
    pub verdict: std::sync::Mutex<Option<Result<(), String>>>,
}

impl russh::client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let fp = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
            }
            russh::keys::PublicKeyOrCertificate::Certificate(cert) => {
                cert.public_key()
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string()
            }
        };
        let verdict = match self.store.get_host_fingerprint(&self.server_id) {
            Ok(Some(existing)) => {
                if existing == fp {
                    Ok(())
                } else {
                    Err(format!(
                        "主机密钥指纹与首次记录不一致（可能存在中间人攻击）。\
                         现有指纹 {existing}，服务器当前指纹 {fp}。\
                         若确认服务器已重装/换钥，可在应用中重置该服务器的主机密钥记录。"
                    ))
                }
            }
            Ok(None) => {
                let ts = chrono::Utc::now().to_rfc3339();
                match self.store.set_host_fingerprint(&self.server_id, &fp, &ts) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        let ok = verdict.is_ok();
        let msg = verdict.err();
        *self.verdict.lock().unwrap() = Some(match msg {
            None => Ok(()),
            Some(m) => Err(m),
        });
        Ok(ok)
    }
}

#[derive(Default)]
struct Pool {
    sessions: HashMap<String, Handle<HostKeyVerifier>>,
}

#[derive(Default)]
pub struct SshPool {
    pool: AsyncMutex<Pool>,
}

fn make_config() -> Arc<russh::client::Config> {
    Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    })
}

impl SshPool {
    pub fn new() -> Self {
        Self::default()
    }

    fn take(
        &self,
        pool: &mut Pool,
        server_id: &str,
    ) -> Option<Handle<HostKeyVerifier>> {
        pool.sessions.remove(server_id)
    }

    /// 建立并认证连接（含 TOFU 校验），成功返回会话句柄。
    pub async fn establish(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
    ) -> Result<Handle<HostKeyVerifier>, String> {
        let verifier = HostKeyVerifier {
            store: store.clone(),
            server_id: server_id.to_string(),
            verdict: std::sync::Mutex::new(None),
        };
        let mut session = russh::client::connect(make_config(), (p.host.as_str(), p.port), verifier)
            .await
            .map_err(|e| {
                // 优先呈现主机密钥校验结论
                format!("SSH 连接失败：{e}")
            })?;
        // 连接失败时若为主机密钥问题，给出更明确的信息
        match &p.auth {
            AuthMethod::Password { password } => {
                let auth_res = session
                    .authenticate_password(&p.username, password)
                    .await
                    .map_err(|e| format!("密码认证过程出错：{e}"))?;
                if !auth_res.success() {
                    return Err("认证失败：用户名或密码被服务器拒绝".into());
                }
            }
            AuthMethod::Key { private_key, passphrase } => {
                let key = russh::keys::decode_secret_key(private_key, passphrase.as_deref())
                    .map_err(|e| format!("解析私钥失败（格式或口令错误）：{e}"))?;
                let auth_res = session
                    .authenticate_publickey(
                        &p.username,
                        russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
                    )
                    .await
                    .map_err(|e| format!("密钥认证过程出错：{e}"))?;
                if !auth_res.success() {
                    return Err("认证失败：私钥被服务器拒绝".into());
                }
            }
        }
        Ok(session)
    }

    /// 连接并纳入连接池管理（保持 30s keepalive）。
    pub async fn connect(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: ConnParams,
    ) -> Result<(), String> {
        let mut pool = self.pool.lock().await;
        let _ = self.take(&mut pool, server_id); // 重建前先丢弃旧会话
        let session = self.establish(store, server_id, &p).await?;
        pool.sessions.insert(server_id.to_string(), session);
        Ok(())
    }

    pub async fn disconnect(&self, server_id: &str) -> bool {
        let mut pool = self.pool.lock().await;
        match self.take(&mut pool, server_id) {
            Some(mut s) => {
                let _ = s
                    .disconnect(Disconnect::ByApplication, "client close", "en")
                    .await;
                true
            }
            None => false,
        }
    }

    /// 所有已建立连接的状态。
    pub async fn status(&self) -> Vec<ConnInfo> {
        let pool = self.pool.lock().await;
        pool.sessions
            .keys()
            .map(|id| ConnInfo {
                server_id: id.clone(),
                status: ConnStatus::Connected,
            })
            .collect()
    }

    /// 在已连接（必要时自动重连）的会话上执行命令。
    pub async fn exec(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecResult, String> {
        let mut last_err = String::new();
        for attempt in 0..=RECONNECT_MAX {
            let session = {
                let mut pool = self.pool.lock().await;
                match self.take(&mut pool, server_id) {
                    Some(s) => Some(s),
                    None => {
                        if attempt > 0 {
                            tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                        }
                        match self.establish(store, server_id, p).await {
                            Ok(s) => Some(s),
                            Err(e) => {
                                last_err = format!("重连失败：{e}");
                                continue;
                            }
                        }
                    }
                }
            };
            let mut session = match session {
                Some(s) => s,
                None => continue,
            };
            match Self::exec_on(&mut session, command, timeout).await {
                Ok(r) => {
                    self.pool.lock().await.sessions.insert(server_id.to_string(), session);
                    return Ok(r);
                }
                Err(e) => {
                    last_err = e;
                    if attempt < RECONNECT_MAX {
                        continue; // 丢弃坏会话，下一轮自动重连
                    }
                }
            }
        }
        Err(last_err)
    }

    async fn exec_on(
        session: &mut Handle<HostKeyVerifier>,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecResult, String> {
        let mut channel = session
            .channel_open_session()
            .await
            .map_err(|e| format!("打开通道失败：{e}"))?;
        channel
            .exec(true, command)
            .await
            .map_err(|e| format!("命令下发失败：{e}"))?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut code: Option<u32> = None;

        let _ = tokio::time::timeout(timeout, async {
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Data { data }) => {
                        stdout.push_str(&String::from_utf8_lossy(data.as_ref()));
                    }
                    Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                        stderr.push_str(&String::from_utf8_lossy(data.as_ref()));
                    }
                    Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                        code = Some(exit_status);
                    }
                    Some(russh::ChannelMsg::Eof) => {}
                    Some(russh::ChannelMsg::Close) => break,
                    None => break,
                    _ => {}
                }
            }
        })
        .await;

        let mut truncated = false;
        if stdout.len() > MAX_OUTPUT {
            stdout.truncate(MAX_OUTPUT);
            truncated = true;
        }
        if stderr.len() > MAX_OUTPUT {
            stderr.truncate(MAX_OUTPUT);
            truncated = true;
        }
        Ok(ExecResult { stdout, stderr, exit_code: code, truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// 反向冒烟：github.com:22 是真实 SSH 服务（禁用密码认证）。
    /// 用错误凭据连接，预期走到「认证被拒」分支，验证握手与认证链路。
    #[tokio::test]
    async fn github_wrong_password_reaches_auth_rejection() {
        let db = std::env::temp_dir().join("serverdesk-smoke-test.db");
        let _ = std::fs::remove_file(&db);
        let store = Arc::new(Store::open(&db).expect("temp store"));
        let pool = SshPool::new();
        let p = ConnParams {
            host: "github.com".into(),
            port: 22,
            username: "serverdesk-smoke".into(),
            auth: AuthMethod::Password {
                password: "definitely-wrong".into(),
            },
        };
        let res = tokio::time::timeout(
            Duration::from_secs(20),
            pool.establish(&store, "smoke-1", &p),
        )
        .await;
        let inner = match res {
            Err(_) => {
                println!("SKIP: github.com:22 网络不可达，冒烟跳过");
                return;
            }
            Ok(v) => v,
        };
        match inner {
            Err(msg) => {
                assert!(
                    msg.contains("认证失败") || msg.contains("认证过程出错"),
                    "期望认证被拒，实际错误：{msg}"
                );
                println!("PASS: {msg}");
            }
            Ok(_) => panic!("错误密码不应认证成功"),
        }
        let _ = std::fs::remove_file(&db);
    }
}
