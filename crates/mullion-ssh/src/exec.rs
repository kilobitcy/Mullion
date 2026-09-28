//! 在**已建立**的连接上跑一条一次性命令(F57 的 `rm -rf` 快路径)。
//!
//! 与 `session::open_pty` / `sftp::SftpClient::open` 同一条防呆:签名里没有
//! 任何网络参数,想在这里偷偷重连一次都做不到。
//!
//! **不请求 PTY**:这是批处理命令,不是交互 shell。请求了的后果是远端白白
//! 起一个伪终端、`who` 里多一行幽灵会话,而且 `PermitTTY no` 的账号会直接被拒。

use std::sync::Arc;
use std::time::Duration;

use crate::session::SshConnection;

/// 一条命令跑完的结果。
#[derive(Debug)]
pub struct ExecOutcome {
    /// 远端的退出码。**`None` 表示对端没送 `exit-status`** —— 那不等于成功,
    /// 调用方(F57 的回退判定)必须把它当失败处理,见 [`ExecOutcome::succeeded`]。
    pub exit_status: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutcome {
    /// 命令是不是干净地成功了。**`exit_status == None` 算失败**(见该字段文档)。
    pub fn succeeded(&self) -> bool {
        self.exit_status == Some(0)
    }
}

#[derive(Debug)]
pub enum ExecError {
    /// 开 channel 失败(连接多半已断)。
    Channel,
    /// 对端**拒绝**执行命令。sftp-only 账号(`ForceCommand internal-sftp` +
    /// `ChrootDirectory`)就是这一类 —— F57 靠它决定回退到逐文件递归删除。
    Rejected,
    /// F298:`exec_with_timeout` 的预算到点、命令还没跑完。**channel 已经在
    /// 这个分支里显式关掉了**(见 `exec_with_timeout` 内部),不是「调用方
    /// 自己把 future 一丢了事」——那样做会因为 `Channel<Msg>` 没有会发
    /// CHANNEL_CLOSE 的 `Drop` 而永久泄漏一个 sshd `MaxSessions` 槽位。
    Timeout,
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Channel => write!(f, "无法开启命令通道,连接可能已断开"),
            ExecError::Rejected => write!(f, "远端拒绝执行命令(sftp-only 账号会这样)"),
            ExecError::Timeout => write!(f, "命令执行超时"),
        }
    }
}

impl std::error::Error for ExecError {}

/// 把一段**字节**包成 shell 单引号字面量。
///
/// 规则只有一条:用 `'` 包住,内部的每个 `'` 换成 `'\''`(闭合、转义一个
/// 单引号、再开启)。单引号内 POSIX shell **不做任何解释** —— `$`、反引号、
/// `\`、换行、空格、`*` 全是字面量。这是唯一不需要枚举元字符的正确写法;
/// 任何「把危险字符列出来逐个转义」的实现,漏一个就是远端任意命令执行。
///
/// 返回字节而不是 `String`:`russh` 的 `Channel::exec` 收 `Into<Vec<u8>>`,
/// 而路径本来就是字节。中间过一趟 `String` 等于给非 UTF-8 路径设一道
/// 本不必要的门槛。
pub fn shell_quote(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'\'');
    for b in bytes {
        if *b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(*b);
        }
    }
    out.push(b'\'');
    out
}

/// 在已建立的连接上跑一条命令,读完 stdout/stderr 与退出码再返回。
///
/// **只用于短命令**(`rm -rf` 这类):全部输出攒在内存里,不做流式。**没有
/// 超时**——想要超时用 [`exec_with_timeout`],**不要**在调用点外面自己套
/// `tokio::time::timeout(d, exec(..))`:那样命令一超时,`timeout` 就把整个
/// `exec()` 的 future(连同它手里已经开好的 `channel`)一起丢弃,而
/// `Channel<Msg>` 没有会发 CHANNEL_CLOSE 的 `Drop`——sshd 那边的
/// `MaxSessions` 槽位永久占着不还,客户端账本(`ledger`)却认为已经释放了
/// (F298 复核踩中的真 bug:`_slot` guard 确实会被 drop、计数会减,但那只是
/// **我们自己的**账本,不代表 sshd 收到了 CHANNEL_CLOSE)。
pub async fn exec(conn: &Arc<SshConnection>, command: Vec<u8>) -> Result<ExecOutcome, ExecError> {
    exec_impl(conn, command, None).await
}

