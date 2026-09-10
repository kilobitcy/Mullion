//! F6:连接失败给可操作错误。每类一个变体,红线是不许统一 "connection failed"。

use std::fmt;

use crate::known_hosts::Fingerprint;

/// F254:开一条会话 channel 分三步走(`channel_open_session` → `request_pty`
/// → `request_shell`),这里记的是**倒在哪一步**。
///
/// 三步的处置互不相干,所以不许并成一句话说。原来的
/// `map_err(|_| ConnectError::PtyRequest)` 连服务端给的原因码一起扔了 ——
/// 那个码是判断「是不是撞了 `MaxSessions`」的唯一客观依据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelStage {
    /// 服务端明确回了 `CHANNEL_OPEN_FAILURE`。带的是 russh 给的原因码原文
    /// (`ResourceShortage` / `AdministrativelyProhibited` / …)。
    ///
    /// **原因码原文透传,不翻译成「可能是…」**:服务端为什么拒只有它知道,
    /// 客户端编一个理由出来就是在猜,而排查的人会当成事实。
    OpenDenied(String),
    /// 连 `channel_open_session` 都没走完(连接已断 / 发送失败)。跟 PTY
    /// 一点关系都没有 —— 这一档的文案里**不许出现 PTY**。
    OpenFailed(String),
    /// 通道开出来了,`request_pty` 被拒。这一档才是真正的「对端不允许 PTY」。
    Pty,
    /// PTY 也拿到了,`request_shell` 被拒。典型是 sftp-only 账号
    /// (`ForceCommand internal-sftp`)。
    Shell,
}

