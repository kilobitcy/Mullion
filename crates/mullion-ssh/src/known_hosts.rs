//! TOFU 主机密钥校验(F3):首次记录指纹,变更时拦截。
//!
//! **红线**:`verify` 绝不无条件返回 true。指纹不匹配、或主机未记录过,
//! 一律返回 `false`——放行等于关掉 SSH 的全部身份保证。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// 主机密钥指纹。骨架用原始字节表示(真实为公钥的哈希摘要)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub Vec<u8>);

/// TOFU 已知主机表:host → 记录过的指纹。
#[derive(Debug, Default)]
pub struct KnownHosts {
    entries: HashMap<String, Fingerprint>,
}

impl KnownHosts {
    pub fn new() -> Self {
        Self::default()
    }

    /// 首次见到 `host` 时记录其指纹(TOFU)。已存在则覆盖(仅在上层确认变更后调用)。
    pub fn record(&mut self, host: &str, fp: Fingerprint) {
        self.entries.insert(host.to_owned(), fp);
    }

    /// 校验 `host` 的指纹。
    ///
    /// 只有「已记录过且指纹完全一致」才返回 `true`。指纹不匹配(主机密钥变更)、
    /// 或主机从未记录过,都返回 `false`——由上层走 TOFU 首次确认/变更拦截流程。
    pub fn verify(&self, host: &str, fp: &Fingerprint) -> bool {
        match self.entries.get(host) {
            Some(known) => known == fp,
            None => false,
        }
    }
}

impl Fingerprint {
    /// 从 SSH 公钥算 SHA-256 指纹(用 ssh_key 内置,不引 sha2)。
    pub fn from_public_key(key: &russh::keys::ssh_key::PublicKey) -> Self {
        let f = key.fingerprint(russh::keys::ssh_key::HashAlg::Sha256);
        Fingerprint(f.as_bytes().to_vec())
    }

