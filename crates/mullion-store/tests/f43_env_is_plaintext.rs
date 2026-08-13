//! F43 守护:登录后自动化的环境变量(`automation.env`)必须**明文**落进
//! `sessions.toml`,绝不能被误路由进加密侧车 `secrets.enc`。
//!
//! 为什么要反着测:设计 §6 明确写了这条——env 值终归要以 `export` 行发进
//! 远端,本来就会落进 shell 历史与 `/proc/<pid>/environ`,加密只会给用户一个
//! 错误的安全承诺,这里不是存密码的地方。`f70_no_plaintext.rs` 钉住的是密码
//! **不出现**在明文文件里;这条测试方向相反,钉住 env **必须出现**在明文文件
//! 里、且**不出现**在加密侧车里——挡的是「未来有人手滑把 env 并入 SecretEntry
//! 加密路径」这类回归,而不是数据丢失(那是既有 round-trip 测试的活)。

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, AutomationPrefs, Connection, EnvVar, Identity, InMemoryKey,
    Protocol, SessionDraft, TerminalPrefs, Vault,
};

// 足够独特的哨兵值,避免碰巧命中 toml 序列化产生的其他字节(键名、schema 版本号等)。
const SENTINEL_KEY: &str = "MULLION_SENTINEL_ENV_KEY_9F3A";
const SENTINEL_VALUE: &str = "MULLION_SENTINEL_ENV_VALUE_9f3a";

#[test]
fn automation_env_is_plaintext_in_sessions_toml_and_absent_from_secrets_enc() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(dir.path().to_path_buf(), &InMemoryKey([7u8; 32])).unwrap();
    vault.add(
        SessionDraft {
            identity: Identity {
                name: "s".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::PublicKey {
                    has_passphrase: false,
                },
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: Default::default(),
            automation: AutomationPrefs {
                env: Some(vec![EnvVar {
                    key: SENTINEL_KEY.into(),
                    value: SENTINEL_VALUE.into(),
                }]),
                ..Default::default()
            },
            sftp: Default::default(),
            secret: None,
        },
        "2026-08-06T00:00:00Z",
    );
    vault.save().unwrap();

    let toml_bytes = std::fs::read(dir.path().join("sessions.toml")).unwrap();
    let enc_bytes = std::fs::read(dir.path().join("secrets.enc")).unwrap();

    // 正向:key 与 value 明文子串确实出现在 sessions.toml 里——env 就是设计
    // 要求「存明文」的东西,这里不是防泄漏,是防「该明文的东西被悄悄加密掉」。
    assert!(
        contains(&toml_bytes, SENTINEL_KEY.as_bytes()),
        "automation.env 的 key 应以明文出现在 sessions.toml 里"
    );
    assert!(
        contains(&toml_bytes, SENTINEL_VALUE.as_bytes()),
        "automation.env 的 value 应以明文出现在 sessions.toml 里"
    );

    // 负向:同一个 value 绝不能出现在加密侧车里——一旦出现,就意味着 env 被
    // 误路由进了 SecretEntry/secrets.enc 的加密路径,这正是本测试要挡的回归。
    assert!(
        !contains(&enc_bytes, SENTINEL_VALUE.as_bytes()),
        "automation.env 的 value 不应出现在 secrets.enc 里(env 不是存密码的地方)"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
