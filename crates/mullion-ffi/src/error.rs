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
    /// F346-b:`cloud.toml` 存在但读不懂。出路是「重置云配置」。
    CloudConfigCorrupt,
    /// F346-b:`cloud.toml` 里的 SK / 口令用别的钥封过(Keystore 重置后残留等)。
    /// **不得**落到 `BadPack`(那条文案说的是「包」)。
    CloudConfigUnreadable,
    /// F346-b:必填项没填。值 = 缺的字段名,顿号分隔。
    CloudIncomplete(String),
    /// F346-b:前缀下一个对象都没有。值 = 前缀。
    CloudNoBackup(String),
    /// F346-b:列到了对象,但没有一个是备份文件的形状 —— 多半是前缀填错。
    CloudNoRecognizedBackup { count: u32, prefix: String },
    /// F346-b:403 InvalidAccessKeyId / SignatureDoesNotMatch。
    CloudAuth,
    /// F346-b:403 RequestTimeTooSkewed。
    CloudClockSkew,
    /// F346-b:列举之后、GET 之前最新那份被桌面的保留策略清掉了(404)。
    CloudBackupVanished,
    /// F346-b:对象超过手机端上限。
    CloudTooLarge,
    /// F346-b:其余云端错误(网络 / 其他状态码 / 配置不合法)。
    Cloud(String),
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
            Self::CloudConfigCorrupt => write!(f, "云配置损坏,请点「重置云配置」后重新填写"),
            Self::CloudConfigUnreadable => {
                write!(f, "云配置里的 SK/口令解不开,请重新填写 SK 与备份口令")
            }
            Self::CloudIncomplete(m) => write!(f, "先填:{m}"),
            Self::CloudNoBackup(p) => write!(f, "云端还没有备份(前缀 {p})"),
            Self::CloudNoRecognizedBackup { count, prefix } => write!(
                f,
                "前缀 {prefix} 下列到 {count} 个对象,都不是备份文件 —— 检查前缀(桌面默认 mullion/,结尾斜杠也算)"
            ),
            Self::CloudAuth => write!(f, "AK/SK 不对(已保存,请重填)"),
            Self::CloudClockSkew => write!(f, "手机时间偏差过大,检查系统时间后重试"),
            Self::CloudBackupVanished => write!(f, "最新备份刚被替换,请重试"),
            Self::CloudTooLarge => write!(f, "云端对象过大,不像 Mullion 的备份"),
            Self::Cloud(e) => write!(f, "云端:{e}"),
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

/// 只取 `<Code>…</Code>`:正文里可能有 `AWSAccessKeyId` / `StringToSign`(设计 R10)。
/// 内容同样来自不可信服务端:长度 > 64 或含 `[A-Za-z0-9.]` 以外的字符就当没有。
fn server_code(body: &str) -> Option<&str> {
    let start = body.find("<Code>")? + "<Code>".len();
    let len = body[start..].find("</Code>")?;
    let code = &body[start..start + len];
    let plain = !code.is_empty()
        && code.len() <= 64
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    plain.then_some(code)
}

/// 服务端给的文本(如重定向的 Location)不可信:去控制字符、最多 160 个字符。
fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(160).collect()
}

