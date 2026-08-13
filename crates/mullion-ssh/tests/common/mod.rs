//! 进程内 echo 测试 server。收到数据原样回显,供客户端断言往返。
//! 注:server Handler 的方法签名以本机 russh 0.54 源码为准,如编译报签名不符按提示微调。

pub mod sftp_server;

use std::sync::Arc;

use russh::keys::PublicKey;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};

pub const TEST_USER: &str = "testuser";
pub const TEST_PASSWORD: &str = "test-password";

/// `common` 模块被每个测试文件各自编译一份;在不用 echo server 的二进制里
/// (`sftp_browse`)它是死代码 —— 同下面 `SftpSshHandler` 那条 allow 一样的
/// 道理,反过来而已。
#[allow(dead_code)]
pub struct EchoHandler;

impl Handler for EchoHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(&mut self, user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        // 测试 server 接受任意 pubkey(客户端持 client_key);真机由 authorized_keys 把关。
        if user == TEST_USER {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // 回显收到的字节,供客户端断言往返。
        session.data(channel, CryptoVec::from(data.to_vec()))?;
        Ok(())
    }
}

/// 在 127.0.0.1:0 起 echo server,返回实际监听地址。随进程/运行时结束回收。
#[allow(dead_code)]
pub async fn spawn_echo_server() -> std::net::SocketAddr {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test server");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let config = config.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, EchoHandler).await;
            });
        }
    });
    addr
}

/// 带 SFTP subsystem 的假 sshd。`EchoHandler` 保持原样不动 —— 那个被
/// auth/pty 几个测试用着,给它加分支等于让不相关的测试跟着变。
///
/// `common` 模块被每个测试文件各自编译一份;在还没有测试用到 SFTP 的
/// 二进制里(pty/auth/two_hop_jump/smoke_server)它是死代码,下一个 Task
/// (`sftp_browse.rs`)加上后就会用起来,故 allow。
#[allow(dead_code)]
pub struct SftpSshHandler {
    channels: std::collections::HashMap<ChannelId, Channel<Msg>>,
    tree: Arc<sftp_server::Tree>,
    probe: Arc<std::sync::Mutex<sftp_server::Probe>>,
}

impl Handler for SftpSshHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    /// 只为数数:SFTP 通道上一次都不该被调用。
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.probe.lock().unwrap().pty_requests += 1;
        session.channel_success(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let Some(ch) = self.channels.remove(&channel) else {
                session.channel_failure(channel)?;
                return Ok(());
            };
            session.channel_success(channel)?;
            // `run` 内部自己 spawn,立即返回;别在这儿 await 到天荒地老。
            russh_sftp::server::run(
                ch.into_stream(),
                sftp_server::SftpHandler::new(self.tree.clone(), self.probe.clone()),
            )
            .await;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }
}

/// 起一个带 SFTP 的假 sshd。返回监听地址与探针(测试用它断言「哪些请求发出去了」)。
#[allow(dead_code)]
pub async fn spawn_sftp_server(
    tree: sftp_server::Tree,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
) {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let tree = Arc::new(tree);
    let probe = Arc::new(std::sync::Mutex::new(sftp_server::Probe::default()));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let (t, p) = (tree.clone(), probe.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            let handler = SftpSshHandler {
                channels: std::collections::HashMap::new(),
                tree: t.clone(),
                probe: p.clone(),
            };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, probe)
}
