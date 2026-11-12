//! 一条 SSH PTY 会话 + 一个仿真器,导出给 Kotlin(F345 最小终端)。
//!
//! 泵任务的三条红线:
//! - **T1**:仿真器产生的应答(光标查询 / DA / 同步探测)必须回写 channel,
//!   否则全屏 TUI 卡死。见 [`absorb`]。
//! - **T2**:BSU 之后字节攒在 vte 肚子里,调用方不主动收口就永远不到期。
//!   泵在没有新字节时也要按 `vt_sync_deadline` 醒一次。
//! - **T4**:`resize` 同时改仿真器和远端 PTY,只改一边就错行。

use std::sync::{Arc, Mutex};

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::{connect, SshSession, TrySendErr};
use mullion_term::emulator::Emulator;
use tokio::sync::mpsc;

use crate::error::FfiError;
use crate::runtime::runtime;
use crate::screen::{project, ScreenText};

/// 一包入站字节进仿真器,返回**必须回写 channel** 的应答(T1)。
pub(crate) fn absorb(emu: &mut Emulator, bytes: &[u8]) -> Vec<u8> {
    emu.feed(bytes);
    emu.take_pty_writes()
}

/// 连接参数(F345:只支持密码认证;私钥、跳板、项目是 F346/F349 的事)。
#[derive(uniffi::Record)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub cols: u16,
    pub rows: u16,
}

impl From<TrySendErr> for FfiError {
    fn from(e: TrySendErr) -> Self {
        match e {
            TrySendErr::Full => Self::Busy,
            TrySendErr::Closed => Self::Closed,
        }
    }
}

/// Kotlin 实现:屏幕有变化 / 连接没了。**在 ffi 的工作线程上回调**,
/// Kotlin 侧只能做「标记脏、请求下一帧」这种轻活,不能阻塞。
#[uniffi::export(with_foreign)]
pub trait ScreenListener: Send + Sync {
    fn on_dirty(&self);
    fn on_closed(&self);
}

#[derive(uniffi::Object)]
pub struct Terminal {
    session: Arc<SshSession>,
    emu: Arc<Mutex<Emulator>>,
    pump: tokio::task::JoinHandle<()>,
}

#[uniffi::export]
impl Terminal {
    /// 阻塞直到握手 + 开 PTY 完成。Kotlin 侧在 `Dispatchers.IO` 上调。
    ///
    /// 主机密钥:F345 用进程内 TOFU(首见即收、不落盘)。**F350 换成真 TOFU**
    /// (包里带来的指纹 + 首见弹窗);本切片的 APK 不发布。
    #[uniffi::constructor]
    pub fn connect(
        params: ConnectParams,
        listener: Arc<dyn ScreenListener>,
    ) -> Result<Arc<Self>, FfiError> {
        let cfg = SshConfig {
            host: params.host,
            port: params.port,
            user: params.user,
            auth: AuthMethod::Password(params.password),
            cols: params.cols,
            rows: params.rows,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        };
        let policy = Arc::new(TofuAccept::new(Arc::new(std::sync::Mutex::new(
            KnownHosts::new(),
        ))));
        let rt = runtime();
        let (session, rx) = rt
            .block_on(connect(&cfg, policy, Arc::new(|| {})))
            .map_err(|e| FfiError::Connect(e.to_string()))?;
        let session = Arc::new(session);
        let emu = Arc::new(Mutex::new(Emulator::new(cfg.cols, cfg.rows)));
        let pump = rt.spawn(pump(rx, emu.clone(), session.clone(), listener));
        Ok(Arc::new(Self { session, emu, pump }))
    }

    /// 把键盘 / 输入框的字节发给远端。非阻塞。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), FfiError> {
        Ok(self.session.write(bytes)?)
    }

    /// T4:仿真器和远端 PTY **一起**改。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), FfiError> {
        self.emu.lock().unwrap().resize(cols, rows);
        Ok(self.session.resize(cols, rows)?)
    }

    /// 当前屏幕的纯文本投影。
    pub fn screen(&self) -> ScreenText {
        project(&self.emu.lock().unwrap().snapshot())
    }

    /// 关 channel、停泵。可重复调用。
    ///
    /// **不能叫 `close`**:uniffi 生成的 Kotlin 类本身实现 `AutoCloseable`,
    /// 自带一个释放对象的 `close()`,同名导出会撞成两个同签名方法、编译不过。
    /// Kotlin 侧调 `close()`(释放 → Rust `Drop`)同样会走到这里。
    pub fn disconnect(&self) {
        self.session.close();
        self.pump.abort();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.disconnect();
    }
}

async fn pump(
    mut rx: mpsc::Receiver<Vec<u8>>,
    emu: Arc<Mutex<Emulator>>,
    session: Arc<SshSession>,
    listener: Arc<dyn ScreenListener>,
) {
    loop {
        // 锁只在这一句里持有,不跨 await。
        let deadline = emu.lock().unwrap().vt_sync_deadline();
        let bytes = match deadline {
            Some(d) => tokio::select! {
                b = rx.recv() => b,
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(d)) => {
                    // T2:到点了没人来问,超时就形同虚设。
                    if emu.lock().unwrap().flush_expired_sync(std::time::Instant::now()) {
                        listener.on_dirty();
                    }
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(bytes) = bytes else {
            listener.on_closed();
            return;
        };
        let reply = absorb(&mut emu.lock().unwrap(), &bytes);
        if !reply.is_empty() {
            // T1。队列满就丢:应答晚到比卡住泵更糟,远端会重发查询。
            let _ = session.write(reply);
        }
        listener.on_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_write_reply_is_returned_for_writeback() {
        // T1:CSI 6n(光标位置查询)必须产出应答 CSI 1;1R。
        let mut e = Emulator::new(80, 24);
        assert_eq!(absorb(&mut e, b"\x1b[6n"), b"\x1b[1;1R".to_vec());
        // 读即清:同一份应答不会被回写两次。
        assert!(absorb(&mut e, b"").is_empty());
    }

    #[test]
    fn emulator_can_live_on_the_pump_task() {
        // 泵在 tokio 工作线程上跑,仿真器得能跨线程。
        fn assert_send<T: Send>() {}
        assert_send::<Emulator>();
    }
}