/// 连接期的可操作错误(F6)。每个变体对应一类可区分的失败。
#[derive(Debug)]
pub enum ConnectError {
    /// 域名解析失败(区别于「解析成功但连不上」)。
    DnsResolution(String),
    /// TCP 连接被拒绝(对端无监听 / 防火墙 RST)。
    ConnectionRefused(String),
    /// 认证失败(凭据不对,区别于连接失败)。
    AuthFailed,
    /// 主机密钥变更 —— 疑似 MITM,已拦截(F3)。
    HostKeyChanged {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 首次连接此主机,指纹未记录,需 TOFU 确认(F3)。
    /// 当前仅未来的 app 弹窗策略会产生;`TofuAccept` 自动记录未知主机,不产生此变体。
    HostKeyUnknown { host: String, got: Fingerprint },
    /// 其余 IO 错误(网络 / 读私钥 / agent socket)。
    Io(String),
    /// F254:开一条会话 channel 的某一段失败了。`held` 是失败那一刻**这条
    /// 连接**账上有几条会话 channel(口径见 [`crate::ledger`],**别拿它跟
    /// `MaxSessions` 精确对账**)。
    ///
    /// **不合并成一句「开 PTY 失败」**:见 `ChannelStage` 各档的说明,以及
    /// 测试 `the_four_ways_to_fail_opening_a_channel_each_say_their_own_thing`。
    SessionChannel { stage: ChannelStage, held: usize },
    /// 连不上代理本身(F4)。区别于「连上了代理但代理连不上目标」。
    ProxyUnreachable { proxy: String, cause: String },
    /// 代理拒绝了我们的认证凭据(F4)。
    ProxyAuthFailed { proxy: String },
    /// 代理接受了连接,但拒绝转发到目标(F4)。
    ProxyRejected { proxy: String, reason: String },
    /// 跳板链上某一跳失败(F5)。`hop` 是 "host:port"。
    JumpFailed { hop: String, cause: String },
    /// 本机侦听端口绑不上(F111)。**与 `Io` 分开**:用户要做的是去关掉占用
    /// 该端口的程序或换一个端口,而不是查网络 —— 而且它是**致命**的,
    /// 退避重试 8 次不会让端口变得没被占用(见 `tunnel::is_fatal`)。
    ListenFailed { port: u16, cause: String },
    /// 远端拒绝了 `tcpip-forward` 请求(F112)。**与 `Io` 分开**,理由同
    /// `ListenFailed`:能被拒的原因只有「远端那个端口已被占」和
    /// 「sshd 配了 `AllowTcpForwarding no`/`GatewayPorts` 不放行」,
    /// 两种都不会因为重试 8 次而改变,所以它是**致命**的。
    /// 协议层只回一个「拒绝」,不带原因 —— 这里不许编一个具体理由出来。
    RemoteForwardDenied { port: u16 },
}

/// 把 TCP 连接阶段的 io 错误分类到精确变体(F6)。
pub(crate) fn classify_tcp(e: std::io::Error) -> ConnectError {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => ConnectError::ConnectionRefused(e.to_string()),
        _ => ConnectError::Io(e.to_string()),
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::DnsResolution(h) => write!(f, "域名解析失败:{h} —— 检查主机名/DNS"),
            ConnectError::ConnectionRefused(a) => {
                write!(f, "连接被拒绝:{a} —— 检查端口/sshd 是否在跑")
            }
            ConnectError::AuthFailed => write!(f, "认证失败 —— 检查用户名/密钥/密码"),
            ConnectError::HostKeyChanged {
                host,
                expected,
                got,
            } => write!(
                f,
                "主机 {host} 的密钥已变更(疑似中间人,已拦截):记录 {} → 收到 {}",
                expected.to_ssh_string(),
                got.to_ssh_string()
            ),
            ConnectError::HostKeyUnknown { host, got } => {
                write!(
                    f,
                    "首次连接 {host},指纹 {} 未记录,需确认(TOFU)",
                    got.to_ssh_string()
                )
            }
            ConnectError::Io(e) => write!(f, "网络 IO 错误:{e}"),
            // F254:四段各说各的。每一句都带上 `held`,因为「持有几条」是
            // 判断该不该去看 MaxSessions 的唯一客观依据 —— 只在被拒那一段
            // 带的话,用户看到别的三段就无从判断了。
            ConnectError::SessionChannel { stage, held } => match stage {
                ChannelStage::OpenDenied(reason) => write!(
                    f,
                    "服务端拒绝新开会话通道({reason});这条连接此刻持有 {held} 条 \
                     —— 撞上 sshd 的 MaxSessions(默认 10)时报的就是这个,\
                     关掉几个分屏或文件面板再试"
                ),
                ChannelStage::OpenFailed(cause) => write!(
                    f,
                    "开会话通道时连接已经断了:{cause};这条连接此刻持有 {held} 条 \
                     —— 先看网络/代理链路,重连一次即可"
                ),
                ChannelStage::Pty => write!(
                    f,
                    "通道开出来了但服务端不给 PTY(request-pty 被拒);\
                     这条连接此刻持有 {held} 条 —— 检查 sshd 的 PermitTTY,\
                     或这个账号是不是配了强制命令"
                ),
                ChannelStage::Shell => write!(
                    f,
                    "PTY 拿到了但服务端不给 shell(request-shell 被拒);\
                     这条连接此刻持有 {held} 条 —— 这个账号可能是 sftp-only\
                     (ForceCommand internal-sftp)"
                ),
            },
            ConnectError::ProxyUnreachable { proxy, cause } => write!(
                f,
                "连不上代理 {proxy}:{cause} —— 检查代理是否在跑/地址端口是否写对"
            ),
            ConnectError::ProxyAuthFailed { proxy } => {
                write!(f, "代理 {proxy} 认证失败 —— 检查代理的用户名/口令")
            }
            ConnectError::ProxyRejected { proxy, reason } => write!(
                f,
                "代理 {proxy} 拒绝转发到目标:{reason} —— 目标地址可能不可达或被代理策略禁止"
            ),
            ConnectError::JumpFailed { hop, cause } => {
                write!(f, "跳板 {hop} 连接失败:{cause} —— 先单独连一下这台跳板")
            }
            ConnectError::ListenFailed { port, cause } => {
                write!(
                    f,
                    "本机端口 {port} 侦听失败:{cause} —— 换个端口或关掉占用它的程序"
                )
            }
            ConnectError::RemoteForwardDenied { port } => {
                write!(
                    f,
                    "远端拒绝在端口 {port} 上侦听 —— 该端口可能已被占用,或 sshd 的 AllowTcpForwarding/GatewayPorts 不允许"
                )
            }
        }
    }
}

impl std::error::Error for ConnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_is_distinct_from_generic_io() {
        let refused = classify_tcp(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(refused, ConnectError::ConnectionRefused(_)));
        let other = classify_tcp(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(
            matches!(other, ConnectError::Io(_)),
            "非 refused 应落 Io,不得混为一类"
        );
    }

