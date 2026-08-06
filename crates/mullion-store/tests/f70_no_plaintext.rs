//! F70 守护:写一条带密码/私钥口令的会话并落盘后,sessions.toml 与 secrets.enc 的
//! 原始字节里都搜不到明文口令。用 InMemoryKey 保证确定性。

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, Identity, InMemoryKey, Protocol, SecretEntry,
    SessionDraft, TerminalPrefs, Vault,
};

const PW: &str = "hunter2-VERY-secret-passphrase-xyz";
const PROXY_PW: &str = "hunter2-VERY-secret-proxy-password-abc";

#[test]
fn plaintext_secret_never_hits_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(dir.path().to_path_buf(), &InMemoryKey([42u8; 32])).unwrap();
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
                    path: "/k.pem".into(),
                    has_passphrase: true,
                },
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: Default::default(),
            automation: Default::default(),
            secret: Some(SecretEntry {
                password: None,
                passphrase: Some(PW.into()),
                proxy_password: Some(PROXY_PW.into()),
            }),
        },
        "2026-07-25T00:00:00Z",
    );
    vault.save().unwrap();

    let toml_bytes = std::fs::read(dir.path().join("sessions.toml")).unwrap();
    let enc_bytes = std::fs::read(dir.path().join("secrets.enc")).unwrap();
    for (label, needle) in [("口令", PW.as_bytes()), ("代理口令", PROXY_PW.as_bytes())] {
        assert!(
            !contains(&toml_bytes, needle),
            "sessions.toml 里出现了明文{label}"
        );
        assert!(
            !contains(&enc_bytes, needle),
            "secrets.enc 里出现了明文{label}"
        );
    }

    // 反证:同一密钥能解回明文,确保不是「加密了但丢了数据」。
    let reopened = Vault::open(dir.path().to_path_buf(), &InMemoryKey([42u8; 32])).unwrap();
    let id = reopened.list()[0].id;
    assert_eq!(reopened.secret(id).unwrap().passphrase.as_deref(), Some(PW));
    assert_eq!(
        reopened.secret(id).unwrap().proxy_password.as_deref(),
        Some(PROXY_PW)
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