/// 同 [`exec`],但接收循环有一个**从发完命令起算的总预算**——超时时**在这个
/// 函数内部**显式 `channel.close().await` 再返回 `Err(ExecError::Timeout)`,
/// 不依赖调用方外层的 `Drop`(见 [`exec`] 的文档,那条路会泄漏 channel)。
///
/// 预算是「总量」不是「相邻两条消息的间隔」:每轮用「距 deadline 还剩多少」
/// 去限时下一次 `channel.wait()`,慢慢吐数据的命令不会靠每条消息都落在
/// 预算内就无限续命。
pub async fn exec_with_timeout(
    conn: &Arc<SshConnection>,
    command: Vec<u8>,
    timeout: Duration,
) -> Result<ExecOutcome, ExecError> {
    exec_impl(conn, command, Some(timeout)).await
}

/// 两个公开入口共用的实现,唯一活的循环——防止「加超时」和「不加超时」
/// 变成两份要同步维护的消息处理逻辑。
async fn exec_impl(
    conn: &Arc<SshConnection>,
    command: Vec<u8>,
    timeout: Option<Duration>,
) -> Result<ExecOutcome, ExecError> {
    use russh::ChannelMsg;

    let mut channel = conn
        .handle()
        .channel_open_session()
        .await
        .map_err(|_| ExecError::Channel)?;
    // F254:exec 用的也是**会话**类型的 channel,一样占 sshd 的 `MaxSessions`
    // 槽位,所以一样要上账 —— 不记的话「这条连接持有 N 条」在跑着 F57 删除
    // 之类的命令时会偏小,而那正是分屏开不出来的时刻。
    //
    // guard 就放在栈上:这个函数是「跑完一条命令再返回」,下面每个 `return`
    // 与函数正常结束都会 drop 它,不会漏。**它只管我们自己的账本**——真正
    // 让 sshd 那边的槽位归还的是 `channel.close().await`,两者是两回事
    // (见 `exec` 的文档)。
    let _slot = conn.ledger().check_out();
    // `want_reply = true` 是必须的:回执才是 F57 判定「该回退了」的信号。
    // 设 false 的话拒绝是静默的,我们会误以为命令跑了而且成功了。
    //
    // **`Channel::exec` 是发完就返回的**(russh 0.54.5:内部只是往 sender
    // 塞一条 `ChannelMsg::Exec`,不等回执)。所以这里的 `Err` 只代表
    // 「连 send 都失败了」= 连接已断,而**对端的拒绝是下面循环里的
    // `ChannelMsg::Failure`**。把拒绝当成 `exec()` 的返回值来判,会在
    // sftp-only 账号上死等到天荒地老 —— 服务端回了 failure 就不再说话,
    // 而 `wait()` 要等到 channel 关闭才结束。这一步本身近乎瞬时(不等回执),
    // 不纳入下面的超时预算。
    if channel.exec(true, command).await.is_err() {
        // `Channel<Msg>` 没有自动发 CHANNEL_CLOSE 的 Drop,不显式关就是
        // 泄漏一个 channel slot(同 `SftpClient::open` 那条注释)。
        let _ = channel.close().await;
        return Err(ExecError::Channel);
    }

    // 预算从这里、命令已经发出去之后起算——deadline 是「发完命令到收完结果」
    // 这一段的总时长,不包含上面开 channel / 发 exec 请求那点近乎瞬时的开销。
    let deadline = timeout.map(|d| tokio::time::Instant::now() + d);

    let mut out = ExecOutcome {
        exit_status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    loop {
        let msg = match deadline {
            None => channel.wait().await,
            Some(dl) => {
                let remaining = dl.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(remaining, channel.wait()).await {
                    Ok(m) => m,
                    Err(_) => {
                        // 预算到点、命令还没跑完。**channel 此刻还活着**——
                        // 上面 `tokio::time::timeout` 只是放弃了这一次
                        // `channel.wait()` 的 future,`channel` 本身没被
                        // drop,在这里显式关掉才是让 sshd 收到 CHANNEL_CLOSE
                        // 的唯一办法(同上面两处「泄漏 channel slot」的注释)。
                        let _ = channel.close().await;
                        return Err(ExecError::Timeout);
                    }
                }
            }
        };
        let Some(msg) = msg else { break };
        match msg {
            // 对端拒绝执行命令。**立刻关掉 channel 再返回** —— 服务端此后
            // 不会再说一个字,不主动关就永远卡在 `wait()` 上。
            ChannelMsg::Failure => {
                let _ = channel.close().await;
                return Err(ExecError::Rejected);
            }
            ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => out.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => out.exit_status = Some(exit_status),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F57 的路径转义。逐条覆盖 spec 点名的五类:空格、引号、换行、`$`、反引号。
    ///
    /// 判据是「包起来之后,shell 解出来的还是原串」。这里不能真起一个 shell,
    /// 所以退一步验结构:外层一对单引号,内部除了 `'` 的转义序列之外**逐字节
    /// 原样**。真 shell 那一半由 `sftp_write.rs` 的端到端测试补上
    /// (假服务端会把命令行解回来跟原路径比)。
    #[test]
    fn quoting_neutralises_every_shell_metacharacter() {
        for raw in [
            &b"a b.txt"[..],
            &b"it's here"[..],
            &b"line1\nline2"[..],
            &b"$HOME"[..],
            &b"`whoami`"[..],
            &b"a*b?c[d]"[..],
            &b"back\\slash"[..],
            "中文 名.txt".as_bytes(),
        ] {
            let q = shell_quote(raw);
            assert_eq!(q.first(), Some(&b'\''), "必须以单引号开头: {q:?}");
            assert_eq!(q.last(), Some(&b'\''), "必须以单引号结尾: {q:?}");
            // 反解:去掉外层引号,把 `'\''` 还原成 `'`,应当逐字节等于原串。
            let inner = &q[1..q.len() - 1];
            let restored = String::from_utf8_lossy(inner).replace("'\\''", "'");
            assert_eq!(
                restored.as_bytes(),
                String::from_utf8_lossy(raw).as_bytes(),
                "反解回来的串必须与原串逐字节相同"
            );
        }
    }

    /// 单引号是**唯一**需要特殊处理的字符。这条钉死的是「别去枚举元字符」:
    /// 换成一个「把 `$`/反引号/`\` 挨个加反斜杠」的实现,这条必然变红 ——
    /// 那种实现在单引号内会把 `\$` 原样留下,shell 解出来就多了个反斜杠。
    #[test]
    fn a_single_quote_is_the_only_character_that_gets_rewritten() {
        assert_eq!(shell_quote(b"$`\\*"), b"'$`\\*'".to_vec());
        assert_eq!(shell_quote(b"a'b"), b"'a'\\''b'".to_vec());
    }

    /// 空串也要包成一对引号 —— 裸的空串在命令行里等于「这个参数不存在」,
    /// `rm -rf` 的参数凭空少一个,后果是删错东西。
    #[test]
    fn an_empty_path_still_produces_a_quoted_empty_argument() {
        assert_eq!(shell_quote(b""), b"''".to_vec());
    }

    /// `exit_status == None`(对端没送退出码)**不算成功**。这条守的是
    /// F57 的回退判定:算成功的话,一条根本没跑起来的 `rm -rf` 会被当成
    /// 「删干净了」,界面刷新后文件还在,用户完全不知道发生了什么。
    #[test]
    fn a_missing_exit_status_is_not_success() {
        let mk = |s| ExecOutcome {
            exit_status: s,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(!mk(None).succeeded(), "没收到退出码不算成功");
        assert!(mk(Some(0)).succeeded());
        assert!(!mk(Some(1)).succeeded());
    }
}
