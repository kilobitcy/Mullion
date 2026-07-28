# Plan A1 — `mullion-store` crate 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增第 5 个 crate `mullion-store`——无头、可纯单测的会话与凭据保险库:明文 TOML 存非敏感字段、XChaCha20-Poly1305 加密存密码/私钥口令,主密钥走 OS keyring(可注入以便测试)。

**Architecture:** 纯同步 IO 库,零 UI/GPU/async,不依赖 core/term/ssh。`Vault` 收一个显式 `dir: PathBuf`(app 用 `directories` 算好传入),CRUD 方法的时间戳由调用方注入(保持确定性、可测)。敏感字段与非敏感字段落两个文件、各自 tmp+rename 原子写。主密钥来源抽成 `MasterKeySource` trait:真实用 keyring,测试用内存实现。

**Tech Stack:** Rust 2021 · `serde`(derive)· `toml` 0.8 · `chacha20poly1305` 0.10(XChaCha20-Poly1305)· `keyring` 3 · 错误**手写**(匹配项目既有 `ConnectError` 风格,不引 thiserror)· 测试用 `tempfile`。

> 关联 spec:`docs/superpowers/specs/2026-07-25-app-shell-session-manager-design.md`(切片 A)。
> 覆盖:§2.1(store crate)、§3(数据模型+持久化+加密+F70)、§6.1(无头守护测试)。
> **不含**:egui/状态机/会话 UI/连接(那是 Plan A2)。

---

## 文件结构

```
Cargo.toml                              修改:members 加 mullion-store;workspace.dependencies 加 serde/toml/chacha20poly1305/keyring
crates/mullion-store/Cargo.toml         新建:crate 清单
crates/mullion-store/src/lib.rs         新建:模块声明 + 再导出
crates/mullion-store/src/error.rs       新建:StoreError(手写 Display + Error + From)
crates/mullion-store/src/model.rs       新建:SessionId/Protocol/AuthKind/SecretEntry/SessionRecord
crates/mullion-store/src/crypto.rs      新建:encrypt/decrypt(XChaCha20-Poly1305,nonce 前置)
crates/mullion-store/src/master_key.rs  新建:MasterKeySource trait + KeyringSource + InMemoryKey
crates/mullion-store/src/vault.rs       新建:Vault(open/save/CRUD/原子写/id 完整性)
crates/mullion-store/tests/f70_no_plaintext.rs  新建:F70 集成守护测试
```

每文件单一职责:`model` 只放数据类型;`crypto` 只做字节进字节出的 AEAD;`master_key` 只管密钥来源;`vault` 是唯一碰文件系统的地方;`error` 汇总错误。

---

## Task 0：脚手架 + workspace 接线 + 依赖 pin/验签

**Files:**
- Modify: `Cargo.toml`(workspace)
- Create: `crates/mullion-store/Cargo.toml`
- Create: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: workspace 加成员与依赖**

改 `Cargo.toml`,`members` 末尾加一行,`[workspace.dependencies]` 末尾加四行:

```toml
members = [
    "crates/mullion-core",
    "crates/mullion-term",
    "crates/mullion-ssh",
    "crates/mullion-app",
    "crates/mullion-store",
]
```

```toml
# 切片 A:会话/凭据持久化(见 spec §3)
serde = { version = "1", features = ["derive"] }
toml = "0.8"
chacha20poly1305 = "0.10"                        # XChaCha20-Poly1305;稳定版 API(非 context7 的 bleeding-edge)
keyring = { version = "3", default-features = false, features = ["sync-secret-service", "crypto-rust", "windows-native", "apple-native"] }
```

- [ ] **Step 2: 建 crate 清单**

`crates/mullion-store/Cargo.toml`:

```toml
[package]
name = "mullion-store"
version.workspace = true
edition.workspace = true
license.workspace = true

# 架构不变量:store 只做持久化,不认识 core/term/ssh/app。
[dependencies]
serde.workspace = true
toml.workspace = true
chacha20poly1305.workspace = true
keyring.workspace = true

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: 建空 lib**

`crates/mullion-store/src/lib.rs`:

```rust
//! mullion-store —— 会话与凭据持久化。无头、可纯单测,零 UI/GPU/async。
//! 依赖方向:app → store;store 不依赖 core/term/ssh。
```

- [ ] **Step 4: 验依赖解析与 keyring feature 名**

Run: `cargo build -p mullion-store`
Expected: 编译通过(空 lib)。

若 `keyring` 报 feature 名不存在:按铁律核当前版本 feature——`cargo doc -p keyring --no-deps` 或读
`~/.cargo/registry/src/**/keyring-3*/Cargo.toml` 的 `[features]`,把 Step 1 的 features 改成该版本实际的
「secret-service(纯 Rust,即 crypto-rust)+ windows-native + apple-native」组合,再重跑本步。**不要**凭记忆硬写。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/mullion-store/Cargo.toml crates/mullion-store/src/lib.rs
git commit -m "feat(store): 脚手架 mullion-store crate + workspace 接线 (切片 A/§2.1)"
```

