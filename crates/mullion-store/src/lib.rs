//! mullion-store —— 会话与凭据持久化。无头、可纯单测,零 UI/GPU/async。
//! 依赖方向:app → store;store 不依赖 core/term/ssh。

pub mod automation;
pub mod credential;
pub mod crypto;
pub mod error;
pub mod group;
pub mod inherit;
pub mod jump;
pub mod kdf;
pub mod known_hosts;
pub mod layout;
pub mod master_key;
pub mod migrate;
pub mod model;
pub mod network;
pub mod secrets_file;
pub mod settings;
pub mod sftp;
pub mod ssh_config;
pub mod tunnel;
pub mod vault;

pub use automation::{
    build_plan, AutomationCommand, AutomationPrefs, EnvVar, ResolvedAutomation, Step, TmuxChoice,
};
pub use credential::{display_user, Auth, CredentialId, CredentialRecord, InlineAuth};
pub use error::StoreError;
pub use group::GroupRecord;
pub use inherit::{resolve, PrefsLayer, ResolvedConfig, DEFAULT_SCROLLBACK};
pub use kdf::{derive_key, KdfParams, SALT_LEN};
pub use known_hosts::{HostKeyEntry, KnownHostsFile};
pub use layout::{
    SavedDir, SavedLayout, SavedNodeEntry, SavedTab, SavedTabKind, SavedWindow,
    CURRENT_LAYOUT_SCHEMA,
};
pub use master_key::{InMemoryKey, KeyringSource, MasterKeySource};
pub use migrate::{migrate_v1, SchemaProbe};
pub use model::{
    AppearancePrefs, AuthKind, ColorSpec, ColorTarget, Connection, GroupId, IconKind, IconSpec,
    Identity, Protocol, SecretEntry, SessionId, SessionRecord, TerminalPrefs,
};
pub use network::{JumpRef, NetworkPrefs, ProxyChoice, ProxyEndpoint};
pub use secrets_file::Scheme;
pub use settings::{Settings, CURRENT_SETTINGS_SCHEMA, MAX_FONT_PT, MIN_FONT_PT};
pub use sftp::{Bookmark, SftpPrefs};
pub use ssh_config::{parse as parse_ssh_config, HostEntry, ParsedConfig, SkipNote};
pub use tunnel::{TunnelId, TunnelKind, TunnelRecord};
pub use vault::{CredentialDraft, ResolvedAuth, SessionDraft, TunnelDraft, Unlock, Vault};
