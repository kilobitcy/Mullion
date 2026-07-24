//! 连接参数与认证方式(F1)。只是数据,不含 UI/pane 概念。

use std::path::PathBuf;

/// F1 三种认证。
#[derive(Debug, Clone)]
pub enum AuthMethod {
    /// 密码认证。
    Password(String),
    /// 公钥认证:本地私钥文件(如 ~/.ssh/id_ed25519)+ 可选 passphrase。
    PublicKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// ssh-agent 认证(从 SSH_AUTH_SOCK 取身份)。
    Agent,
}

/// 一次连接所需的全部参数。app 构造后交给 `session::connect`。
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    /// 初始 PTY 尺寸;reflow 后由 `SshSession::resize` 同步(F34)。
    pub cols: u16,
    pub rows: u16,
    /// TERM 名,固定 "xterm-256color"。
    pub term: String,
}