---

## Task 1：`StoreError`(手写,匹配项目错误风格)

**Files:**
- Create: `crates/mullion-store/src/error.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

在 `error.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_and_io_have_distinct_messages() {
        let a = StoreError::Crypto.to_string();
        let b = StoreError::Io("x".into()).to_string();
        assert!(!a.is_empty() && !b.is_empty());
        assert_ne!(a, b, "不同错误消息必须可区分");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib error`
Expected: FAIL —— `error` 模块/`StoreError` 尚未定义。

- [ ] **Step 3: 写实现**

`error.rs` 顶部(测试之上):

```rust
//! store 的错误。手写 Display + std::error::Error,匹配项目既有 `ConnectError` 风格(不引 thiserror)。

use std::fmt;

use crate::model::SessionId;

#[derive(Debug)]
pub enum StoreError {
    /// 文件读写失败。
    Io(String),
    /// TOML 序列化失败。
    TomlSer(String),
    /// TOML 解析失败(文件被手改坏)。
    TomlDe(String),
    /// 加解密失败:密钥错误或密文被篡改/损坏。
    Crypto,
    /// keyring 里的主密钥长度非 32。
    CorruptKey,
    /// OS keyring 访问失败(缺 Secret Service 等)。
    Keyring(String),
    /// secrets.enc 解密后非法 UTF-8。
    Utf8,
    /// 目标会话不存在。
    NotFound(SessionId),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "文件读写失败:{e}"),
            StoreError::TomlSer(e) => write!(f, "TOML 序列化失败:{e}"),
            StoreError::TomlDe(e) => write!(f, "TOML 解析失败(文件可能被手改坏):{e}"),
            StoreError::Crypto => write!(f, "加解密失败 —— 密钥错误或密文损坏"),
            StoreError::CorruptKey => write!(f, "主密钥损坏(长度非 32 字节)"),
            StoreError::Keyring(e) => write!(f, "系统密钥库访问失败:{e} —— 检查 keyring/Secret Service"),
            StoreError::Utf8 => write!(f, "secrets.enc 解密后非法 UTF-8"),
            StoreError::NotFound(id) => write!(f, "会话不存在:{id:?}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}
impl From<toml::ser::Error> for StoreError {
    fn from(e: toml::ser::Error) -> Self {
        StoreError::TomlSer(e.to_string())
    }
}
impl From<toml::de::Error> for StoreError {
    fn from(e: toml::de::Error) -> Self {
        StoreError::TomlDe(e.to_string())
    }
}
impl From<std::string::FromUtf8Error> for StoreError {
    fn from(_: std::string::FromUtf8Error) -> Self {
        StoreError::Utf8
    }
}
impl From<keyring::Error> for StoreError {
    fn from(e: keyring::Error) -> Self {
        StoreError::Keyring(e.to_string())
    }
}
```

`lib.rs` 追加:

```rust
pub mod error;
pub mod model;

pub use error::StoreError;
```

> `error.rs` 引用 `crate::model::SessionId`,故本步同时声明 `model` 模块;`model.rs` 的内容在 Task 2 写。
> 为让本 Task 能独立编译,先在 `crates/mullion-store/src/model.rs` 建占位类型:
> ```rust
> //! 会话数据模型。Task 2 填充。
> #[derive(Debug)]
> pub struct SessionId(pub u64);
> ```
> Task 2 会把它替换成完整定义。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib error`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/error.rs crates/mullion-store/src/lib.rs crates/mullion-store/src/model.rs
git commit -m "feat(store): 手写 StoreError(匹配项目错误风格,不引 thiserror)"
```

---

## Task 2：数据模型 `SessionRecord` + serde round-trip

**Files:**
- Modify: `crates/mullion-store/src/model.rs`(替换占位)

- [ ] **Step 1: 写失败测试**

`model.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_toml_round_trips() {
        let rec = SessionRecord {
            id: SessionId(7),
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: "跳板后".into(),
            modified_at: "2026-07-25T00:00:00Z".into(),
            auth: AuthKind::PublicKey {
                path: "/path/to/key.pem".into(),
                has_passphrase: false,
            },
        };
        // [[session]] 数组结构靠 SessionsFile 包裹
        let file = SessionsFile { session: vec![rec.clone()] };
        let s = toml::to_string_pretty(&file).unwrap();
        let back: SessionsFile = toml::from_str(&s).unwrap();
        assert_eq!(back.session, vec![rec]);
    }

    #[test]
    fn empty_toml_parses_to_no_sessions() {
        let back: SessionsFile = toml::from_str("").unwrap();
        assert!(back.session.is_empty(), "空文件应解析为零会话,不报错");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib model`
Expected: FAIL —— `Protocol`/`AuthKind`/`SessionRecord`/`SessionsFile` 未定义。

- [ ] **Step 3: 写实现**

把 `model.rs` 顶部(测试之上)整个替换为:

```rust
//! 会话数据模型。只放数据类型,零 IO。非敏感字段落明文 TOML;密码/口令走加密侧车(vault)。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 会话稳定主键。新建时取现有 max+1(见 vault)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// 会话协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ssh,
    Sftp,
}

/// 认证方式的**非敏感**部分。真正的密码/口令在 `SecretEntry`(加密)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthKind {
    /// 密码认证:密码串存加密侧车。
    Password,
    /// 公钥认证:私钥 path 明文;口令(若有)存加密侧车。
    PublicKey { path: PathBuf, has_passphrase: bool },
}

/// 一条会话(非敏感字段)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
    pub user: String,
    #[serde(default)]
    pub note: String,
    /// RFC3339;由调用方(app)注入,store 不持有时钟。
    pub modified_at: String,
    pub auth: AuthKind,
}

