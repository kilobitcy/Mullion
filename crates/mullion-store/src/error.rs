//! store 的错误。手写 Display + std::error::Error,匹配项目既有 `ConnectError` 风格(不引 thiserror)。

use std::fmt;

use crate::credential::CredentialId;
use crate::model::{GroupId, SessionId};
use crate::tunnel::TunnelId;

#[derive(Debug)]
pub enum StoreError {
    /// 文件读写失败。
    Io(String),
    /// TOML 序列化失败。
    TomlSer(String),
    /// TOML 解析失败(文件被手改坏)。
    TomlDe(String),
    /// 加解密失败:密钥错误或密文被篡改/损坏。
    Crypto,
    /// Argon2id 派生失败(参数非法 / 盐为空)。
    Kdf(String),
    /// `secrets.enc` 的文件头读不懂:被截断 / KDF 或盐长本版本不认。
    /// **与 `Crypto` 分开**:这条说的是「结构不对」,那条说的是「钥匙不对」。
    CorruptSecrets(String),
    /// 主密码**已经改好并落盘**了,但 `cloud.toml` 里的 AK/SK 没能用新密钥重封。
    ///
    /// **与别的改密码失败严格分开**:那些说的是「密码没改成」,这条说的是
    /// 「密码改成了,云端备份的密钥掉队了」—— 用户的下一步完全不同。
    /// 报成通用失败的话,用户会重试改密码,而第二次 `take_cloud_secret_plain`
    /// 拿新密钥去解旧密文,解不开被 `.ok()` 吞成 `None`,重封从此再也不会发生;
    /// 他还会以为密码仍是旧的,下次启动输旧密码打不开,以为整个库坏了。
    CloudReseal(String),
    /// `secrets.enc` 由主密码加密,但调用方没给密码。
    PasswordRequired,
    /// 主密码不对。**与 `Crypto` 分开**:用户的下一步动作完全不同
    /// (重打一遍 vs 从备份恢复)。
    WrongPassword,
    /// keyring 里的主密钥长度非 32。
    CorruptKey,
    /// OS keyring 访问失败(缺 Secret Service 等)。
    Keyring(String),
    /// secrets.enc 解密后非法 UTF-8。
    Utf8,
    /// 目标会话不存在。
    NotFound(SessionId),
    /// 目标分组不存在。
    GroupNotFound(GroupId),
    /// 目标隧道不存在。
    TunnelNotFound(TunnelId),
    /// 文件由更新版本的客户端写出,本版本读不了。
    UnsupportedSchema(u32),
    /// v1 → v2 迁移失败(结构不兼容,非语法问题)。
    Migration(String),
    /// 跳板链存在环(F5)。带上参与环的会话 id 便于定位。
    JumpCycle(SessionId),
    /// 跳板链超过 `jump::MAX_JUMP_DEPTH`。
    JumpTooDeep(SessionId),
    /// 跳板引用了不存在的会话。**不静默降级为直连**:那会让用户
    /// 以为流量过了堡垒机而实际没有 —— 这是安全属性,必须硬失败。
    JumpDangling(SessionId),
    /// 隧道引用了不存在的会话。同 `JumpDangling` 的道理:静默回落到
    /// 任何一个「别的会话」都等于把端口悄悄接到另一台机器上。
    TunnelDangling(SessionId),
    /// 目标凭据不存在(F74)。
    CredentialNotFound(CredentialId),
    /// 凭据还被这些会话引用着,不能删(F74,设计 D7)。带上引用者便于 UI
    /// 直接列出「先去解绑这几条」。
    CredentialInUse(Vec<SessionId>),
    /// F46-a:迁移包的格式版本高于本客户端。**不猜着解** —— 包里装的是凭据
    /// 和会话,而导入会把整份配置替换掉。
    UnsupportedPack(u32),
    /// F46-a:迁移包的结构不对(base64 解不开 / 密文段没有口令头)。
    /// **与 `WrongPassword` 分开**:那条说「钥匙不对」,这条说「拿来的不是包」。
    CorruptPack(String),
    /// 会话引用的凭据不存在。**绝不回落到别的身份**——与 `JumpDangling`
    /// 同一条铁律:静默换一个身份去登录是安全事故(设计 D6)。
    DanglingCredential(CredentialId),
    /// F270:这个操作要求库是主密码方案,而当前是钥匙串方案。
    ///
    /// **不是「加密失败」** —— 加密本身完全能做,只是封出来的东西离不开这台
    /// 机器。把它跟 `Crypto` 混成一条的话,UI 只能说「加密失败」,而用户需要
    /// 知道的是「去设一个主密码」。
    NoMasterPassword,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "文件读写失败:{e}"),
            StoreError::TomlSer(e) => write!(f, "TOML 序列化失败:{e}"),
            StoreError::TomlDe(e) => write!(f, "TOML 解析失败(文件可能被手改坏):{e}"),
            StoreError::Crypto => write!(f, "加解密失败 —— 密钥错误或密文损坏"),
            StoreError::Kdf(e) => write!(f, "主密码派生失败:{e}"),
            StoreError::CorruptSecrets(e) => write!(f, "secrets.enc 的文件头读不懂:{e}"),
            StoreError::CloudReseal(e) => write!(
                f,
                "主密码已经改好了,但云端备份的 Access Key Secret 没能用新密钥重封:{e} —— \
                 不要重试改密码,去设置里的「云端备份」一节把 Access Key Secret 重填一遍即可"
            ),
            StoreError::PasswordRequired => {
                write!(f, "secrets.enc 由主密码加密 —— 需要先输入主密码")
            }
            StoreError::WrongPassword => write!(f, "主密码不对"),
            StoreError::CorruptKey => write!(f, "主密钥损坏(长度非 32 字节)"),
            StoreError::Keyring(e) => {
                write!(f, "系统密钥库访问失败:{e} —— 检查 keyring/Secret Service")
            }
            StoreError::Utf8 => write!(f, "secrets.enc 解密后非法 UTF-8"),
            StoreError::NotFound(id) => write!(f, "会话不存在:{id:?}"),
            StoreError::GroupNotFound(id) => write!(f, "分组不存在:{id:?}"),
            StoreError::TunnelNotFound(id) => write!(f, "隧道不存在:{id:?}"),
            StoreError::UnsupportedSchema(v) => write!(
                f,
                "会话文件的 schema 版本 {v} 高于本客户端支持的上限 —— 请升级 Mullion"
            ),
            StoreError::Migration(e) => write!(f, "会话文件迁移失败:{e}"),
            StoreError::JumpCycle(id) => {
                write!(f, "跳板链存在环,经过会话 {id:?} —— 检查该会话的跳板设置")
            }
            StoreError::JumpTooDeep(id) => write!(
                f,
                "跳板链过深(上限 {}),从会话 {id:?} 展开 —— 检查是否配错",
                crate::jump::MAX_JUMP_DEPTH
            ),
            StoreError::JumpDangling(id) => write!(
                f,
                "跳板指向的会话 {id:?} 不存在 —— 它可能已被删除,请重新指定跳板"
            ),
            StoreError::TunnelDangling(id) => write!(
                f,
                "隧道引用的会话 {id:?} 不存在 —— 它可能已被删除,请重新指定或删除此隧道"
            ),
            StoreError::UnsupportedPack(v) => write!(
                f,
                "这份迁移包是 v{v} 格式写出来的(本版本认到 v{}) —— 请先把 Mullion 升到导出它的那个版本",
                crate::portable::CURRENT_PACK_FORMAT
            ),
            StoreError::CorruptPack(e) => write!(f, "迁移包读不懂:{e}"),
            StoreError::CredentialNotFound(id) => write!(f, "凭据不存在:{id:?}"),
            StoreError::CredentialInUse(ids) => write!(
                f,
                "这份凭据还被 {} 条会话引用着 —— 请先把它们改成别的凭据再删",
                ids.len()
            ),
            StoreError::DanglingCredential(id) => write!(
                f,
                "会话引用的凭据 {id:?} 不存在 —— 它可能已被删除,请重新指定认证方式"
            ),
            StoreError::NoMasterPassword => write!(f, "这个操作需要先设置主密码"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}
impl From<toml::ser::Error> for StoreError {
    fn from(e: toml::ser::Error) -> Self {
        StoreError::TomlSer(e.to_string())
    }
}
impl From<toml::de::Error> for StoreError {
    fn from(e: toml::de::Error) -> Self {
        StoreError::TomlDe(e.to_string())
    }
}
impl From<std::string::FromUtf8Error> for StoreError {
    fn from(_: std::string::FromUtf8Error) -> Self {
        StoreError::Utf8
    }
}
impl From<keyring::Error> for StoreError {
    fn from(e: keyring::Error) -> Self {
        StoreError::Keyring(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_and_io_have_distinct_messages() {
        let a = StoreError::Crypto.to_string();
        let b = StoreError::Io("x".into()).to_string();
        assert!(!a.is_empty() && !b.is_empty());
        assert_ne!(a, b, "不同错误消息必须可区分");
    }
}
