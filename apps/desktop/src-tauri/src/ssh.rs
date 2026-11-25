//! russh SSH 连接池：连接保活、断线自动重连（有限重试 + 指数退避）、
//! 命令执行（超时 + 输出截断）、TOFU 主机密钥校验（known_hosts 指纹比对）。

use crate::store::Store;
use tokio::io::{AsyncReadExt as FuturesRead, AsyncWriteExt as FuturesWrite};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use russh::client::Handle;
use russh::Disconnect;
use shared::AuthMethod;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

const MAX_OUTPUT: usize = 512 * 1024;
const RECONNECT_MAX: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnStatus {
    Connected,
    /// 由「不在连接池中」推断，非池内显式状态（供前端/契约使用）
    #[allow(dead_code)]
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
    /// 读取输出超时（远端可能仍在执行）：调用方必须能与「正常结束」区分开。
    pub timed_out: bool,
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

/// 手动连接的取消通道：key = server_id，value = (连接代号, 取消发送端)。
/// 代号用于让「结束的连接」只清理自己那条登记，绝不误删后来者的。
type CancelMap = HashMap<String, (u64, tokio::sync::oneshot::Sender<()>)>;

#[derive(Clone, Default)]
pub struct SshPool {
    pool: Arc<AsyncMutex<Pool>>,
    cancels: Arc<AsyncMutex<CancelMap>>,
    /// 连接代号发号器（AtomicU64 不便 derive Default，由 new() 初始化）
    seq: Arc<std::sync::atomic::AtomicU64>,
}

/// 用户主动取消连接时的统一错误文案（前端据此区分「失败」与「取消」）。
pub const CONNECT_CANCELLED: &str = "连接已取消";

fn make_config() -> Arc<russh::client::Config> {
    Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    })
}

