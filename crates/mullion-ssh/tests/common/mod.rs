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
    spawn_echo_server_with_hostkey("tests/fixtures/server_hostkey").await
}

/// 指定主机密钥的 echo server。要在**同一个** 127.0.0.1 上摆出两台指纹不同的
/// 主机时用(F3-a 的键冲突测试)。
#[allow(dead_code)]
pub async fn spawn_echo_server_with_hostkey(key_path: &str) -> std::net::SocketAddr {
    let host_key = russh::keys::load_secret_key(key_path, None).expect("load hostkey");
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
    tree: Arc<std::sync::Mutex<sftp_server::Tree>>,
    probe: Arc<std::sync::Mutex<sftp_server::Probe>>,
    /// `false` = 像 sftp-only 账号(`ForceCommand internal-sftp` +
    /// `ChrootDirectory`)那样**拒绝** exec。F57 的回退分支靠它触发 ——
    /// 没有这个开关,「exec 被拒时回退」那条路径在测试里永远走不到。
    allow_exec: bool,
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

    /// 记下命令行;`allow_exec == false` 时直接拒(见该字段的文档)。
    ///
    /// 允许执行时**只认 `rm -rf -- <路径…>` 这一种**,并真的在内存树上删掉。
    /// 起一个真 shell 来解析命令行既不可能也没必要 —— 要验的是
    /// 「转义对不对 + 回退判定对不对」,不是 shell 的实现。
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.probe.lock().unwrap().execs.push(data.to_vec());
        if !self.allow_exec {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let code = match parse_rm_rf(data) {
            Some(paths) => {
                let mut tree = self.tree.lock().unwrap();
                for p in paths {
                    remove_recursively(&mut tree, &p);
                }
                0
            }
            // 认不出来的命令 —— 与真 shell 的 "command not found" 同码。
            None => 127,
        };
        session.exit_status_request(channel, code)?;
        session.close(channel)?;
        Ok(())
    }
}

/// 认 `rm -rf -- '<路径>' '<路径>'…`,把单引号字面量解回原始字节。
/// 认不出来返回 `None` —— 在测试里就是「命令没跑成」,正好扎住
/// 「转义漏了导致命令行结构不对」这一类错。
#[allow(dead_code)]
fn parse_rm_rf(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let prefix = b"rm -rf -- ";
    if !cmd.starts_with(prefix) {
        return None;
    }
    let mut rest = &cmd[prefix.len()..];
    let mut out = Vec::new();
    while !rest.is_empty() {
        if rest[0] == b' ' {
            rest = &rest[1..];
            continue;
        }
        if rest[0] != b'\'' {
            // 参数没被引号包住 —— 转义漏了,这正是我们要抓的那个 bug。
            return None;
        }
        let mut arg = Vec::new();
        let mut i = 1;
        loop {
            if i >= rest.len() {
                return None; // 引号没闭合
            }
            if rest[i] == b'\'' {
                // `'\''` = 闭合 + 一个转义的单引号 + 重新开启
                if rest[i..].starts_with(b"'\\''") {
                    arg.push(b'\'');
                    i += 4;
                    continue;
                }
                i += 1;
                break;
            }
            arg.push(rest[i]);
            i += 1;
        }
        out.push(arg);
        rest = &rest[i..];
    }
    Some(out)
}

/// 在内存树里递归删掉一条(目录连同整棵子树)。
///
/// **不跟随符号链接**:只按树上的目录键往下走,链接节点没有自己的目录键,
/// 于是天然停在那里 —— 与真实 `rm -rf` 的行为一致。
#[allow(dead_code)]
fn remove_recursively(tree: &mut sftp_server::Tree, path: &[u8]) {
    let children: Vec<Vec<u8>> = tree
        .get(path)
        .map(|v| v.iter().map(|n| n.name.clone()).collect())
        .unwrap_or_default();
    for name in children {
        let mut child = path.to_vec();
        if !child.ends_with(b"/") {
            child.push(b'/');
        }
        child.extend_from_slice(&name);
        remove_recursively(tree, &child);
    }
    tree.remove(path);
    let (dir, name) = sftp_server::split_last_pub(path);
    if let Some(v) = tree.get_mut(&dir) {
        v.retain(|n| n.name != name);
    }
}

/// 起一个带 SFTP 的假 sshd。返回监听地址、探针、以及**可写的内存树**
/// (测试用树断言「远端到底变成什么样」,用探针断言「发了哪些请求」)。
#[allow(dead_code)]
pub async fn spawn_sftp_server(
    tree: sftp_server::Tree,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
    Arc<std::sync::Mutex<sftp_server::Tree>>,
) {
    spawn_sftp_server_with(tree, true).await
}

/// 像 sftp-only 账号那样**拒绝 exec** 的变体(F57 回退分支的测试用)。
#[allow(dead_code)]
pub async fn spawn_sftp_server_without_exec(
    tree: sftp_server::Tree,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
    Arc<std::sync::Mutex<sftp_server::Tree>>,
) {
    spawn_sftp_server_with(tree, false).await
}

#[allow(dead_code)]
async fn spawn_sftp_server_with(
    tree: sftp_server::Tree,
    allow_exec: bool,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
    Arc<std::sync::Mutex<sftp_server::Tree>>,
) {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let tree = Arc::new(std::sync::Mutex::new(tree));
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
                allow_exec,
            };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, probe, tree)
}