/// 一条会话的**敏感**部分,加密后存 secrets.enc。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntry {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub passphrase: Option<String>,
}

/// sessions.toml 的顶层结构:产生 `[[session]]` 数组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsFile {
    #[serde(default)]
    pub session: Vec<SessionRecord>,
}
```

`lib.rs` 追加再导出:

```rust
pub use model::{AuthKind, Protocol, SecretEntry, SessionId, SessionRecord};
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib model`
Expected: PASS(两个测试)。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/model.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): SessionRecord/Protocol/AuthKind/SecretEntry + serde round-trip"
```

---

## Task 3：`crypto.rs` —— XChaCha20-Poly1305 加解密

**Files:**
- Create: `crates/mullion-store/src/crypto.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`crypto.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let key = [7u8; 32];
        let msg = b"hunter2 the secret";
        let blob = encrypt(&key, msg).unwrap();
        assert_ne!(&blob[24..], msg, "密文不得等于明文");
        let back = decrypt(&key, &blob).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn wrong_key_fails_not_panics() {
        let blob = encrypt(&[1u8; 32], b"x").unwrap();
        assert!(matches!(decrypt(&[2u8; 32], &blob), Err(StoreError::Crypto)));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = [3u8; 32];
        let mut blob = encrypt(&key, b"abcdef").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff; // 篡改 tag/密文
        assert!(matches!(decrypt(&key, &blob), Err(StoreError::Crypto)));
    }

    #[test]
    fn short_blob_is_crypto_error() {
        assert!(matches!(decrypt(&[0u8; 32], &[0u8; 10]), Err(StoreError::Crypto)));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib crypto`
Expected: FAIL —— `crypto` 模块未定义。

- [ ] **Step 3: 写实现**

`crypto.rs` 顶部:

```rust
//! secrets.enc 的 AEAD:XChaCha20-Poly1305,24 字节随机 nonce 前置于密文。
//! 只做「字节进字节出」,不碰文件系统。

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::error::StoreError;

const NONCE_LEN: usize = 24;

/// 用 32 字节密钥加密;输出 = 24 字节 nonce ‖ 密文(含 16 字节 tag)。
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, StoreError> {
    let cipher = XChaCha20Poly1305::new(Key::<XChaCha20Poly1305>::from_slice(key));
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng); // 24 字节,每条消息唯一
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| StoreError::Crypto)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解密 `encrypt` 产出的 blob。密钥错/被篡改/过短 → `StoreError::Crypto`。
pub fn decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, StoreError> {
    if blob.len() < NONCE_LEN {
        return Err(StoreError::Crypto);
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::<XChaCha20Poly1305>::from_slice(key));
    let nonce = XNonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|_| StoreError::Crypto)
}
```

`lib.rs` 追加:

```rust
pub mod crypto;
```

> **验签(按 CLAUDE.md「API 漂移」铁律)**:若 `aead::{Aead, AeadCore, KeyInit, OsRng}` 或 `generate_nonce`
> 导入不解析,说明锁定的 0.10 API 与此处不符——读 `~/.cargo/registry/src/**/chacha20poly1305-0.10*/src/lib.rs`
> 顶部 doc 示例核实,按实际签名改导入/调用。**不要**用 context7 给的 `::generate()` 无参形式(那是未发布版)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib crypto`
Expected: PASS(四个测试)。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/crypto.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): XChaCha20-Poly1305 加解密(nonce 前置)+ 篡改/错密钥测试"
```

---

## Task 4：`master_key.rs` —— 主密钥来源(可注入)

**Files:**
- Create: `crates/mullion-store/src/master_key.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`master_key.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_returns_fixed_key() {
        let src = InMemoryKey([9u8; 32]);
        assert_eq!(src.load_or_create().unwrap(), [9u8; 32]);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib master_key`
