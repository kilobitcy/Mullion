//! mullion-store —— 会话与凭据持久化。无头、可纯单测,零 UI/GPU/async。
//! 依赖方向:app → store;store 不依赖 core/term/ssh。

pub mod automation;
pub mod crypto;
pub mod error;
pub mod group;
pub mod inherit;
pub mod jump;
pub mod known_hosts;
pub mod layout;
pub mod master_key;
pub mod migrate;
pub mod model;
pub mod network;
pub mod sftp;
pub mod tunnel;
pub mod vault;

pub use automation::{
    build_plan, AutomationCommand, AutomationPrefs, EnvVar, ResolvedAutomation, Step, TmuxChoice,
};
pub use error::StoreError;
pub use group::GroupRecord;
pub use inherit::{resolve, PrefsLayer, ResolvedConfig, DEFAULT_SCROLLBACK};
pub use known_hosts::{HostKeyEntry, KnownHostsFile};
pub use layout::{
    SavedDir, SavedLayout, SavedNodeEntry, SavedTab, SavedTabKind, SavedWindow,
    CURRENT_LAYOUT_SCHEMA,
};
pub use master_key::{InMemoryKey, KeyringSource, MasterKeySource};
pub use migrate::{migrate_v1, SchemaProbe};
pub use model::{
    AppearancePrefs, Auth, AuthKind, ColorSpec, ColorTarget, Connection, GroupId, IconKind,
    IconSpec, Identity, Protocol, SecretEntry, SessionId, SessionRecord, TerminalPrefs,
};
pub use network::{JumpRef, NetworkPrefs, ProxyChoice, ProxyEndpoint};
pub use sftp::{Bookmark, SftpPrefs};
pub use tunnel::{TunnelId, TunnelKind, TunnelRecord};
pub use vault::{SessionDraft, TunnelDraft, Vault};
