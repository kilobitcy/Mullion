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
    /// `true` = 底下的 `SftpHandler` 一律拒绝 `rename`(模拟 EXDEV)。
    /// F220/B3 缺口 2:没有这个开关,`copy_tree.rs` 里「rename 失败 →
    /// 拷贝+删源」那条分支在测试里永远走不到(假服务端的 rename 从不失败)。
    reject_rename: bool,
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
                sftp_server::SftpHandler::new_with_rename_policy(
                    self.tree.clone(),
                    self.probe.clone(),
                    self.reject_rename,
                ),
            )
            .await;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    /// 记下命令行;`allow_exec == false` 时直接拒(见该字段的文档)。
    ///
    /// 允许执行时认三种形状:`rm -rf -- <路径…>`(F57);`cp -a[f] --`/
    /// `mv [-f] --`(F220 非覆盖,单对,或用 ` && ` 串起来的多对);以及
    /// `rm -rf -- <dst> && cp -a -- <src> <dst>`(或 `mv`)这种「先清目标
    /// 再放」的覆盖形状(F220,B3 修正——`cp -a`/`mv` 撞上已存在的目标
    /// 目录时是嵌进去而不是替换,`-f` 救不了这一种,见 `copy_tree.rs` 模块
    /// 文档),并真的在内存树上执行。起一个真 shell 来解析命令行既不可能
    /// 也没必要 —— 要验的是「转义对不对 + 回退判定对不对」,不是 shell
    /// 的实现。
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
            None => match parse_copy_or_move(data) {
                Some((is_move, pairs)) => {
                    let mut tree = self.tree.lock().unwrap();
                    for (from, to) in pairs {
                        copy_recursively(&mut tree, &from, &to);
                        if is_move {
                            remove_recursively(&mut tree, &from);
                        }
                    }
                    0
                }
                None => match parse_overwriting_copy_or_move(data) {
                    Some((is_move, pairs)) => {
                        let mut tree = self.tree.lock().unwrap();
                        for (from, to) in pairs {
                            // 先清目标再放,与 `rm -rf -- <dst> && cp/mv …`
                            // 这条命令自己说的语义一致。
                            remove_recursively(&mut tree, &to);
                            copy_recursively(&mut tree, &from, &to);
                            if is_move {
                                remove_recursively(&mut tree, &from);
                            }
                        }
                        0
                    }
                    // 认不出来的命令 —— 与真 shell 的 "command not found" 同码。
                    None => 127,
                },
            },
        };
        session.exit_status_request(channel, code)?;
        session.close(channel)?;
        Ok(())
    }
}

/// 解一串 `'a' 'b' 'c'` 形式的单引号参数,把单引号字面量解回原始字节。
/// 认不出来返回 `None` —— 在测试里就是「命令没跑成」,正好扎住
/// 「转义漏了导致命令行结构不对」这一类错。
#[allow(dead_code)]
fn parse_quoted_args(mut rest: &[u8]) -> Option<Vec<Vec<u8>>> {
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

/// 认 `rm -rf -- '<路径>' '<路径>'…`。
#[allow(dead_code)]
fn parse_rm_rf(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let prefix = b"rm -rf -- ";
    parse_quoted_args(cmd.strip_prefix(prefix)?)
}

/// 按字节串分隔符切分 `haystack`(不做转义感知 —— 调用方要保证分隔符不会
/// 出现在被引号包住的内容里;`shell_quote` 保证了这一点,见调用点注释)。
#[allow(dead_code)]
fn split_on<'a>(haystack: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = haystack;
    while let Some(pos) = rest.windows(sep.len()).position(|w| w == sep) {
        out.push(&rest[..pos]);
        rest = &rest[pos + sep.len()..];
    }
    out.push(rest);
    out
}

/// 依次试每个前缀,命中就剥掉返回剩余部分;都不命中给 `None`。
#[allow(dead_code)]
fn strip_any<'a>(s: &'a [u8], prefixes: &[&[u8]]) -> Option<&'a [u8]> {
    prefixes.iter().find_map(|p| s.strip_prefix(*p))
}