Expected: FAIL —— `InMemoryKey`/`MasterKeySource` 未定义。

- [ ] **Step 3: 写实现**

`master_key.rs` 顶部:

```rust
//! 主密钥来源。真实实现走 OS keyring;测试用内存实现,让加密逻辑在无头 CI 可测。

use chacha20poly1305::aead::{KeyInit, OsRng};
use chacha20poly1305::XChaCha20Poly1305;

use crate::error::StoreError;

/// 取 32 字节主密钥;不存在则生成并持久化。
pub trait MasterKeySource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError>;
}

/// 生产实现:主密钥存 OS keyring(Windows DPAPI / macOS Keychain / Linux Secret Service)。
pub struct KeyringSource {
    pub service: String,
    pub account: String,
}

impl KeyringSource {
    /// 默认 service=`mullion`、account=`vault-master-key`。
    pub fn new() -> Self {
        Self {
            service: "mullion".into(),
            account: "vault-master-key".into(),
        }
    }
}

impl Default for KeyringSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterKeySource for KeyringSource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError> {
        let entry = keyring::Entry::new(&self.service, &self.account)?;
        match entry.get_secret() {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes.try_into().map_err(|_| StoreError::CorruptKey)?;
                Ok(arr)
            }
            Err(keyring::Error::NoEntry) => {
                // 复用 cipher 的密钥生成器(内部走 getrandom),免引 rand_core/getrandom 直接依赖。
                let key = XChaCha20Poly1305::generate_key(&mut OsRng);
                let arr: [u8; 32] = key.into();
                entry.set_secret(&arr)?;
                Ok(arr)
            }
            Err(e) => Err(StoreError::Keyring(e.to_string())),
        }
    }
}

/// 测试用:返回固定密钥,不碰 keyring。
pub struct InMemoryKey(pub [u8; 32]);

impl MasterKeySource for InMemoryKey {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError> {
        Ok(self.0)
    }
}
```

`lib.rs` 追加:

```rust
pub mod master_key;

pub use master_key::{InMemoryKey, KeyringSource, MasterKeySource};
```

> **验签**:`entry.get_secret()`/`set_secret(&[u8])`/`keyring::Error::NoEntry` 是 keyring 3.x API(已用
> context7 核过)。`XChaCha20Poly1305::generate_key(&mut OsRng)` 返回的 `Key` 能否 `.into()` 成 `[u8;32]`
> 取决于 0.10 的 `Key` 是否 `GenericArray<u8, U32>`——若 `into()` 不通,改用 `let mut arr=[0u8;32]; arr.copy_from_slice(&key); `。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib master_key`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/master_key.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): MasterKeySource trait + KeyringSource(真实)+ InMemoryKey(测试)"
```

---

## Task 5：`Vault` 骨架 —— open/save + 原子写(空库)

**Files:**
- Create: `crates/mullion-store/src/vault.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`vault.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_key::InMemoryKey;

    fn key() -> InMemoryKey {
        InMemoryKey([5u8; 32])
    }

    #[test]
    fn open_empty_dir_has_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(vault.list().is_empty());
    }

    #[test]
    fn save_writes_both_files_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        vault.save().unwrap();
        assert!(dir.path().join("sessions.toml").exists());
        assert!(dir.path().join("secrets.enc").exists());
        // 无残留临时文件
        assert!(!dir.path().join("sessions.tmp").exists());
        assert!(!dir.path().join("secrets.tmp").exists());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib vault`
Expected: FAIL —— `Vault` 未定义。

- [ ] **Step 3: 写实现**

`vault.rs` 顶部:

```rust
//! Vault:唯一碰文件系统的地方。sessions.toml(明文非敏感)+ secrets.enc(加密敏感)。
//! 两文件各自 tmp+rename 原子写;时间戳由调用方注入(store 不持时钟)。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto;
use crate::error::StoreError;
use crate::master_key::MasterKeySource;
use crate::model::{SecretEntry, SessionRecord, SessionsFile};