    #[test]
    fn every_variant_has_distinct_actionable_message() {
        // F6 红线:每类错误消息互不相同且非空,不许统一 "connection failed"。
        let variants = [
            ConnectError::DnsResolution("h".into()),
            ConnectError::ConnectionRefused("1.2.3.4:22".into()),
            ConnectError::AuthFailed,
            ConnectError::HostKeyChanged {
                host: "h".into(),
                expected: Fingerprint(vec![1]),
                got: Fingerprint(vec![2]),
            },
            ConnectError::HostKeyUnknown {
                host: "h".into(),
                got: Fingerprint(vec![3]),
            },
            ConnectError::Io("io".into()),
            ConnectError::SessionChannel {
                stage: ChannelStage::Pty,
                held: 3,
            },
            ConnectError::ProxyUnreachable {
                proxy: "127.0.0.1:7891".into(),
                cause: "connection refused".into(),
            },
            ConnectError::ProxyAuthFailed {
                proxy: "127.0.0.1:7891".into(),
            },
            ConnectError::ProxyRejected {
                proxy: "127.0.0.1:7891".into(),
                reason: "host unreachable".into(),
            },
            ConnectError::JumpFailed {
                hop: "bastion:22".into(),
                cause: "认证失败".into(),
            },
            ConnectError::ListenFailed {
                port: 3306,
                cause: "地址已被占用".into(),
            },
            ConnectError::RemoteForwardDenied { port: 8080 },
        ];
        let msgs: Vec<String> = variants.iter().map(|e| e.to_string()).collect();
        for m in &msgs {
            assert!(!m.is_empty());
        }
        let mut uniq = msgs.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), msgs.len(), "错误消息必须两两不同(F6)");
    }

    /// F6 的延伸:代理失败和目标失败必须能一眼分开,否则用户会去查目标主机的
    /// sshd 而问题其实在本机代理上。
    #[test]
    fn proxy_errors_name_the_proxy_not_the_target() {
        let e = ConnectError::ProxyUnreachable {
            proxy: "127.0.0.1:7891".into(),
            cause: "refused".into(),
        }
        .to_string();
        assert!(e.contains("127.0.0.1:7891"), "消息里必须点名代理: {e}");
        assert!(e.contains("代理"), "消息里必须说明这是代理侧失败: {e}");
    }

    /// F254:开会话 channel 的四种失败必须**各说各的**。
    ///
    /// 原来它们统统落在一句「开 PTY 失败 —— 对端可能不允许 PTY」上,而
    /// 实际处置完全不同:
    /// - 服务端拒绝开通道:多半撞了 `MaxSessions`,该关掉几个分屏;
    /// - 通道请求都没发出去:连接已经断了,PTY 一点关系都没有;
    /// - `request_pty` 被拒:才是真正的「对端不允许 PTY」(`PermitTTY no`);
    /// - `request_shell` 被拒:sftp-only 账号,连不上跟 PTY 也没关系。
    ///
    /// 用户拿着「对端可能不允许 PTY」这句话去查 sshd 的 `PermitTTY`,而真
    /// 原因是他开了 10 个分屏 —— 查一整晚也查不出来。
    ///
    /// 自证会变红:把 `Display` 里四个分支中的任意两个写成同一句。
    #[test]
    fn the_four_ways_to_fail_opening_a_channel_each_say_their_own_thing() {
        let msgs: Vec<String> = [
            ChannelStage::OpenDenied("ResourceShortage".into()),
            ChannelStage::OpenFailed("connection closed".into()),
            ChannelStage::Pty,
            ChannelStage::Shell,
        ]
        .into_iter()
        .map(|stage| ConnectError::SessionChannel { stage, held: 9 }.to_string())
        .collect();
        let mut uniq = msgs.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), msgs.len(), "四段的消息必须两两不同:{msgs:?}");
        for m in &msgs {
            assert!(m.contains("9"), "每一段都要带上持有条数:{m}");
        }
        assert!(
            msgs[0].contains("ResourceShortage"),
            "服务端给的原因码要原文透传,不许自己编一个理由:{}",
            msgs[0]
        );
        assert!(
            msgs[0].contains("MaxSessions"),
            "被拒那一段要点名最可能的那个原因,否则「持有 9 条」没人看得懂:{}",
            msgs[0]
        );
        assert!(
            msgs[1].contains("connection closed"),
            "发不出去那一段要带上根因:{}",
            msgs[1]
        );
        assert!(
            !msgs[1].contains("PTY"),
            "连通道都没开出来,提 PTY 就是把人往错方向引:{}",
            msgs[1]
        );
        assert!(
            msgs[2].contains("PermitTTY"),
            "request_pty 被拒才该指向 PermitTTY:{}",
            msgs[2]
        );
    }

    /// 跳板失败要说清是**哪一跳**——五跳链路里不说明等于没说。
    #[test]
    fn jump_error_names_the_failing_hop() {
        let e = ConnectError::JumpFailed {
            hop: "bastion:22".into(),
            cause: "认证失败".into(),
        }
        .to_string();
        assert!(e.contains("bastion:22"), "必须点名失败的那一跳: {e}");
        assert!(e.contains("认证失败"), "必须带上根因: {e}");
    }
}