/// F220:认 `cp -a[f] -- '<src>' '<dst>'`(单对,多对用 ` && ` 串),
/// 以及同形状的 `mv`。返回(是不是移动、一串 (src, dst))。
///
/// 起一个真 shell 来解析命令行既不可能也没必要 —— 要验的是「转义对不对 +
/// 回退判定对不对」,不是 shell 的实现(同 `parse_rm_rf` 的理由)。
/// `pub(crate)`(而不是私有):B2 的守护测试要在 `sftp_write.rs` 里直接拿
/// 真实的 `shell_quote` 拼一条命令,断言这个解析器认得出来 —— 不然
/// `parse_copy_or_move` 认不出 B3 真实发的命令这件事,要等 B3 落地才会
/// 被测试撞见(而且撞见的方式是「静默回退到 SFTP,exec 快路径从没被验过」)。
#[allow(dead_code, clippy::type_complexity)]
pub(crate) fn parse_copy_or_move(cmd: &[u8]) -> Option<(bool, Vec<(Vec<u8>, Vec<u8>)>)> {
    let mut is_move = None;
    let mut out = Vec::new();
    // ` && ` 分段,**按字节找、不感知引号**。已知的失真:`shell_quote` 只
    // 转义单引号本身,不转义空格和 `&`,所以文件名字面就含 ` && ` 时(真 shell
    // 照样跑得好好的)这里会从引号中间切开、解析失败 → 假服务端回 127。
    // 落到 B3 就是「以为对端没有 cp」的**假红**,不是假绿。现在不修:引号
    // 感知的切法没有任何测试扎得住(没有调用方产得出这种输入),补了也是
    // 一段没人验的代码;等真撞上再连着守护一起加。
    for seg in split_on(cmd, b" && ") {
        let (mv, rest) = if let Some(r) = strip_any(seg, &[b"mv -f -- ", b"mv -- "]) {
            (true, r)
        } else if let Some(r) = strip_any(seg, &[b"cp -af -- ", b"cp -a -- "]) {
            (false, r)
        } else {
            return None;
        };
        if *is_move.get_or_insert(mv) != mv {
            return None; // 一条命令里混着 cp 和 mv —— 实现出错了
        }
        let args = parse_quoted_args(rest)?;
        if args.len() != 2 {
            return None;
        }
        out.push((args[0].clone(), args[1].clone()));
    }
    Some((is_move?, out))
}