    /// `SHA256:<base64-unpadded>` —— 与 `ssh-keygen -lf` 第二列同格式,用户可肉眼核对。
    /// 非 32 字节且非空(只可能来自测试/伪造)退化成十六进制;空字节(存档指纹
    /// 解析失败时的占位值,见 `PromptingPolicy::decide`)退化成中文占位文本
    /// ——`Vec::new().iter().map(...).collect::<String>()` 本身就是空串,直接
    /// 拼进「记录 → 收到 」这种变更警告文案里会显示成一片空白,读起来像程序
    /// 坏了而不是安全警告,所以单独判掉。
    pub fn to_ssh_string(&self) -> String {
        match <[u8; 32]>::try_from(self.0.as_slice()) {
            Ok(bytes) => russh::keys::ssh_key::Fingerprint::Sha256(bytes).to_string(),
            Err(_) if self.0.is_empty() => "(无法解析)".to_string(),
            Err(_) => self.0.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    /// `SHA256:<base64>` → 指纹字节。解析失败返回 `None`(存档被手改坏)。
    ///
    /// 只接受 `Sha256` 变体:`ssh_key::Fingerprint` 还有 `Sha512`,放进来会得到
    /// 64 字节的 `Fingerprint`——它与 `from_public_key` 产出的 32 字节永不相等
    /// (只会永久判「指纹变更」,安全上是保守的),但 `to_ssh_string()` 会退化成
    /// 无前缀十六进制,写进存档后再也读不回来,TOFU 记录被静默丢弃。
    pub fn parse_ssh(s: &str) -> Option<Self> {
        match s.parse::<russh::keys::ssh_key::Fingerprint>().ok()? {
            f @ russh::keys::ssh_key::Fingerprint::Sha256(_) => {
                Some(Fingerprint(f.as_bytes().to_vec()))
            }
            _ => None,
        }
    }
}

impl KnownHosts {
    /// 查已记录的指纹(不改状态)。
    pub fn get(&self, host: &str) -> Option<&Fingerprint> {
        self.entries.get(host)
    }
}

/// 主机密钥被拒时的精确原因,供 connect 映射成 ConnectError。
#[derive(Debug, Clone)]
pub enum HostKeyOutcome {
    /// 记录过但指纹不一致(疑似 MITM)。
    Changed {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 从未记录,策略不自动信任。
    Unknown { host: String, got: Fingerprint },
}

/// check_server_key 的决策结果。
#[derive(Debug)]
pub enum HostKeyDecision {
    Accept,
    Reject(HostKeyOutcome),
}

/// `HostKeyPolicy::decide` 的返回类型。
///
/// 手写 `Pin<Box<dyn Future>>` 而不是 `async fn`:trait 里的 async fn(AFIT)不是
/// dyn-safe,而 `Arc<dyn HostKeyPolicy>` 正是 ssh 与 app 解耦的关键(ssh 不认识 GUI)。
/// 也不引 `async_trait` 宏——一个方法不值得多一个依赖。
///
/// 给实现者(如 Task 8 的弹窗版)的提示:若 Future 需要跨 `.await` 持有
/// `host`/`algo`/`fp`,必须先 `to_owned()`/`clone()` 转成有所有权的值再 move 进
/// `async move` 块——不能直接捕获 `&'a str`/`&'a Fingerprint` 引用去跨等待点。
/// 同理,任何 `MutexGuard` 都不是 `Send`,跨 `.await` 持有会直接编译失败;
/// 参考 `TofuAccept` 里「锁内算完结果、锁释放后再 `Box::pin`」的写法。
pub type HostKeyFuture<'a> = Pin<Box<dyn Future<Output = HostKeyDecision> + Send + 'a>>;

/// 主机密钥策略。ssh 不弹 UI —— app 注入实现(弹窗版),测试/冒烟注入 TofuAccept。
///
/// 返回 Future:app 的弹窗版要在这里挂起,等 GUI 线程回答(F3)。
pub trait HostKeyPolicy: Send + Sync {
    /// `algo` 形如 `ssh-ed25519`,只用于弹窗展示与人工核对。
    fn decide<'a>(&'a self, host: &'a str, algo: &'a str, fp: &'a Fingerprint)
        -> HostKeyFuture<'a>;
}

/// TOFU 策略:未记录→记录并放行;一致→放行;不一致→拒(Changed)。
/// 冒烟/hermetic 测试用它;app 用 `PromptingPolicy`(弹窗版)。
pub struct TofuAccept {
    known: std::sync::Arc<std::sync::Mutex<KnownHosts>>,
}

impl TofuAccept {
    pub fn new(known: std::sync::Arc<std::sync::Mutex<KnownHosts>>) -> Self {
        Self { known }
    }
}

impl HostKeyPolicy for TofuAccept {
    fn decide<'a>(
        &'a self,
        host: &'a str,
        _algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        // `known` 是 App 生命周期内全局共用的单例锁(main.rs 建一次,所有连接共享)。
        // `expect` 会让一次意外 panic 把本会话的 TOFU 永久废掉——此后每次
        // `check_server_key` 都会在这里再 panic 一次。表内数据只是 HashMap 插入,
        // panic-safe,中毒后继续用不会读到半成品状态,所以选择恢复而不是 panic。
        let mut kh = self.known.lock().unwrap_or_else(|e| e.into_inner());
        let decision = match kh.get(host).cloned() {
            None => {
                kh.record(host, fp.clone());
                HostKeyDecision::Accept
            }
            Some(known) if &known == fp => HostKeyDecision::Accept,
            Some(known) => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host.to_owned(),
                expected: known,
                got: fp.clone(),
            }),
        };
        // 本策略不需要等任何人,立刻就绪。guard 在这里(Box::pin 之前)已经
        // 随 kh 一起 drop——Future 内部不持有锁,Send 边界不受影响。
        Box::pin(std::future::ready(decision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(bytes: &[u8]) -> Fingerprint {
        Fingerprint(bytes.to_vec())
    }

    #[test]
    fn mismatched_fingerprint_is_rejected() {
        // F3:主机密钥变更(指纹不匹配)必须被拦截,连接失败。
        let mut kh = KnownHosts::new();
        kh.record("host.example", fp(b"AAAA"));
        assert!(
            !kh.verify("host.example", &fp(b"BBBB")),
            "指纹变更必须返回 false"
        );
    }

    #[test]
    fn matching_fingerprint_is_accepted() {
        let mut kh = KnownHosts::new();
        kh.record("host.example", fp(b"AAAA"));
        assert!(kh.verify("host.example", &fp(b"AAAA")));
    }

    #[test]
    fn unknown_host_is_rejected() {
        // 未记录过的主机绝不默认放行,交上层走 TOFU 首次确认。
        let kh = KnownHosts::new();
        assert!(!kh.verify("stranger.example", &fp(b"AAAA")));
    }

    #[test]
    fn fingerprint_from_key_is_deterministic_and_distinguishes_keys() {
        // 同一把私钥的公钥 → 指纹稳定;不同私钥 → 指纹不同。
        let k1 = russh::keys::load_secret_key("tests/fixtures/client_key", None).unwrap();
        let k2 = russh::keys::load_secret_key("tests/fixtures/other_key", None).unwrap();
        let fp1a = Fingerprint::from_public_key(k1.public_key());
        let fp1b = Fingerprint::from_public_key(k1.public_key());
        let fp2 = Fingerprint::from_public_key(k2.public_key());
        assert_eq!(fp1a, fp1b, "同一公钥指纹必须稳定");
        assert_ne!(fp1a, fp2, "不同公钥指纹必须不同");
        assert_eq!(fp1a.0.len(), 32, "SHA-256 应为 32 字节");
    }

    #[tokio::test]
    async fn tofu_records_unknown_then_accepts_same_rejects_changed() {
        // F3:未知主机首次记录并放行;同指纹再来放行;指纹变更 → Reject(Changed)。
        let known = std::sync::Arc::new(std::sync::Mutex::new(KnownHosts::new()));
        let policy = TofuAccept::new(known);
        let a = fp(b"AAAA");
        let b = fp(b"BBBB");
        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &a).await,
                HostKeyDecision::Accept
            ),
            "首次应记录并放行"
        );
        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &a).await,
                HostKeyDecision::Accept
            ),
            "同指纹应放行"
        );
        match policy.decide("h", "ssh-ed25519", &b).await {
            HostKeyDecision::Reject(HostKeyOutcome::Changed { expected, got, .. }) => {
                assert_eq!(expected, a);
                assert_eq!(got, b);
            }
            _ => panic!("指纹变更必须 Reject(Changed)(F3 红线)"),
        }
    }

    #[test]
    fn ssh_string_round_trips_and_matches_ssh_keygen_format() {
        // F3:存档里的指纹必须与 `ssh-keygen -lf` 的第二列同格式,
        // 否则用户没法用官方工具核对弹窗里的指纹。
        let k = russh::keys::load_secret_key("tests/fixtures/client_key", None).unwrap();
        let f = Fingerprint::from_public_key(k.public_key());
        let text = f.to_ssh_string();
        assert!(text.starts_with("SHA256:"), "格式不对:{text}");
        assert!(!text.ends_with('='), "OpenSSH 用不带填充的 base64");
        assert_eq!(Fingerprint::parse_ssh(&text), Some(f));
        assert_eq!(Fingerprint::parse_ssh("garbage"), None);
    }

    #[test]
    fn empty_fingerprint_renders_placeholder_so_change_warning_is_never_blank() {
        // 存档里的指纹解析不出来时(手改坏 / 历史 SHA512 残留),app 侧
        // `PromptingPolicy::decide` 会用空 `Fingerprint` 占位塞进
        // `HostKeyOutcome::Changed { expected, .. }`。它最终经 to_ssh_string()
        // 拼进「记录 {expected} → 收到 {got}」这条最高危警告文案——如果这里
        // 退化成空串,警告会显示成一片空白,用户读起来像程序坏了而不是遇到
        // 中间人。必须是非空占位文本。
        let text = Fingerprint(Vec::new()).to_ssh_string();
        assert!(!text.is_empty(), "空指纹绝不能渲染成空串");
        assert_eq!(text, "(无法解析)");
    }

    #[test]
    fn parse_ssh_rejects_non_sha256_so_archive_round_trip_cannot_break() {
        // SHA512 能被 ssh_key 解析成 64 字节指纹,但那样 to_ssh_string() 会退化成
        // 无前缀十六进制,存进 known_hosts.toml 后 parse_ssh 再也读不回来——
        // TOFU 记录静默丢失,主机退回「首次连接」。从入口拒掉。
        let sha512 = format!("SHA512:{}", "A".repeat(86));
        assert_eq!(Fingerprint::parse_ssh(&sha512), None, "只接受 SHA256");
    }
}