/// id.to_string() → 敏感条目。
type SecretMap = BTreeMap<String, SecretEntry>;

pub struct Vault {
    dir: PathBuf,
    sessions: Vec<SessionRecord>,
    secrets: SecretMap,
    key: [u8; 32],
}

impl Vault {
    fn sessions_path(&self) -> PathBuf {
        self.dir.join("sessions.toml")
    }
    fn secrets_path(&self) -> PathBuf {
        self.dir.join("secrets.enc")
    }

    /// 打开(或初始化)`dir` 下的保险库。dir 由调用方(app)算好(directories)传入。
    pub fn open(dir: PathBuf, key_source: &dyn MasterKeySource) -> Result<Self, StoreError> {
        fs::create_dir_all(&dir)?;
        let key = key_source.load_or_create()?;

        let sessions_path = dir.join("sessions.toml");
        let sessions = if sessions_path.exists() {
            let text = fs::read_to_string(&sessions_path)?;
            let file: SessionsFile = toml::from_str(&text)?;
            file.session
        } else {
            Vec::new()
        };

        let secrets_path = dir.join("secrets.enc");
        let secrets = if secrets_path.exists() {
            let blob = fs::read(&secrets_path)?;
            let plain = crypto::decrypt(&key, &blob)?;
            let text = String::from_utf8(plain)?;
            toml::from_str::<SecretMap>(&text)?
        } else {
            SecretMap::new()
        };

        Ok(Self {
            dir,
            sessions,
            secrets,
            key,
        })
    }

    /// 落盘:两文件各自原子写。
    pub fn save(&self) -> Result<(), StoreError> {
        let file = SessionsFile {
            session: self.sessions.clone(),
        };
        let toml_text = toml::to_string_pretty(&file)?;
        write_atomic(&self.sessions_path(), toml_text.as_bytes())?;

        let secret_text = toml::to_string_pretty(&self.secrets)?;
        let blob = crypto::encrypt(&self.key, secret_text.as_bytes())?;
        write_atomic(&self.secrets_path(), &blob)?;
        Ok(())
    }

    pub fn list(&self) -> &[SessionRecord] {
        &self.sessions
    }
}

/// tmp + rename 原子写:防写到一半崩溃导致两文件 desync。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
```

`lib.rs` 追加:

```rust
pub mod vault;

pub use vault::Vault;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib vault`
Expected: PASS(两个测试)。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): Vault open/save 骨架 + tmp+rename 原子写(空库)"
```

---

## Task 6：`Vault` 新增会话 —— `add` + id=max+1 + 注入时间戳

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

在 `vault.rs` 的 `#[cfg(test)] mod tests` 里追加:

```rust
    use crate::model::{AuthKind, Protocol, SecretEntry, SessionId};

    fn draft_pw(name: &str, pw: &str) -> SessionDraft {
        SessionDraft {
            name: name.into(),
            host: "h".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "u".into(),
            note: String::new(),
            auth: AuthKind::Password,
            secret: Some(SecretEntry {
                password: Some(pw.into()),
                passphrase: None,
            }),
        }
    }

    #[test]
    fn add_allocates_incrementing_ids_and_stamps_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id1 = vault.add(draft_pw("a", "p1"), "2026-07-25T00:00:00Z");
        let id2 = vault.add(draft_pw("b", "p2"), "2026-07-25T00:00:01Z");
        assert_eq!(id1, SessionId(1));
        assert_eq!(id2, SessionId(2));
        assert_eq!(vault.list().len(), 2);
        assert_eq!(vault.get(id1).unwrap().modified_at, "2026-07-25T00:00:00Z");
        assert_eq!(vault.secret(id1).unwrap().password.as_deref(), Some("p1"));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib vault::tests::add_allocates`
Expected: FAIL —— `SessionDraft`/`add`/`get`/`secret` 未定义。

- [ ] **Step 3: 写实现**

在 `vault.rs` 里 `use` 区补 `SessionId`,并在文件里(`Vault` impl 之上)加 `SessionDraft`:

```rust
use crate::model::SessionId;

/// 新建/编辑会话的输入(不含 id/modified_at,由 vault 分配/注入)。
pub struct SessionDraft {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: crate::model::Protocol,
    pub user: String,
    pub note: String,
    pub auth: crate::model::AuthKind,
    /// 敏感部分(密码/口令);无则 None。
    pub secret: Option<SecretEntry>,
}
```

在 `impl Vault` 里 `list` 之后加:

