//! B3:sftp 失败的**分类**。判据是「界面该怎么反应」,不是「错误码是什么」——
//! 所以它住在 `mullion-app` 而不是 `mullion-ssh`。
//!
//! 两类:
//! - `Session` —— 连接/通道级。整条链路没了,当前目录这个概念都不成立,
//!   面板要转断开态、给用户一个回到可用状态的入口
//! - `Path` —— 路径级。链路好好的,只是这一个路径动不了(没权限/不存在),
//!   面板停在原地报一句就行
//!
//! **分类拿字符串匹配是权宜**:`mullion_ssh::sftp` 目前把错误压成了
//! `String`(见 `app.rs::spawn_sftp_list_dir` 的 `map_err`)。哪天那边给出
//! 结构化错误类型,这里要改成按类型分类 —— 字符串匹配对服务端换措辞
//! 是脆的,下面这组测试覆盖的是**当前**真实文案。
//!
//! 真实文案是怎么核实的(而不是拍脑袋编的):
//! - 路径级:`app.rs` 里 `spawn_sftp_list_dir` 用 `format!("读取目录失败:{e}")`
//!   包一层,`e` 来自 `mullion_ssh::sftp::SftpError::Protocol`,它的 `Display`
//!   直接透传 `russh_sftp::client::error::Error` 的文本。协议层的
//!   `Status(status_code, error_message)` 打印成 `"{status_code}: {error_message}"`
//!   ——本项目自己的假 sftp 服务端(`crates/mullion-ssh/tests/common/sftp_server.rs`)
//!   只传裸 `StatusCode`,`error_message` 按 `russh_sftp` 的约定回退成
//!   `status_code.to_string()`,于是真实落地是「No such file: No such file」
//!   「Permission denied: Permission denied」这种「同一句话说两遍」的形状。
//! - 连接级:`russh-sftp` 2.4.0(`Cargo.lock` 锁定版本;`0.54.5` 是 `russh`
//!   自己的版本号,别查混了)依赖树里,`SftpSession` 的后台读循环
//!   一断线就退出并 `cancel()` 掉，等待中的请求随之在
//!   `tokio::sync::oneshot::Receiver` 上收到 `RecvError`(其 `Display` 固定
//!   是 `"channel closed"`),外层 `client::error::Error::UnexpectedBehavior`
//!   把它包成 `"RecvError: channel closed"`;真实的 TCP 复位/管道破裂会经
//!   `client::error::Error::IO(String)`(`Display` = `"I/O: {0}"`)把
//!   `std::io::Error` 的操作系统原文("Connection reset by peer (os error
//!   104)"这类)透传出来;协议层还有一条 `UnexpectedEof`(`Display` =
//!   "Unexpected EOF on stream"),对应 `io::ErrorKind::UnexpectedEof`。
//!   三条分别在 `connection_level_failures_are_session_kind` 里当真实样本用。

/// 一次 sftp 失败该让界面怎么反应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 连接/通道级 —— 面板转断开态。
    Session,
    /// 路径级 —— 停在原地报错。
    Path,
}

/// 命中任意一个就判定为连接级。**只加不减需要证据**——每条都对应上面文档
/// 注释里核实过的某个真实 `Display` 输出,不是拍脑袋列的猜测词表。
const SESSION_MARKERS: &[&str] = &[
    "channel",
    "Channel",
    "connection",
    "Connection",
    "disconnect",
    "Disconnect",
    "EOF",
    "eof",
    "closed",
    "broken pipe",
    "not connected",
    "session",
];

/// 把一条已格式化的错误文案分类。
///
/// **默认落 `Path`**:分错方向的代价不对称 —— 把路径错误误判成断开,会
/// 让用户对着一个好好的连接点「重连」(白等一次拨号);把断开误判成路径
/// 错误,只是少一个重连按钮、用户还能自己关标签重开。前者更烦人,所以
/// 只有**明确**认得出的连接级关键词才判 `Session`。
pub fn classify(msg: &str) -> FailKind {
    if SESSION_MARKERS.iter().any(|marker| msg.contains(marker)) {
        FailKind::Session
    } else {
        FailKind::Path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连接级失败必须判成 `Session` —— 否则用户在一条死链路上点目录,
    /// 只会看见一串底层错误,没有任何回到可用状态的入口。
    ///
    /// 三条文案都能在文档注释里追到出处,不是编的:
    /// - `RecvError: channel closed`——请求在途时读循环因断线退出,
    ///   等待中的 oneshot 收到 tokio 的 `RecvError`(其 `Display` 固定是
    ///   `"channel closed"`)。
    /// - `I/O: Connection reset by peer (os error 104)`——底层 TCP 被复位,
    ///   `client::error::Error::IO` 透传 `std::io::Error` 的系统原文。
    /// - `Unexpected EOF on stream`——`io::ErrorKind::UnexpectedEof` 对应的
    ///   固定文案。
    ///
    /// 自证会变红:把 `classify` 改成恒返回 `FailKind::Path`。
    #[test]
    fn connection_level_failures_are_session_kind() {
        for msg in [
            "读取目录失败:RecvError: channel closed",
            "读取目录失败:I/O: Connection reset by peer (os error 104)",
            "读取目录失败:Unexpected EOF on stream",
        ] {
            assert_eq!(classify(msg), FailKind::Session, "该判成连接级:{msg}");
        }
    }

    /// 路径级失败必须判成 `Path` —— 否则点一个没权限的目录会把整栏
    /// 打成「连接已断开」,而连接好好的。
    ///
    /// 前两条是本项目假 sftp 服务端(`tests/common/sftp_server.rs`)真实会
    /// 吐出的形状:裸 `StatusCode` 没带自定义 `error_message`,`russh_sftp`
    /// 按约定回退成 `status_code.to_string()`,于是状态码文本被打印了两遍。
    /// 第三条是协议层「包解析不对」的兜底分支,同样与连接死活无关。
    ///
    /// 自证会变红:把 `classify` 改成恒返回 `FailKind::Session`。
    #[test]
    fn path_level_failures_are_path_kind() {
        for msg in [
            "读取目录失败:No such file: No such file",
            "读取目录失败:Permission denied: Permission denied",
            "读取目录失败:Unexpected packet",
        ] {
            assert_eq!(classify(msg), FailKind::Path, "该判成路径级:{msg}");
        }
    }
}