/// F220/B3:认覆盖形状 —— `rm -rf -- '<dst>' && cp -a -- '<src>' '<dst>'`
/// (或同形的 `mv`),多对是同样两段一组、彼此再用 ` && ` 串起来。
///
/// 存在的理由:`cp -a src dst` / `mv src dst` 撞上一个已存在的目标目录时
/// 是把 src 拷成 `dst/basename(src)`(嵌进去),不是替换 `dst`——`-f` 只
/// 救得了「目标是已存在的文件」这一种。B3 的 `try_exec` 因此在
/// `overwrite == true` 时不再走单纯的 `cp -af`/`mv -f`,而是先删目标、
/// 再放,与 SFTP 回退路径(先 `remove_tree` 再拷)语义对齐。这里没有
/// 复用 `parse_copy_or_move`:那个函数的一段对一对 `(src, dst)`,而这里
/// 是两段对一对,混在一起解析容易两头都解错还看不出来,分开更看得清楚。
///
/// 每组第二段(cp/mv)的目标路径必须与第一段(rm)删的路径逐字节相同——
/// 不同就说明拼错了,不认,回 127(与 `parse_copy_or_move` 认不出的命令
/// 同样的失败方式)。
#[allow(dead_code, clippy::type_complexity)]
fn parse_overwriting_copy_or_move(cmd: &[u8]) -> Option<(bool, Vec<(Vec<u8>, Vec<u8>)>)> {
    let segs = split_on(cmd, b" && ");
    if segs.is_empty() || !segs.len().is_multiple_of(2) {
        return None;
    }
    let mut is_move = None;
    let mut out = Vec::new();
    for pair in segs.chunks_exact(2) {
        let rm_rest = pair[0].strip_prefix(b"rm -rf -- ")?;
        let rm_args = parse_quoted_args(rm_rest)?;
        if rm_args.len() != 1 {
            return None;
        }
        let (mv, rest) = if let Some(r) = strip_any(pair[1], &[b"mv -f -- ", b"mv -- "]) {
            (true, r)
        } else if let Some(r) = strip_any(pair[1], &[b"cp -af -- ", b"cp -a -- "]) {
            (false, r)
        } else {
            return None;
        };
        if *is_move.get_or_insert(mv) != mv {
            return None; // 一条命令里混着 cp 和 mv —— 实现出错了
        }
        let args = parse_quoted_args(rest)?;
        if args.len() != 2 {
            return None;
        }
        if args[1] != rm_args[0] {
            // rm 删的和 cp/mv 写的目标不是同一条路径 —— 拼错了,不认。
            return None;
        }
        out.push((args[0].clone(), args[1].clone()));
    }
    Some((is_move?, out))
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

/// 在内存树里把一条(文件、链接或整棵目录树)拷到新路径。
/// **不跟随符号链接**:链接节点原样复制(连同它的目标字符串)。
///
/// 镜像 `remove_recursively` 的结构(它是同一套树操作的反向):先在源的
/// 父目录里找到节点,找不到就什么都不做;否则克隆一份、名字换成 `to`
/// 的末段,插进 `to` 的父目录;源是目录的话再 `tree.insert(to, vec![])`
/// 建出目标这一层的目录键,并对每个孩子递归。父目录/名字的切法用
/// `sftp_server::split_last_pub` —— 自己再写一遍切法就会两边不一致。
///
/// **目标已存在同名节点时,这个函数自己不做任何清理**:实测会在目标父
/// 目录里 push 出同名双节点(而 `exists()` 是 `.any()` 查找,看不见双
/// 节点),目标是目录时还会被 `tree.insert(to, vec![])` 把原有内容整个
/// 清空。B3 落地后已经把这一层责任挪到了调用方:`exec_request` 在覆盖
/// 形状(`rm -rf -- <dst> && cp/mv …`,见 `parse_overwriting_copy_or_move`)
/// 下,会在调用这个函数**之前**先 `remove_recursively(&mut tree, &to)`
/// 把目标清空,所以调用到这里时 `to` 已经不存在同名节点了 —— 这个函数
/// 本身仍然不处理「目标已存在」这种输入,谁调用谁负责先清场。
///
/// `pub(crate)`:B2 的守护测试要在 `sftp_write.rs` 里直接对内存树验它的
/// 树操作(目录树 + 符号链接不跟随),不必等 B3 的协议层落地。
#[allow(dead_code)]
pub(crate) fn copy_recursively(tree: &mut sftp_server::Tree, from: &[u8], to: &[u8]) {
    let (from_dir, from_name) = sftp_server::split_last_pub(from);
    let Some(node) = tree
        .get(&from_dir)
        .and_then(|v| v.iter().find(|n| n.name == from_name))
        .cloned()
    else {
        return;
    };
    let is_dir = node.kind == sftp_server::NodeKind::Dir;

    let (to_dir, to_name) = sftp_server::split_last_pub(to);
    let mut cloned = node;
    cloned.name = to_name;
    tree.entry(to_dir).or_default().push(cloned);

    if is_dir {
        let children: Vec<Vec<u8>> = tree
            .get(from)
            .map(|v| v.iter().map(|n| n.name.clone()).collect())
            .unwrap_or_default();
        tree.insert(to.to_vec(), Vec::new());
        for name in children {
            let mut child_from = from.to_vec();
            if !child_from.ends_with(b"/") {
                child_from.push(b'/');
            }
            child_from.extend_from_slice(&name);
            let mut child_to = to.to_vec();
            if !child_to.ends_with(b"/") {
                child_to.push(b'/');
            }
            child_to.extend_from_slice(&name);
            copy_recursively(tree, &child_from, &child_to);
        }
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
    spawn_sftp_server_with(tree, true, false).await
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
    spawn_sftp_server_with(tree, false, false).await
}

/// **拒绝 exec 且 `rename` 一律失败**(模拟 EXDEV)的变体。F220/B3 缺口 2:
/// `copy_tree.rs` 的 SFTP 回退路径里「rename 失败 → 拷贝+删源」那条分支,
/// 靠这个变体逼 `sftp.rename` 报错才走得到 —— 单开 `allow_exec=false`
/// 不够,普通假服务端的 rename 从不失败。
#[allow(dead_code)]
pub async fn spawn_sftp_server_without_exec_and_rename(
    tree: sftp_server::Tree,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
    Arc<std::sync::Mutex<sftp_server::Tree>>,
) {
    spawn_sftp_server_with(tree, false, true).await
}

#[allow(dead_code)]
async fn spawn_sftp_server_with(
    tree: sftp_server::Tree,
    allow_exec: bool,
    reject_rename: bool,
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
                reject_rename,
            };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, probe, tree)
}
