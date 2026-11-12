//! Kotlin 侧会收到 `FfiException`。`flat_error`:Kotlin 拿到的是 `Display` 文案,
//! 所以每条文案都要指一条出路。

use mullion_store::StoreError;

#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    /// 握手 / 认证 / 开 PTY 失败。
    Connect(String),
    /// 出站队列满(大段粘贴 + 慢链路)。
    Busy,
    /// channel 已关。
    Closed,
    /// F346-a:包口令不对。
    WrongPassword,
    /// F346-a:包的格式或 schema 版本比手机端新。桌面与手机版本号独立,
    /// 手机落后一版是**常态**,文案要叫人升级而不是说「坏了」。
    NeedsNewerApp(u32),
    /// F346-a:包损坏 / 不是 Mullion 的包。
    BadPack(String),
    /// F346-a:写手机本地数据失败。
    Storage(String),
    /// 调用方(Kotlin)传错了参数,例如数据密钥长度。
    Internal(String),
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "连接失败:{e}"),
            Self::Busy => write!(f, "发送队列已满,稍后再试"),
            Self::Closed => write!(f, "连接已关闭"),
            Self::WrongPassword => write!(f, "口令不对,再输一次"),
            Self::NeedsNewerApp(v) => write!(f, "这份包的版本({v})比本应用新,请升级手机端"),
            Self::BadPack(e) => write!(f, "包已损坏,请换一份:{e}"),
            Self::Storage(e) => write!(f, "写入本机失败:{e}"),
            Self::Internal(e) => write!(f, "内部错误:{e}"),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<StoreError> for FfiError {
    fn from(e: StoreError) -> Self {
        use StoreError as S;
        match &e {
            // 口令错:用户重输即可。
            S::WrongPassword => Self::WrongPassword,
            // 包 / schema 版本比本端新:叫用户升级。
            S::UnsupportedPack(v) | S::UnsupportedSchema(v) => Self::NeedsNewerApp(*v),
            // 解析 / 迁移失败:`toml::de::Error` 的 Display 带出错行,那行可能就是
            // `password = "..."` 或私钥正文,所以固定文案,不拼 `e.to_string()`。
            // 判据:Display 是否可能含**包内容**(而不只是结构位置 / id / 数字)。
            S::TomlDe(_) | S::Migration(_) => Self::BadPack("包内容无法解析".into()),
            // 包内容问题,Display 只含固定文案 / 头部数字 / base64 位置 / 会话 id /
            // 库的参数错误,不含明文:保留原文案帮用户判断。`PackTooOld` 的文案
            // 就是出路(重新导出)。
            S::CorruptPack(_)
            | S::CorruptSecrets(_)
            | S::PackTooOld(_)
            | S::Utf8
            | S::Crypto
            | S::Kdf(_)
            | S::JumpCycle(_)
            | S::JumpTooDeep(_)
            | S::JumpDangling(_)
            | S::TunnelDangling(_)
            | S::DanglingCredential(_) => Self::BadPack(e.to_string()),
            // 本机问题(文件 IO / 序列化写出 / keyring / 本机主密钥):写手机本地数据失败。
            S::Io(_) | S::TomlSer(_) | S::Keyring(_) | S::CorruptKey => {
                Self::Storage(e.to_string())
            }
            // 不在导入路径上的变体(桌面端的改密 / 增删改 / 云备份操作):出现即调用方有误。
            S::CloudReseal(_)
            | S::PasswordRequired
            | S::NotFound(_)
            | S::GroupNotFound(_)
            | S::TunnelNotFound(_)
            | S::CredentialNotFound(_)
            | S::CredentialInUse(_)
            | S::NoMasterPassword => Self::Internal(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_in_pack_is_a_bad_pack_not_a_storage_failure() {
        assert!(matches!(
            FfiError::from(StoreError::Utf8),
            FfiError::BadPack(_)
        ));
        assert!(matches!(
            FfiError::from(StoreError::Crypto),
            FfiError::BadPack(_)
        ));
        assert!(matches!(
            FfiError::from(StoreError::Io("x".into())),
            FfiError::Storage(_)
        ));
    }

    /// 含包内容的变体(解析错误会带出错行,可能是 `password = "..."`)
    /// 映射到 BadPack 时不得拼接原文。
    #[test]
    fn bad_pack_text_never_carries_pack_content() {
        let mark = "password = \"s3cr3t-MARK\"";
        for e in [
            StoreError::TomlDe(mark.into()),
            StoreError::Migration(mark.into()),
        ] {
            let shown = FfiError::from(e).to_string();
            assert!(!shown.contains("s3cr3t-MARK"), "泄露了包内容:{shown}");
        }
    }

    #[test]
    fn a_too_old_pack_is_a_bad_pack_that_keeps_its_own_advice() {
        let shown = FfiError::from(StoreError::PackTooOld(4)).to_string();
        assert!(matches!(
            FfiError::from(StoreError::PackTooOld(4)),
            FfiError::BadPack(_)
        ));
        assert!(shown.contains("重新导出"), "{shown}");
    }
}