impl SshPool {
    pub fn new() -> Self {
        Self {
            pool: Arc::new(AsyncMutex::new(Pool::default())),
            cancels: Arc::new(AsyncMutex::new(CancelMap::new())),
            seq: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
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
    ///
    /// 支持取消：连接进行中调用 [`cancel_connect`] 会立刻中断握手（返回
    /// [`CONNECT_CANCELLED`]）。握手期间**不持有连接池锁**，避免状态轮询被阻塞。
    pub async fn connect(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: ConnParams,
    ) -> Result<(), String> {
        // 重建前先丢弃旧会话（短暂持锁，随后释放）
        {
            let mut pool = self.pool.lock().await;
            let _ = self.take(&mut pool, server_id);
        }

        // 注册取消通道：同一服务器重复发起连接时，旧连接的取消信号先失效（视为被取消）
        let my_id = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        self.cancels
            .lock()
            .await
            .insert(server_id.to_string(), (my_id, tx));

        let result = tokio::select! {
            r = self.establish(store, server_id, &p) => r,
            _ = rx => Err(CONNECT_CANCELLED.to_string()),
        };

        // 只清理本连接自己的登记：若已被新一轮连接覆盖（代号不同），保持不动
        {
            let mut cancels = self.cancels.lock().await;
            if cancels.get(server_id).is_some_and(|(id, _)| *id == my_id) {
                cancels.remove(server_id);
            }
        }

        let session = result?;
        let mut pool = self.pool.lock().await;
        pool.sessions.insert(server_id.to_string(), session);
        Ok(())
    }

    /// 取消进行中的手动连接；返回是否确有连接在进行（无可取消对象 → false）。
    pub async fn cancel_connect(&self, server_id: &str) -> bool {
        let mut cancels = self.cancels.lock().await;
        match cancels.remove(server_id) {
            Some((_, tx)) => {
                // 接收方已被 drop（连接已结束）时发送会失败，仍视为「取消已发出」
                let _ = tx.send(());
                true
            }
            None => false,
        }
    }

    /// 是否有连接正在进行中（供前端/状态展示）。
    pub async fn is_connecting(&self, server_id: &str) -> bool {
        self.cancels.lock().await.contains_key(server_id)
    }
    pub async fn disconnect(&self, server_id: &str) -> bool {
        let mut pool = self.pool.lock().await;
        match self.take(&mut pool, server_id) {
            Some(s) => {
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

    // ---------- SFTP ----------

    /// 取会话：优先复用连接池，否则新建（一次性，用完归还/丢弃）。
    async fn acquire_session(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
    ) -> Result<Handle<HostKeyVerifier>, String> {
        {
            let mut pool = self.pool.lock().await;
            if let Some(s) = pool.sessions.remove(server_id) {
                return Ok(s);
            }
        }
        self.establish(store, server_id, p).await
    }

    async fn release_session(&self, server_id: &str, s: Handle<HostKeyVerifier>) {
        self.pool
            .lock()
            .await
            .sessions
            .insert(server_id.to_string(), s);
    }

    async fn open_sftp(session: &mut Handle<HostKeyVerifier>) -> Result<SftpSession, String> {
        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| format!("打开 SFTP 通道失败：{e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| format!("请求 sftp 子系统失败：{e}"))?;
        SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| format!("SFTP 初始化失败：{e}"))
    }

    async fn sftp_write_bytes(
        session: &mut Handle<HostKeyVerifier>,
        remote_path: &str,
        data: &[u8],
    ) -> Result<u64, String> {
        let sftp = Self::open_sftp(session).await?;
        let mut file = sftp
            .create(remote_path)
            .await
            .map_err(|e| format!("创建远程文件失败（路径不存在或无权限）：{e}"))?;
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + 32768).min(data.len());
            FuturesWrite::write_all(&mut file, &data[off..end])
                .await
                .map_err(|e| format!("写入远程文件失败：{e}"))?;
            off = end;
        }
        FuturesWrite::flush(&mut file)
            .await
            .map_err(|e| format!("刷新远程文件失败：{e}"))?;
        Ok(data.len() as u64)
    }

    /// 上传本地文件（≤512MB）。失败时会话丢弃，下次操作自动重建。
    pub async fn sftp_upload(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        local_path: &str,
        remote_path: &str,
    ) -> Result<u64, String> {
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| format!("读取本地文件失败：{e}"))?;
        if meta.len() > 512 * 1024 * 1024 {
            return Err("文件超过 512MB 上限".into());
        }
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("读取本地文件失败：{e}"))?;
        let mut session = self.acquire_session(store, server_id, p).await?;
        match Self::sftp_write_bytes(&mut session, remote_path, &data).await {
            Ok(n) => {
                self.release_session(server_id, session).await;
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }

    /// 从内存写入远程文件（AI write_file 用）。
    pub async fn sftp_write_file(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        remote_path: &str,
        content: &[u8],
    ) -> Result<u64, String> {
        let mut session = self.acquire_session(store, server_id, p).await?;
        match Self::sftp_write_bytes(&mut session, remote_path, content).await {
            Ok(n) => {
                self.release_session(server_id, session).await;
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }

    /// 真正的追加语义：以 APPEND 打开（不 truncate、不创建），审批期间别人写入的内容不会被覆盖。
    pub async fn sftp_append_file(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        remote_path: &str,
        content: &[u8],
    ) -> Result<u64, String> {
        let mut session = self.acquire_session(store, server_id, p).await?;
        match Self::sftp_append_bytes(&mut session, remote_path, content).await {
            Ok(n) => {
                self.release_session(server_id, session).await;
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }

    async fn sftp_append_bytes(
        session: &mut Handle<HostKeyVerifier>,
        remote_path: &str,
        data: &[u8],
    ) -> Result<u64, String> {
        let sftp = Self::open_sftp(session).await?;
        // 不带 CREATE：文件不存在直接报错（append 只对已有文件；新建走 write_file）
        let mut file = sftp
            .open_with_flags(
                remote_path,
                OpenFlags::WRITE | OpenFlags::APPEND,
            )
            .await
            .map_err(|e| format!("远程文件不存在或不可写：{e}"))?;
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + 32768).min(data.len());
            FuturesWrite::write_all(&mut file, &data[off..end])
                .await
                .map_err(|e| format!("追加远程文件失败：{e}"))?;
            off = end;
        }
        FuturesWrite::flush(&mut file)
            .await
            .map_err(|e| format!("刷新远程文件失败：{e}"))?;
        Ok(data.len() as u64)
    }

    /// 读远程文件到内存，cap 为大小上限；返回 (数据, 完整大小, 是否截断)。
    async fn sftp_read_bytes(
        session: &mut Handle<HostKeyVerifier>,
        remote_path: &str,
        cap: u64,
    ) -> Result<(Vec<u8>, u64, bool), String> {
        let sftp = Self::open_sftp(session).await?;
        let mut file = sftp
            .open(remote_path)
            .await
            .map_err(|_| "远程文件不存在或无法读取".to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 32768];
        loop {
            let n = FuturesRead::read(&mut file, &mut chunk)
                .await
                .map_err(|e| format!("读取远程文件失败：{e}"))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let full = buf.len() as u64;
        let truncated = full > cap;
        if truncated {
            buf.truncate(cap as usize);
        }
        Ok((buf, full, truncated))
    }

    /// 下载远程文件到本地路径。
    pub async fn sftp_download(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        remote_path: &str,
        save_path: &str,
        max: u64,
    ) -> Result<u64, String> {
        let mut session = self.acquire_session(store, server_id, p).await?;
        let res = Self::sftp_read_bytes(&mut session, remote_path, max).await;
        match res {
            Ok((data, _full, truncated)) => {
                if truncated {
                    Err(format!("文件超过 {}MB 上限", max / 1024 / 1024))
                } else {
                    let n = data.len() as u64;
                    match tokio::fs::write(save_path, &data).await {
                        Ok(()) => {
                            self.release_session(server_id, session).await;
                            Ok(n)
                        }
                        Err(e) => Err(format!("写入本地文件失败：{e}")),
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// AI read_file：读取远程文件内容（cap 截断）。
    pub async fn sftp_read_file(
        &self,
        store: &Arc<Store>,
        server_id: &str,
        p: &ConnParams,
        remote_path: &str,
        cap: u64,
    ) -> Result<(Vec<u8>, u64, bool), String> {
        let mut session = self.acquire_session(store, server_id, p).await?;
        let res = Self::sftp_read_bytes(&mut session, remote_path, cap).await;
        match res {
            Ok(v) => {
                self.release_session(server_id, session).await;
                Ok(v)
            }
            Err(e) => Err(e),
        }
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

        // 用字节缓冲累积，边收边限长：不会先把整段输出读进内存再截断。
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        let mut code: Option<u32> = None;
        let mut truncated = false;

        let deadline = tokio::time::Instant::now() + timeout;
        let timed_out = {
            let read = async {
                loop {
                    match channel.wait().await {
                        Some(russh::ChannelMsg::Data { data }) => {
                            push_capped(&mut stdout, data.as_ref(), &mut truncated);
                            if truncated {
                                break;
                            }
                        }
                        Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                            push_capped(&mut stderr, data.as_ref(), &mut truncated);
                            if truncated {
                                break;
                            }
                        }
                        Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                            code = Some(exit_status);
                        }
                        Some(russh::ChannelMsg::Eof) => {}
                        Some(russh::ChannelMsg::Close) | None => break,
                        _ => {}
                    }
                }
            };
            tokio::time::timeout_at(deadline, read).await.is_err()
        };

        Ok(ExecResult {
            stdout: bytes_to_string(stdout),
            stderr: bytes_to_string(stderr),
            exit_code: code,
            truncated,
            timed_out,
        })
    }
}

/// 追加一段输出，超过上限即标记截断（不再继续累积）。
fn push_capped(buf: &mut Vec<u8>, chunk: &[u8], truncated: &mut bool) {
    let room = MAX_OUTPUT.saturating_sub(buf.len());
    if room == 0 {
        *truncated = true;
        return;
    }
    if chunk.len() > room {
        buf.extend_from_slice(&chunk[..room]);
        *truncated = true;
    } else {
        buf.extend_from_slice(chunk);
    }
}

/// 字节 → 字符串：按 lossy 解码，截断处即使切断多字节字符也不会 panic。
fn bytes_to_string(buf: Vec<u8>) -> String {
    String::from_utf8_lossy(&buf).into_owned()
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