```rust
    pub fn get(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn secret(&self, id: SessionId) -> Option<&SecretEntry> {
        self.secrets.get(&id.0.to_string())
    }

    /// 新增会话。id 取现有 max+1(空库从 1 起);modified_at 由调用方注入。
    pub fn add(&mut self, draft: SessionDraft, now_rfc3339: &str) -> SessionId {
        let id = SessionId(self.sessions.iter().map(|s| s.id.0).max().map_or(1, |m| m + 1));
        if let Some(sec) = draft.secret {
            self.secrets.insert(id.0.to_string(), sec);
        }
        self.sessions.push(SessionRecord {
            id,
            name: draft.name,
            host: draft.host,
            port: draft.port,
            protocol: draft.protocol,
            user: draft.user,
            note: draft.note,
            modified_at: now_rfc3339.to_string(),
            auth: draft.auth,
        });
        id
    }
```

`lib.rs` 更新再导出:

```rust
pub use vault::{SessionDraft, Vault};
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib vault`
Expected: PASS(全部 vault 测试)。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): Vault::add(id=max+1 + 注入时间戳)+ get/secret"
```

---

## Task 7：`Vault` 改/删 —— `update`/`delete` + id 完整性

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

在 `mod tests` 追加:

```rust
    /// 保存/删除后不变量:secrets 的 key 集合 ⊆ 会话 id 集合(无孤儿密文)。
    fn assert_no_orphan_secrets(vault: &Vault) {
        let ids: std::collections::BTreeSet<String> =
            vault.list().iter().map(|s| s.id.0.to_string()).collect();
        for k in vault.secrets_keys_for_test() {
            assert!(ids.contains(&k), "孤儿密文 id={k}");
        }
    }

    #[test]
    fn update_replaces_fields_and_restamps() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = vault.add(draft_pw("a", "p1"), "2026-07-25T00:00:00Z");
        let mut d = draft_pw("a-renamed", "p1-new");
        d.host = "newhost".into();
        vault.update(id, d, "2026-07-25T09:00:00Z").unwrap();
        let rec = vault.get(id).unwrap();
        assert_eq!(rec.name, "a-renamed");
        assert_eq!(rec.host, "newhost");
        assert_eq!(rec.modified_at, "2026-07-25T09:00:00Z");
        assert_eq!(vault.secret(id).unwrap().password.as_deref(), Some("p1-new"));
    }

    #[test]
    fn update_missing_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let r = vault.update(SessionId(99), draft_pw("x", "p"), "t");
        assert!(matches!(r, Err(StoreError::NotFound(SessionId(99)))));
    }

    #[test]
    fn delete_removes_session_and_purges_secret() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = vault.add(draft_pw("a", "p1"), "t");
        vault.delete(id).unwrap();
        assert!(vault.get(id).is_none());
        assert!(vault.secret(id).is_none(), "删除必须连带清密文(id 完整性)");
        assert_no_orphan_secrets(&vault);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib vault`
Expected: FAIL —— `update`/`delete`/`secrets_keys_for_test` 未定义。

- [ ] **Step 3: 写实现**

在 `impl Vault`(`add` 之后)加:

```rust
    /// 编辑现有会话:替换非敏感字段、重打时间戳、覆盖敏感部分。
    pub fn update(
        &mut self,
        id: SessionId,
        draft: SessionDraft,
        now_rfc3339: &str,
    ) -> Result<(), StoreError> {
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.name = draft.name;
        rec.host = draft.host;
        rec.port = draft.port;
        rec.protocol = draft.protocol;
        rec.user = draft.user;
        rec.note = draft.note;
        rec.auth = draft.auth;
        rec.modified_at = now_rfc3339.to_string();
        match draft.secret {
            Some(sec) => {
                self.secrets.insert(id.0.to_string(), sec);
            }
            None => {
                self.secrets.remove(&id.0.to_string());
            }
        }
        Ok(())
    }

    /// 删除会话,并**连带清除**其密文(守 id 完整性,见 spec §3.1)。
    pub fn delete(&mut self, id: SessionId) -> Result<(), StoreError> {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        if self.sessions.len() == before {
            return Err(StoreError::NotFound(id));
        }
        self.secrets.remove(&id.0.to_string());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn secrets_keys_for_test(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib vault`
Expected: PASS(全部)。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/vault.rs
git commit -m "feat(store): Vault::update/delete + 删除连带清密文(id 完整性)"
```

---

## Task 8：F70 守护测试 —— 磁盘搜不到明文口令

**Files:**
- Create: `crates/mullion-store/tests/f70_no_plaintext.rs`

- [ ] **Step 1: 写失败测试**

`crates/mullion-store/tests/f70_no_plaintext.rs`:

```rust
//! F70 守护:写一条带密码/私钥口令的会话并落盘后,sessions.toml 与 secrets.enc 的
//! 原始字节里都搜不到明文口令。用 InMemoryKey 保证确定性。

use mullion_store::{AuthKind, InMemoryKey, Protocol, SecretEntry, SessionDraft, Vault};

const PW: &str = "hunter2-VERY-secret-passphrase-xyz";

#[test]
fn plaintext_secret_never_hits_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(dir.path().to_path_buf(), &InMemoryKey([42u8; 32])).unwrap();
    vault.add(
        SessionDraft {
            name: "s".into(),
            host: "h".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "u".into(),
            note: String::new(),
            auth: AuthKind::PublicKey {
                path: "/k.pem".into(),
                has_passphrase: true,
            },
            secret: Some(SecretEntry {
                password: None,
                passphrase: Some(PW.into()),
            }),
        },
        "2026-07-25T00:00:00Z",
    );
    vault.save().unwrap();

    let toml_bytes = std::fs::read(dir.path().join("sessions.toml")).unwrap();
    let enc_bytes = std::fs::read(dir.path().join("secrets.enc")).unwrap();
    let needle = PW.as_bytes();
    assert!(
        !contains(&toml_bytes, needle),
        "sessions.toml 里出现了明文口令"
    );
    assert!(
        !contains(&enc_bytes, needle),
        "secrets.enc 里出现了明文口令"
    );

    // 反证:同一密钥能解回明文,确保不是「加密了但丢了数据」。
    let reopened = Vault::open(dir.path().to_path_buf(), &InMemoryKey([42u8; 32])).unwrap();
    let id = reopened.list()[0].id;
    assert_eq!(
        reopened.secret(id).unwrap().passphrase.as_deref(),
        Some(PW)
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}
```

- [ ] **Step 2: 跑测试确认失败(或先确认它能编译并跑)**

Run: `cargo test -p mullion-store --test f70_no_plaintext`
Expected: 若前序 Task 都在,应直接 PASS;若 `contains` 逻辑或导出名不符会 FAIL——本测试的价值在于回归守护。
先确认它**跑起来**;若 PASS 即达标(F70)。

> 说明:此测试不引入新实现,是纯守护测试。若希望严格遵循「先红」,可临时把 `save()` 里
> `crypto::encrypt(...)` 换成 `Ok(secret_text.into_bytes())`(明文直写)跑一次,看它 FAIL,再改回加密看它 PASS,
> 以证明这条断言真的能抓到明文落盘。改回后再进 Step 3。

- [ ] **Step 3: 确认通过**

Run: `cargo test -p mullion-store --test f70_no_plaintext`
Expected: PASS。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-store/tests/f70_no_plaintext.rs
git commit -m "test(store): F70 守护 —— sessions.toml/secrets.enc 磁盘字节搜不到明文口令"
```

---

## Task 9：reload round-trip + 全绿(clippy/fmt/workspace)

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`(加一个 reload 测试)

- [ ] **Step 1: 写 reload round-trip 测试**

在 `vault.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn save_then_reopen_preserves_everything() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = vault.add(draft_pw("a", "secretpw"), "2026-07-25T00:00:00Z");
            vault.save().unwrap();
        }
        let reopened = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.get(id).unwrap().name, "a");
        assert_eq!(
            reopened.secret(id).unwrap().password.as_deref(),
            Some("secretpw")
        );
    }
```

- [ ] **Step 2: 跑该测试**

Run: `cargo test -p mullion-store --lib vault::tests::save_then_reopen`
Expected: PASS。

- [ ] **Step 3: 整 crate 测试**

Run: `cargo test -p mullion-store`
Expected: 全部 PASS(lib 各模块 + f70 集成)。

- [ ] **Step 4: workspace 全绿 + clippy + fmt**

Run:
```bash
cargo test --workspace > /tmp/store-test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/store-test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 无 FAILED/panicked;clippy 无输出;fmt 无 diff。

> 「绿」的定义(项目 CLAUDE.md):`cargo test --workspace` 全过 **且** `clippy -D warnings` 无输出。
> 若 clippy 抱怨(如 `KeyringSource::new` 无参可用 `Default` 等),按提示改;不许 `#[allow]` 绕过。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/vault.rs
git commit -m "test(store): save→reopen round-trip;workspace 全绿(clippy -D warnings 无输出)"
```

---

## Task 10：文档 —— CLAUDE.md 架构表(4→5 crate)+ ADR-006

**Files:**
- Modify: `CLAUDE.md`(架构不变量表 + 目录约定)
- Create: `docs/adr-006-session-store-crate.md`

- [ ] **Step 1: CLAUDE.md 架构表加 store**

在 `CLAUDE.md`「架构不变量」代码块里,`mullion-app` 行之上加一行,并更新依赖方向:

```
mullion-store    会话/凭据持久化。TOML + keyring 加密。零 UI、零 async、仅同步 IO。可纯单测。
```

把依赖方向那句改为:

```
**依赖方向严格单向**:`app → {core, term, ssh, store}`,其余互不依赖。
```

- [ ] **Step 2: 写 ADR-006**

`docs/adr-006-session-store-crate.md`:

```markdown
# ADR-006: 新增 mullion-store crate(会话/凭据持久化)

- 状态: 已接受
- 日期: 2026-07-25
- 关联: spec.md F70/F71、ADR-002(TOML)、切片 A spec

## 背景

切片 A 要「无参启动 + 会话增删改查 + 一次配置认证一直使用」,需要一个持久化层:
非敏感字段可 diff 的 TOML(承 ADR-002),密码/私钥口令加密。这段逻辑要能无头单测(F70)。

## 决策

新增第 5 个 crate `mullion-store`,承载 `SessionRecord` + TOML 读写 + 敏感字段加密。
依赖方向 `app → {core, term, ssh, store}`,store 不依赖其余任何 crate。app 做整合者
(SessionRecord → SshConfig)。

## 备选与否决理由

- **塞进 mullion-app**:app 是最难测的 crate(winit/wgpu),把可测的持久化/加密逻辑埋进去,
  F70「磁盘搜不到明文」这类验收就只能带 GUI 测。否掉。
- **塞进 mullion-ssh**:违反「ssh 只认字节流,不认会话/窗口」的架构不变量。否掉。

## 关键实现取舍

- 存储沿用 ADR-002:`sessions.toml` 明文非敏感 + `secrets.enc` 加密 blob(XChaCha20-Poly1305)。
- 主密钥走 OS keyring(满足「一次配置一直使用」不再追问),来源抽成 `MasterKeySource` trait,
  测试用内存实现 → 加密逻辑无头可测。
- **F70 的 Argon2id 推迟到 F71**:Argon2id 是「从口令派生密钥」,无主密码时无输入;切片 A 用
  keyring 高熵随机主密钥即满足 F70 的 P0。Argon2id 待 F71 主密码层引入。
- 两文件各自 tmp+rename 原子写;删除会话连带清密文(id 完整性)。
- 错误手写(匹配项目既有 `ConnectError` 风格),不引 thiserror。

## 后果

- crate 数 4 → 5;CLAUDE.md 架构表同步更新。
- egui/状态机/会话 UI 属切片 A 的 Plan A2,其架构决策(egui 做外壳)另记 ADR 或在 A2 落地时补。
```

- [ ] **Step 3: 提交文档**

```bash
git add CLAUDE.md docs/adr-006-session-store-crate.md
git commit -m "docs: CLAUDE.md 架构表 4→5 crate + ADR-006(mullion-store 持久化)"
```

---

## 自查(写完计划的复盘)

- **Spec 覆盖**:§2.1 新 crate → Task 0;§3.1 数据模型 → Task 2;§3.2 磁盘布局/原子写 → Task 5;
  §3.3 加密/keyring/主密钥 trait/Argon2id 推迟 → Task 3+4;§3.1 id 完整性 → Task 7;
  §6.1 F70 无头守护 → Task 8;§8 文档动作 → Task 10。**均有对应 Task**。
- **超出 A1 的部分**(§4 UI、§5 连接、§4.5 路由纯函数、§4.2 rect→cols/rows、待定 F/G)→ **Plan A2**,本计划不含。
- **类型一致性**:`SessionId`/`Protocol`/`AuthKind`/`SecretEntry`/`SessionRecord`/`SessionsFile`(model)、
  `SessionDraft`/`Vault`/`SecretMap`(vault)、`MasterKeySource`/`KeyringSource`/`InMemoryKey`(master_key)
  在各 Task 间签名一致;`add/update/delete/get/secret/list/open/save` 命名全程一致。
- **验签点**已在 Task 0/3/4 标注(keyring feature 名、chacha20poly1305 0.10 API、Key→[u8;32] 转换)——
  按 CLAUDE.md「API 漂移」铁律对锁定版核实,不凭记忆。

## 落地后的下一步

A1 全绿后写 **Plan A2 · App 外壳**(egui 集成 + `Option<Connection>` 状态机 + 会话管理 UI + 统一异步 connect),
届时先 pin egui/egui-wgpu/egui-winit 对 winit 0.30 / wgpu 23 的兼容版本(spec「API 漂移」铁律)。
