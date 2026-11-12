//! F346-a 演示包(全是假数据)。`tests/import_pack.rs` 与
//! `examples/make_test_pack.rs`(生成 Android 插桩测试资产)用 `#[path]` 共用这一份。
//!
//! 项目「演示项目」= 节点 1 + 节点 2(经跳板 3;节点 2 用共享凭据 5);会话 4 无关。
//! 密文键 1、3、4、cred:5 → 可达 1、3、cred:5(保留 3),丢 4(丢弃 1)。
#![allow(dead_code)]

use std::collections::BTreeMap;

use base64::Engine as _;
use mullion_store::automation::AutomationPrefs;
use mullion_store::model::{SessionsFile, CURRENT_SCHEMA};
use mullion_store::project::ProjectId;
use mullion_store::sftp::SftpPrefs;
use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, CredentialId, CredentialRecord, Identity, JumpRef,
    NetworkPrefs, PackFile, ProjectRecord, Protocol, SecretEntry, SessionId, SessionRecord,
    TerminalPrefs,
};

pub const DEMO_PASSWORD: &str = "demo-pack-password";
pub const DEMO_PROJECT: &str = "演示项目";

fn session(id: u64, name: &str, jump: &[u64], auth: Auth) -> SessionRecord {
    SessionRecord {
        id: SessionId(id),
        modified_at: "2026-11-12T00:00:00Z".into(),
        identity: Identity {
            name: name.into(),
            note: String::new(),
            group_id: None,
            tags: Vec::new(),
        },
        connection: Connection {
            host: format!("node{id}.example"),
            port: 22,
            protocol: Protocol::Ssh,
        },
        auth,
        terminal: TerminalPrefs::default(),
        appearance: AppearancePrefs::default(),
        network: NetworkPrefs {
            proxy: None,
            jump: Some(jump.iter().map(|i| JumpRef(SessionId(*i))).collect()),
        },
        automation: AutomationPrefs::default(),
        sftp: SftpPrefs::default(),
        last_connected_at: None,
    }
}

pub fn sessions_file() -> SessionsFile {
    let pw = || Auth::inline("demo", AuthKind::Password);
    SessionsFile {
        schema_version: CURRENT_SCHEMA,
        group: Vec::new(),
        session: vec![
            session(1, "节点", &[], pw()),
            session(2, "经跳板节点", &[3], Auth::Ref(CredentialId(5))),
            session(3, "跳板", &[], pw()),
            session(4, "无关会话", &[], pw()),
        ],
        tunnel: Vec::new(),
        credential: vec![CredentialRecord {
            id: CredentialId(5),
            name: "共享凭据".into(),
            user: "demo".into(),
            kind: AuthKind::Password,
        }],
        project: vec![ProjectRecord {
            id: ProjectId(1),
            name: DEMO_PROJECT.into(),
            note: String::new(),
            nodes: vec![SessionId(1), SessionId(2)],
            preferred: None,
            dir: "/srv/demo".into(),
            tmux_name: None,
            created_at: "2026-11-12T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            memos: Vec::new(),
            icon: None,
        }],
    }
}

/// 任意文件 + 任意密文段拼成包文本。
pub fn pack_text(files: Vec<(&str, Vec<u8>)>, secrets_blob: &[u8]) -> String {
    let files = files
        .into_iter()
        .map(|(path, bytes)| PackFile {
            path: path.into(),
            body: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
        .collect();
    mullion_store::write_pack(files, secrets_blob, "demo", "2026-11-12T00:00:00Z").unwrap()
}

pub fn demo_pack() -> String {
    let secrets: BTreeMap<String, SecretEntry> = ["1", "3", "4", "cred:5"]
        .into_iter()
        .map(|k| {
            let e = SecretEntry {
                password: Some(format!("fake-{k}")),
                ..Default::default()
            };
            (k.to_string(), e)
        })
        .collect();
    let plain = toml::to_string_pretty(&secrets).unwrap();
    let blob = mullion_store::seal_secrets(plain.as_bytes(), DEMO_PASSWORD).unwrap();
    pack_text(
        vec![
            (
                "sessions.toml",
                toml::to_string_pretty(&sessions_file())
                    .unwrap()
                    .into_bytes(),
            ),
            ("known_hosts.toml", Vec::new()),
        ],
        &blob,
    )
}
