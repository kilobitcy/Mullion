//! mullion-store —— 会话与凭据持久化。无头、可纯单测,零 UI/GPU/async。
//! 依赖方向:app → store;store 不依赖 core/term/ssh。

pub mod crypto;
pub mod error;
pub mod known_hosts;
pub mod master_key;
pub mod model;
pub mod vault;

pub use error::StoreError;
pub use known_hosts::{HostKeyEntry, KnownHostsFile};
pub use master_key::{InMemoryKey, KeyringSource, MasterKeySource};
pub use model::{AuthKind, Protocol, SecretEntry, SessionId, SessionRecord};
pub use vault::{SessionDraft, Vault};