impl From<mullion_cloud::CloudError> for FfiError {
    fn from(e: mullion_cloud::CloudError) -> Self {
        use mullion_cloud::CloudError as C;
        match e {
            C::Status { code, ref body } => match (code, server_code(body)) {
                (403, Some("InvalidAccessKeyId" | "SignatureDoesNotMatch")) => Self::CloudAuth,
                (_, Some("RequestTimeTooSkewed")) => Self::CloudClockSkew,
                (_, Some(c)) => Self::Cloud(format!("服务端返回 {code} {c}")),
                (_, None) => Self::Cloud(format!("服务端返回 {code}")),
            },
            C::TooLarge { .. } => Self::CloudTooLarge,
            C::TooManyKeys { .. } => Self::Cloud("云端对象数量过多,超出上限".into()),
            // Malformed 带响应正文前 200 字符(captive portal 回 200+HTML):固定文案。
            C::Malformed(_) => {
                Self::Cloud("云端响应不是有效的对象列表(检查 Endpoint / 代理)".into())
            }
            C::Transport(_) | C::Config(_) => Self::Cloud(sanitize(&e.to_string())),
            // 写路径专属:手机端不该见到。
            C::AlreadyExists | C::IfNoneMatchRejected => Self::Internal(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_cloud::CloudError;

    fn status(code: u16, server_code: &str) -> CloudError {
        CloudError::Status {
            code,
            body: format!(
                "<Error><Code>{server_code}</Code><AWSAccessKeyId>AKIA-SECRET-MARK</AWSAccessKeyId></Error>"
            ),
        }
    }

    #[test]
    fn auth_and_clock_errors_get_their_own_advice() {
        assert!(matches!(
            FfiError::from(status(403, "InvalidAccessKeyId")),
            FfiError::CloudAuth
        ));
        assert!(matches!(
            FfiError::from(status(403, "SignatureDoesNotMatch")),
            FfiError::CloudAuth
        ));
        assert!(matches!(
            FfiError::from(status(403, "RequestTimeTooSkewed")),
            FfiError::CloudClockSkew
        ));
        assert!(matches!(
            FfiError::from(CloudError::TooLarge { limit: 1 }),
            FfiError::CloudTooLarge
        ));
    }

    /// 正文里可能带 AK / StringToSign:只许把 `<Code>` 带给用户。
    #[test]
    fn other_status_errors_show_only_the_server_code() {
        let shown = FfiError::from(status(500, "InternalError")).to_string();
        assert!(
            shown.contains("500") && shown.contains("InternalError"),
            "{shown}"
        );
        assert!(!shown.contains("AKIA-SECRET-MARK"), "泄露了正文:{shown}");
        let html = FfiError::from(CloudError::Status {
            code: 502,
            body: "<html>AKIA-SECRET-MARK</html>".into(),
        })
        .to_string();
        assert!(
            html.contains("502") && !html.contains("AKIA-SECRET-MARK"),
            "{html}"
        );
    }

    /// `<Code>` 的内容也来自不可信服务端:不像错误码的一律不显示。
    #[test]
    fn a_server_code_that_does_not_look_like_a_code_is_not_shown() {
        for bad in ["AKIA-SECRET-MARK xyz", "AKIA-SECRET-MARK", &"A".repeat(65)] {
            let shown = FfiError::from(status(500, bad)).to_string();
            assert!(shown.contains("500"), "{shown}");
            assert!(
                !shown.contains("AKIA-SECRET-MARK") && !shown.contains(&"A".repeat(65)),
                "{shown}"
            );
        }
    }

    /// Malformed 带响应正文前 200 字符(captive portal 回 200+HTML):不得直通。
    #[test]
    fn a_malformed_response_never_shows_its_payload() {
        let shown = FfiError::from(CloudError::Malformed("AKIA-SECRET-MARK".into())).to_string();
        assert!(!shown.contains("AKIA-SECRET-MARK"), "泄露了正文:{shown}");
    }

    /// Config 可带服务端给的 Location 头:去控制字符、限长。
    #[test]
    fn transport_and_config_text_is_sanitized_and_bounded() {
        let raw = format!("x\u{1b}[31m{}", "A".repeat(500));
        for e in [
            CloudError::Config(raw.clone()),
            CloudError::Transport(raw.clone()),
        ] {
            let shown = FfiError::from(e).to_string();
            assert!(!shown.contains('\u{1b}'), "含 ESC:{shown:?}");
            assert!(
                shown.chars().count() <= 200,
                "过长:{}",
                shown.chars().count()
            );
        }
    }

    #[test]
    fn write_path_errors_are_internal() {
        assert!(matches!(
            FfiError::from(CloudError::AlreadyExists),
            FfiError::Internal(_)
        ));
        assert!(matches!(
            FfiError::from(CloudError::IfNoneMatchRejected),
            FfiError::Internal(_)
        ));
    }

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
