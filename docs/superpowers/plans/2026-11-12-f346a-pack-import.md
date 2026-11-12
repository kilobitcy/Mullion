# F346-a 手机导入 `.mullionpack` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 手机端从 `.mullionpack` 文件导入配置:元数据整份、密文只留项目可达的条目,用 Android Keystore 信封里的数据密钥重封;ffi 不具备任何回写能力。

**Architecture:** 判据(可达集、暂存-裁剪-提交)全在 `mullion-store::phone_import`(纯同步、可单测);`mullion-ffi::import` 只做类型转换与错误映射;Kotlin 侧 `VaultKey` 做 Keystore 信封,`PackImport`/`ImportViewModel` 做 SAF 选文件 → 口令 → 摘要。

**Tech Stack:** Rust(mullion-store / mullion-ffi,uniffi 0.32 proc-macro)、Kotlin + Compose、AndroidKeyStore AES-256-GCM、模拟器插桩测试。

**权威设计:** `docs/superpowers/specs/2026-11-12-f346a-pack-import-design.md`(下称「设计」)。分支 `slice/f346a`。

---

## 通用约定(每个 Task 都适用)

- 注释、提交信息用简体中文;提交摘要带 `(F346)`,尾注 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出 **且** `cargo fmt --check` 干净。大输出先落盘:
  `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`
- **私密信息(真机 IP / 用户名 / 私钥 / 凭据)永不进被跟踪文件**。测试数据一律 `*.example` 主机名与明显的假口令。
- Bash 访问 `android/app/build/` 下路径会被权限拒绝 —— 不要绕;要看生成的 Kotlin 绑定,另生成一份到 `/tmp`(Task 4 Step 6 给了命令)。
- 编译失败先看报错里的实际签名,别猜。同一处连续两次没修好就停下报告。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-store/src/phone_import.rs` | 新建 | `Reachable` / `project_reachable_secret_keys` / `PhoneImport` / `install_projects_only` + 全部 store 单测 |
| `crates/mullion-store/src/lib.rs` | 改 | `pub mod phone_import;` + `pub use` |
| `crates/mullion-store/src/portable.rs` | 改 | `fn b64()` → `pub(crate) fn b64()` |
| `crates/mullion-store/src/vault.rs` | 改 | `fn cred_key` → `pub(crate) fn cred_key` |
| `crates/mullion-store/src/master_key.rs` | 改 | `InMemoryKey` 文档改为「测试与 Android 生产路径」 |
| `crates/mullion-ffi/Cargo.toml` | 改 | 依赖 `mullion-store`;dev-deps 加 `toml`、`base64`、`tempfile` |
| `crates/mullion-ffi/src/error.rs` | 新建 | `FfiError`(从 terminal.rs 挪来)+ 新变体 + `From<StoreError>` |
| `crates/mullion-ffi/src/terminal.rs` | 改 | 删掉挪走的 `FfiError` 定义,改 `use crate::error::FfiError` |
| `crates/mullion-ffi/src/import.rs` | 新建 | `PackInfo` / `ImportSummary` / `inspect_pack` / `import_pack` |
| `crates/mullion-ffi/src/lib.rs` | 改 | `mod error; mod import;` + `pub use` |
| `crates/mullion-ffi/tests/common/demo_pack.rs` | 新建 | 演示包(假数据),测试与 example 共用 |
| `crates/mullion-ffi/tests/import_pack.rs` | 新建 | ffi 层导入测试 |
| `crates/mullion-ffi/tests/no_write_back.rs` | 新建 | 源码守护 |
| `crates/mullion-ffi/tests/dependency_direction.rs` | 改 | 加「不依赖 mullion-cloud」 |
| `crates/mullion-ffi/examples/make_test_pack.rs` | 新建 | 生成 Android 测试资产 |
| `android/app/src/androidTest/assets/test.mullionpack` | 新建(生成物,入库) | 插桩测试用演示包 |
| `android/app/src/main/java/io/github/kilobitcy/mullion/VaultKey.kt` | 新建 | Keystore 信封 |
| `android/app/src/main/java/io/github/kilobitcy/mullion/PackImport.kt` | 新建 | 取钥 → 导入 → 摘要文本 |
| `android/app/src/main/java/io/github/kilobitcy/mullion/ImportViewModel.kt` | 新建 | SAF 读文件、口令状态机 |
| `android/app/src/main/java/io/github/kilobitcy/mullion/MainActivity.kt` | 改 | 导入入口 |
| `android/app/src/main/AndroidManifest.xml` | 改 | `allowBackup="false"` + `dataExtractionRules` |
| `android/app/src/main/res/xml/data_extraction_rules.xml` | 新建 | 云备份与设备迁移都排除全部数据 |
| `android/app/src/androidTest/java/io/github/kilobitcy/mullion/VaultKeyTest.kt` | 新建 | 信封插桩测试 |
| `android/app/src/androidTest/java/io/github/kilobitcy/mullion/PackImportTest.kt` | 新建 | 端到端导入插桩测试 |
| `docs/android-build.md`、`spec.md` | 改 | 文档与状态 |

> 设计外补的一处:`dataExtractionRules`。Android 12+ 上 `allowBackup="false"` 只关云备份,**不关设备间迁移**;迁移过去的 `vault-key.bin` 解不开(Keystore 钥不跟着走),会落到 `VaultKey` 的重置分支 —— 不泄密,但用户会看到「本机密钥已失效」。显式排除更干净,且不改变任何判据。

---

### Task 1: store 可达集 `project_reachable_secret_keys`

**Files:**
- Create: `crates/mullion-store/src/phone_import.rs`
- Modify: `crates/mullion-store/src/lib.rs`(模块表加 `pub mod phone_import;`,`pub use` 区加一行)
- Modify: `crates/mullion-store/src/vault.rs:121`(`fn cred_key` → `pub(crate) fn cred_key`)

背景(实现者必读):
- `jump::expand_chain(target, &BTreeMap<SessionId, SessionRecord>, &BTreeMap<GroupId, GroupRecord>) -> Result<Vec<SessionId>, StoreError>`:返回**不含 target 自身**的整条跳板链,已经走分组继承;target 不存在 → `JumpDangling`;悬空/成环/超深 → `Err`。
- 会话的密文键是 `id.0.to_string()`;共享凭据的密文键是 `vault::cred_key(id)` = `"cred:<id>"`。
- 设计 D7:归档项目(`archived_at.is_some()`)**也算**可达。D8:展开失败只留节点自身,`broken_nodes` 计数(按节点去重)。

- [ ] **Step 1: 写失败的测试(新建文件,先只放类型桩与测试)**

`crates/mullion-store/src/phone_import.rs`:

```rust
//! F346-a:手机导入 `.mullionpack` —— 元数据整份,密文只留**项目可达**的那部分,
//! 用手机自己的数据密钥重封。零 UI、零 async、纯同步 IO(架构不变量)。
//!
//! 与桌面 [`crate::portable::install`] 的区别:
//! - 不备份:手机是只读消费者,真源在桌面,重导一次即恢复(设计 D4);
//! - 不写 `settings.toml`、`layouts/`(设计 D2);
//! - 未裁的全量密文**从不落盘**:裁剪在内存里做(设计 §3)。

use std::collections::{BTreeMap, BTreeSet};

use crate::credential::Auth;
use crate::group::GroupRecord;
use crate::model::{SessionId, SessionRecord};
use crate::project::ProjectRecord;
use crate::vault::cred_key;

/// 项目可达的密文键集合。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reachable {
    /// `secrets.enc` 里要保留的键:会话 id 的十进制串,以及 `cred:<id>`。
    pub keys: BTreeSet<String>,
    /// 跳板链展开失败的节点数(按节点去重)。**非零必须报给用户**(设计 D8):
    /// 「桌面能连、手机说跳板悬空」要事先看得见。
    pub broken_nodes: u32,
}

/// 对**每个项目(含归档,设计 D7)**的每个节点,收集「节点自身 + 整条跳板链」
/// 的会话,取它们的密文键和它们 `Auth::Ref` 引用的 `cred:` 键。
///
/// 展开失败的节点只留它自己(设计 D8):`expand_chain` 全有或全无,
/// 不能让一条坏配置拖垮整次导入。
pub fn project_reachable_secret_keys(
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    projects: &[ProjectRecord],
) -> Reachable {
    let _ = (sessions, groups, projects);
    Reachable::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::CredentialId;
    use crate::model::{AppearancePrefs, AuthKind, Connection, GroupId, Identity, Protocol, TerminalPrefs};
    use crate::network::{JumpRef, NetworkPrefs};
    use crate::project::ProjectId;

    /// `jump`:`None` = 未设置(走分组继承);`Some(&[])` = 显式不走跳板。
    fn session(id: u64, jump: Option<&[u64]>, auth: Auth) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-11-12T00:00:00Z".into(),
            identity: Identity {
                name: format!("s{id}"),
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
                jump: jump.map(|j| j.iter().map(|i| JumpRef(SessionId(*i))).collect()),
            },
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
            last_connected_at: None,
        }
    }

    fn inline() -> Auth {
        Auth::inline("u", AuthKind::Password)
    }

    fn group(id: u64, jump: &[u64]) -> GroupRecord {
        let mut g = GroupRecord {
            id: GroupId(id),
            name: format!("g{id}"),
            tags: Vec::new(),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
        };
        g.network.jump = Some(jump.iter().map(|i| JumpRef(SessionId(*i))).collect());
        g
    }

    fn project(id: u64, name: &str, nodes: &[u64]) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: nodes.iter().map(|n| SessionId(*n)).collect(),
            preferred: None,
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-11-12T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            memos: Vec::new(),
            icon: None,
        }
    }

    fn keys(r: &Reachable) -> Vec<&str> {
        r.keys.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_project_node_keeps_its_own_secret_and_nothing_else() {
        let s = [session(1, Some(&[]), inline()), session(2, Some(&[]), inline())];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"]);
        assert_eq!(r.broken_nodes, 0);
    }

    #[test]
    fn a_jump_inherited_from_the_group_is_kept() {
        let mut node = session(1, None, inline());
        node.identity.group_id = Some(GroupId(7));
        let s = [node, session(3, Some(&[]), inline())];
        let r = project_reachable_secret_keys(&s, &[group(7, &[3])], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1", "3"], "分组继承来的跳板 3 必须可达");
    }

    #[test]
    fn the_credential_referenced_by_a_jump_session_is_kept() {
        let s = [
            session(1, Some(&[3]), inline()),
            session(3, Some(&[]), Auth::Ref(CredentialId(5))),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1", "3", "cred:5"]);
    }

    #[test]
    fn nodes_of_an_archived_project_are_kept() {
        let mut p = project(1, "p", &[1]);
        p.archived_at = Some("2026-11-01T00:00:00Z".into());
        let r = project_reachable_secret_keys(&[session(1, Some(&[]), inline())], &[], &[p]);
        assert_eq!(keys(&r), ["1"], "设计 D7:手机能打开归档项目,其凭据必须在");
    }

    #[test]
    fn unrelated_sessions_and_unused_credentials_are_dropped() {
        let s = [
            session(1, Some(&[]), inline()),
            session(4, Some(&[]), Auth::Ref(CredentialId(6))),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"], "会话 4 与它引用的 cred:6 都不在任何项目里");
    }

    #[test]
    fn a_jump_used_only_by_non_project_sessions_is_dropped() {
        let s = [
            session(1, Some(&[]), inline()),
            session(3, Some(&[]), inline()),
            session(4, Some(&[3]), inline()),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"]);
    }

    #[test]
    fn a_node_whose_chain_cannot_expand_keeps_only_itself_and_is_counted_once() {
        // 跳板 99 不存在 → expand_chain 失败。同一节点出现在两个项目里只计一次。
        let s = [session(1, Some(&[99]), inline())];
        let ps = [project(1, "p", &[1]), project(2, "q", &[1])];
        let r = project_reachable_secret_keys(&s, &[], &ps);
        assert_eq!(keys(&r), ["1"]);
        assert_eq!(r.broken_nodes, 1);
    }
}
```

`crates/mullion-store/src/lib.rs`:在 `pub mod known_hosts;` 等模块表里按字母序插入 `pub mod phone_import;`(在 `pub mod network;` 与 `pub mod portable;` 之间),并在 `pub use portable::{...};` 之后加:

```rust
pub use phone_import::{project_reachable_secret_keys, Reachable};
```

`crates/mullion-store/src/vault.rs:121`:

```rust
pub(crate) fn cred_key(id: CredentialId) -> String {
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store phone_import 2>&1 | tail -20`
Expected:除 `a_project_node...` 外多数 FAIL(断言 `keys == [...]` 得到 `[]`);编译通过。

- [ ] **Step 3: 实现**

把桩函数体替换为:

```rust
pub fn project_reachable_secret_keys(
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    projects: &[ProjectRecord],
) -> Reachable {
    let by_id: BTreeMap<SessionId, SessionRecord> =
        sessions.iter().map(|s| (s.id, s.clone())).collect();
    let groups: BTreeMap<_, _> = groups.iter().map(|g| (g.id, g.clone())).collect();

    let mut ids = BTreeSet::new();
    let mut broken = BTreeSet::new();
    for node in projects.iter().flat_map(|p| &p.nodes) {
        ids.insert(*node);
        match crate::jump::expand_chain(*node, &by_id, &groups) {
            Ok(chain) => ids.extend(chain),
            Err(_) => {
                broken.insert(*node);
            }
        }
    }

    let mut keys = BTreeSet::new();
    for id in ids {
        // 项目引用了不存在的会话:没有密文可留(展开那步已计入 broken)。
        let Some(rec) = by_id.get(&id) else { continue };
        keys.insert(id.0.to_string());
        if let Auth::Ref(c) = &rec.auth {
            keys.insert(cred_key(*c));
        }
    }
    Reachable {
        keys,
        broken_nodes: broken.len() as u32,
    }
}
```

(若 `CredentialId` 不是 `Copy`,把 `cred_key(*c)` 改成 `cred_key(c.clone())`;以编译器报错为准。)

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store phone_import 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok. 7 passed`

- [ ] **Step 5: clippy + fmt**

Run: `cargo clippy -p mullion-store --all-targets -- -D warnings && cargo fmt --check`
Expected: 无输出(fmt 有差异就 `cargo fmt` 后再查)。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/phone_import.rs crates/mullion-store/src/lib.rs crates/mullion-store/src/vault.rs
git commit -m "$(cat <<'EOF'
feat(store): 项目可达密文键集合,含跳板链与引用凭据 (F346)

归档项目算可达(D7);跳板链展开失败只留节点自身并计数(D8)。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: store `install_projects_only`(暂存 → 内存裁剪 → 重封 → 提交)

**Files:**
- Modify: `crates/mullion-store/src/phone_import.rs`
- Modify: `crates/mullion-store/src/portable.rs:371`(`fn b64()` → `pub(crate) fn b64()`)
- Modify: `crates/mullion-store/src/master_key.rs:60`(文档)
- Modify: `crates/mullion-store/src/lib.rs`(扩 `pub use`)

背景(实现者必读):
- `portable::read_pack(text)`:格式版本过新 → `UnsupportedPack(v)`。`portable::secrets_blob(&pack)`:空串 → 空 `Vec`(**不是错误**)。`portable::open_secrets(blob, pw)`:口令错 → `WrongPassword`;**空 blob 会被判成 CorruptPack**,所以空 blob 不能调它(设计 C1)。`portable::reseal_for_target(plain, key_source)`:用目标密钥加密并按 `Keyring` 方案编码,与 `Vault::save` 写出的字节同构。
- `portable::entry_target(dir, path)`:白名单(三个顶层文件 + `layouts/<数字和->.toml`),`None` = 不认。
- `Vault::open(dir, key_source)`:没有 `secrets.enc` → 空密文表;schema 过新 → `UnsupportedSchema(v)`;schema 偏旧 → 迁移并**立即 `save()`**,会在该目录写出 `secrets.enc` 和 `sessions.toml.bak`。所以它只能在 staging 里开,提交时只搬白名单三文件。
- `vault::write_atomic(path, bytes)` 是 `pub(crate)`。
- 设计 §3 步骤 5:三次 rename 之间崩溃不另做事务 —— 「新 sessions + 旧 secrets」同一把数据钥,`Vault::open` 的孤儿裁剪会容忍。

- [ ] **Step 1: 写失败的测试**

在 `phone_import.rs` 顶部 `use` 区补上(实现要用的也一起加,未用的 clippy 会在 Step 3 前报 unused,可以先加 `#[allow]` 或等实现完再跑 clippy):

```rust
use std::path::Path;

use base64::Engine as _;

use crate::error::StoreError;
use crate::master_key::MasterKeySource;
use crate::model::SecretEntry;
use crate::vault::{write_atomic, Vault};
```

在 `Reachable` 之后加类型与桩:

```rust
/// 暂存目录名。以 `.` 开头,与任何白名单文件都撞不上。
pub const STAGING_DIR: &str = ".import-staging";

/// 手机上落盘的明文文件。`settings.toml`、`layouts/` 按设计不写(D2)。
const PHONE_FILES: &[&str] = &["sessions.toml", "known_hosts.toml"];
const SECRETS_FILE: &str = "secrets.enc";

/// 一次导入的结果,给手机界面报给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneImport {
    /// 导入后的项目名(含归档),按 `sessions.toml` 里的顺序。
    pub projects: Vec<String>,
    /// 保留 / 丢弃的密文条目数。
    pub kept: u32,
    pub dropped: u32,
    /// 见 [`Reachable::broken_nodes`]。
    pub broken_nodes: u32,
    /// **只计**白名单外 / base64 坏掉的条目。`settings.toml`、`layouts/*` 是
    /// 「按设计不写」,不计 —— 否则每份桌面包都报「跳过 N 条」,用户会以为丢了会话。
    pub unrecognized: u32,
}

/// 整份替换 `dir`:元数据整份、密文只留项目可达条目,用 `key_source` 重封。
///
/// 失败(口令错 / 包损坏 / 版本过新)时 `dir` 原样不动:一切先落在
/// `dir/.import-staging/`,全部成功后才逐个 rename 进 `dir`。
pub fn install_projects_only(
    dir: &Path,
    pack_text: &str,
    password: &str,
    key_source: &dyn MasterKeySource,
) -> Result<PhoneImport, StoreError> {
    let _ = (dir, pack_text, password, key_source);
    Err(StoreError::Io("未实现".into()))
}
```

在 `mod tests` 末尾追加(复用 Task 1 的 `session` / `inline` / `project`):

```rust
    use crate::master_key::InMemoryKey;
    use crate::model::{SecretEntry, SessionsFile, CURRENT_SCHEMA};
    use crate::portable::PackFile;

    const PW: &str = "pack-pw";
    const KEY: InMemoryKey = InMemoryKey([7u8; 32]);

    /// 与 `portable` 测试同一手法:低代价 Argon2 参数,免得每条测试跑几百毫秒。
    /// 参数写在文件头里,`open_secrets` 按头里的解,与生产参数无关。
    fn seal_fast(plain: &[u8], password: &str) -> Vec<u8> {
        let params = crate::kdf::KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        };
        let salt = [9u8; crate::kdf::SALT_LEN];
        let key = crate::kdf::derive_key(password, &salt, params).unwrap();
        let payload = crate::crypto::encrypt(&key, plain).unwrap();
        crate::secrets_file::encode(&crate::secrets_file::Scheme::Argon2id { params, salt }, &payload)
    }

    fn secrets_toml(keys: &[&str]) -> String {
        let m: BTreeMap<String, SecretEntry> = keys
            .iter()
            .map(|k| {
                let e = SecretEntry {
                    password: Some(format!("fake-{k}")),
                    ..Default::default()
                };
                (k.to_string(), e)
            })
            .collect();
        toml::to_string_pretty(&m).unwrap()
    }

    fn file(sessions: Vec<SessionRecord>, projects: Vec<ProjectRecord>) -> SessionsFile {
        SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: sessions,
            tunnel: Vec::new(),
            credential: Vec::new(),
            project: projects,
        }
    }

    fn pack_with(files: Vec<(String, Vec<u8>)>, secrets_blob: &[u8]) -> String {
        let files = files
            .into_iter()
            .map(|(path, bytes)| PackFile {
                path,
                body: crate::portable::b64().encode(bytes),
            })
            .collect();
        crate::portable::write_pack(files, secrets_blob, "test", "2026-11-12T00:00:00Z").unwrap()
    }

    /// 一份「桌面导出」的包:三个顶层文件 + 一条布局记录 + 密文。
    fn desktop_pack(f: &SessionsFile, secret_keys: &[&str], known_hosts: bool) -> String {
        let mut files = vec![
            ("sessions.toml".to_string(), toml::to_string_pretty(f).unwrap().into_bytes()),
            (crate::settings::SETTINGS_FILE.to_string(), b"# settings\n".to_vec()),
            (
                format!("{}/1700000000000-42.toml", crate::history::HISTORY_DIR),
                b"# layout\n".to_vec(),
            ),
        ];
        if known_hosts {
            files.push(("known_hosts.toml".to_string(), b"# kh\n".to_vec()));
        }
        pack_with(files, &seal_fast(secrets_toml(secret_keys).as_bytes(), PW))
    }

    /// 项目 1 = 节点 1;会话 4 无关。密文 1、4 → 留 1 丢 4。
    fn demo() -> SessionsFile {
        file(
            vec![session(1, Some(&[]), inline()), session(4, Some(&[]), inline())],
            vec![project(1, "演示", &[1])],
        )
    }

    fn names(dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn three_files() -> BTreeSet<String> {
        ["known_hosts.toml", "secrets.enc", "sessions.toml"]
            .map(String::from)
            .into()
    }

    fn phone_secret_keys(dir: &Path) -> BTreeSet<String> {
        let v = Vault::open(dir.to_path_buf(), &KEY).unwrap();
        let m: BTreeMap<String, SecretEntry> = toml::from_str(&v.secrets_plaintext().unwrap()).unwrap();
        m.into_keys().collect()
    }

    #[test]
    fn imports_only_reachable_secrets_and_writes_only_the_three_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let r = install_projects_only(&dir, &desktop_pack(&demo(), &["1", "4"], true), PW, &KEY)
            .unwrap();
        assert_eq!(
            r,
            PhoneImport {
                projects: vec!["演示".into()],
                kept: 1,
                dropped: 1,
                broken_nodes: 0,
                unrecognized: 0,
            },
            "settings.toml 与 layouts/ 按设计不写,不计入 unrecognized"
        );
        assert_eq!(names(&dir), three_files(), "无 settings.toml / layouts/ / staging");
        assert_eq!(phone_secret_keys(&dir), ["1".to_string()].into());
        let v = Vault::open(dir.clone(), &KEY).unwrap();
        assert_eq!(v.list().len(), 2, "元数据整份导入(D2):无关会话的元数据也在");
    }

    #[test]
    fn a_pack_without_secrets_imports_with_an_empty_password() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = pack_with(
            vec![("sessions.toml".into(), toml::to_string_pretty(&demo()).unwrap().into_bytes())],
            &[],
        );
        let r = install_projects_only(&dir, &text, "", &KEY).unwrap();
        assert_eq!((r.kept, r.dropped), (0, 0));
        assert!(phone_secret_keys(&dir).is_empty());
    }

    #[test]
    fn a_wrong_password_leaves_the_existing_import_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = desktop_pack(&demo(), &["1", "4"], true);
        install_projects_only(&dir, &text, PW, &KEY).unwrap();
        let before = std::fs::read(dir.join("sessions.toml")).unwrap();

        let err = install_projects_only(&dir, &text, "wrong", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::WrongPassword), "{err:?}");
        assert_eq!(std::fs::read(dir.join("sessions.toml")).unwrap(), before);
        assert_eq!(names(&dir), three_files(), "失败后 staging 也得清掉");
    }

    #[test]
    fn a_newer_pack_format_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = install_projects_only(tmp.path(), "format_version = 2\n", PW, &KEY).unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedPack(2)), "{err:?}");
    }

    #[test]
    fn a_newer_schema_is_rejected_and_the_existing_import_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        let before = std::fs::read(dir.join("sessions.toml")).unwrap();

        let text = pack_with(vec![("sessions.toml".into(), b"schema_version = 99\n".to_vec())], &[]);
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedSchema(99)), "{err:?}");
        assert_eq!(std::fs::read(dir.join("sessions.toml")).unwrap(), before);
        assert_eq!(names(&dir), three_files());
    }

    #[test]
    fn an_older_schema_pack_is_migrated_without_leaking_the_migration_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let mut f = demo();
        f.schema_version = CURRENT_SCHEMA - 1;
        let r = install_projects_only(&dir, &desktop_pack(&f, &["1", "4"], true), PW, &KEY).unwrap();
        assert_eq!((r.kept, r.dropped), (1, 1));
        assert_eq!(names(&dir), three_files(), "迁移写出的 sessions.toml.bak 不能跟进 dir");
        let text = std::fs::read_to_string(dir.join("sessions.toml")).unwrap();
        assert!(
            text.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "手机上落的是迁移后的文件"
        );
    }

    #[test]
    fn only_entries_outside_the_whitelist_are_counted_as_unrecognized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let mut pack = crate::portable::read_pack(&desktop_pack(&demo(), &["1"], true)).unwrap();
        pack.file.push(PackFile {
            path: "../evil".into(),
            body: crate::portable::b64().encode(b"x"),
        });
        let text = toml::to_string_pretty(&pack).unwrap();
        let r = install_projects_only(&dir, &text, PW, &KEY).unwrap();
        assert_eq!(r.unrecognized, 1);
        assert!(!tmp.path().join("evil").exists());
    }

    #[test]
    fn a_reimport_replaces_the_whole_set_including_files_the_new_pack_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        assert!(dir.join("known_hosts.toml").exists());

        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], false), PW, &KEY).unwrap();
        assert!(
            !dir.join("known_hosts.toml").exists(),
            "第二份包没带 known_hosts.toml,手机上那份必须删(整份替换,D4)"
        );
    }

    #[test]
    fn a_stale_staging_dir_from_a_crash_is_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let staging = dir.join(STAGING_DIR);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("sessions.toml"), "这不是 TOML [[[").unwrap();
        std::fs::write(staging.join("junk"), "x").unwrap();

        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        assert_eq!(names(&dir), three_files());
    }

    #[test]
    fn new_sessions_with_old_secrets_still_open_after_a_crash_between_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1", "4"], true), PW, &KEY).unwrap();
        // 模拟第 5 步只 rename 了 sessions.toml 就崩溃:新会话表 + 旧密文。
        let newer = file(
            vec![session(1, Some(&[]), inline()), session(9, Some(&[]), inline())],
            vec![project(1, "演示", &[9])],
        );
        std::fs::write(dir.join("sessions.toml"), toml::to_string_pretty(&newer).unwrap()).unwrap();

        let v = Vault::open(dir.clone(), &KEY).expect("同一把数据钥,孤儿裁剪应容忍");
        assert_eq!(v.list().len(), 2);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store phone_import 2>&1 | grep -E "test result|FAILED|panicked" | head -20`
Expected:新增的 10 条 FAIL(`未实现`),Task 1 的 7 条仍 ok。

- [ ] **Step 3: 实现**

`portable.rs:371`:`fn b64()` → `pub(crate) fn b64()`。

`master_key.rs:60` 的文档改为:

```rust
/// 固定密钥,不碰 keyring。**测试与 Android 生产路径共用**(F346-a):Android 的
/// 数据密钥由 Keystore 信封解出后经 ffi 传进来。不要把它当测试辅助 `#[cfg(test)]` 掉。
pub struct InMemoryKey(pub [u8; 32]);
```

`phone_import.rs`:替换 `install_projects_only` 桩,并加两个私有函数:

```rust
pub fn install_projects_only(
    dir: &Path,
    pack_text: &str,
    password: &str,
    key_source: &dyn MasterKeySource,
) -> Result<PhoneImport, StoreError> {
    let staging = dir.join(STAGING_DIR);
    let result = stage(&staging, pack_text, password, key_source).and_then(|summary| {
        commit(dir, &staging)?;
        Ok(summary)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// 第 1~4 步:一切只落在 staging。
fn stage(
    staging: &Path,
    pack_text: &str,
    password: &str,
    key_source: &dyn MasterKeySource,
) -> Result<PhoneImport, StoreError> {
    // 上次崩溃的残留整个清掉,里面的东西一概不信。
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging)?;

    let pack = crate::portable::read_pack(pack_text)?;
    let mut unrecognized = 0u32;
    for entry in &pack.file {
        if crate::portable::entry_target(staging, &entry.path).is_none() {
            unrecognized += 1;
            continue;
        }
        if !PHONE_FILES.contains(&entry.path.as_str()) {
            continue; // settings.toml / layouts/*:按设计不写,也不计
        }
        match crate::portable::b64().decode(&entry.body) {
            Ok(bytes) => write_atomic(&staging.join(&entry.path), &bytes)?,
            Err(_) => unrecognized += 1,
        }
    }

    // 只读元数据:staging 里还没有 secrets.enc,Vault 拿到的是空密文表。
    // schema 偏旧会在这里迁移并写回 staging(连同 .bak),提交时只搬白名单。
    let (reach, projects) = {
        let vault = Vault::open(staging.to_path_buf(), key_source)?;
        let reach = project_reachable_secret_keys(vault.list(), vault.groups(), vault.projects());
        let projects = vault.projects().iter().map(|p| p.name.clone()).collect();
        (reach, projects)
    };

    // 空 blob 不能交给 open_secrets:它会判成「没有口令头」(设计 C1)。
    let blob = crate::portable::secrets_blob(&pack)?;
    let mut secrets: BTreeMap<String, SecretEntry> = if blob.is_empty() {
        BTreeMap::new()
    } else {
        let plain = crate::portable::open_secrets(&blob, password)?;
        toml::from_str(&String::from_utf8(plain)?)?
    };
    let total = secrets.len();
    secrets.retain(|k, _| reach.keys.contains(k));
    let kept = secrets.len();

    let plain = toml::to_string_pretty(&secrets)?;
    let sealed = crate::portable::reseal_for_target(plain.as_bytes(), key_source)?;
    write_atomic(&staging.join(SECRETS_FILE), &sealed)?;

    Ok(PhoneImport {
        projects,
        kept: kept as u32,
        dropped: (total - kept) as u32,
        broken_nodes: reach.broken_nodes,
        unrecognized,
    })
}

/// 第 5 步:逐个 rename 进 `dir`;包里没带的在 `dir` 里删掉(整份替换)。
///
/// 顺序 sessions 在前、secrets 在后。中途崩溃留下「新 sessions + 旧 secrets」,
/// 二者同一把数据钥,`Vault::open` 的孤儿裁剪会容忍;重导即恢复(设计 §3)。
fn commit(dir: &Path, staging: &Path) -> Result<(), StoreError> {
    for name in PHONE_FILES.iter().chain(std::iter::once(&SECRETS_FILE)) {
        let src = staging.join(name);
        if src.exists() {
            std::fs::rename(&src, dir.join(name))?;
        } else {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Ok(())
}
```

`lib.rs` 的 `pub use phone_import::...` 改为:

```rust
pub use phone_import::{
    install_projects_only, project_reachable_secret_keys, PhoneImport, Reachable, STAGING_DIR,
};
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store phone_import 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok. 17 passed`

若 `an_older_schema_pack_is_migrated...` 失败:先看报错是不是迁移本身拒收了「当前结构 + 旧版本号」的文件;**不要削弱断言**,改用 `vault.rs` 测试里现成的某个真实旧版本文件样例当 `sessions.toml`(搜 `真实 v11 文件`),并相应调整 `kept/dropped` 的期望。

- [ ] **Step 5: store 全量 + clippy + fmt**

Run: `cargo test -p mullion-store > /tmp/store.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/store.log; cargo clippy -p mullion-store --all-targets -- -D warnings; cargo fmt --check`
Expected:全 `ok`,clippy/fmt 无输出。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/phone_import.rs crates/mullion-store/src/portable.rs crates/mullion-store/src/master_key.rs crates/mullion-store/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(store): 手机导入只留项目可达密文,暂存后整份替换 (F346)

内存裁剪,未裁全量密文不落盘;空密文包不调 open_secrets;schema 过新原样上抛;
失败时 dir 不动,崩溃残留的 staging 下次清掉。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: ffi 导出 `inspect_pack` / `import_pack` + 「不回写」守护

**Files:**
- Modify: `crates/mullion-ffi/Cargo.toml`
- Create: `crates/mullion-ffi/src/error.rs`
- Modify: `crates/mullion-ffi/src/terminal.rs`(删除 `FfiError` 的定义 / `Display` / `Error` 三段,`From<TrySendErr>` 留在原处)
- Create: `crates/mullion-ffi/src/import.rs`
- Modify: `crates/mullion-ffi/src/lib.rs`
- Create: `crates/mullion-ffi/tests/common/demo_pack.rs`
- Create: `crates/mullion-ffi/tests/import_pack.rs`
- Create: `crates/mullion-ffi/tests/no_write_back.rs`
- Modify: `crates/mullion-ffi/tests/dependency_direction.rs`

背景:`FfiError` 是 `#[derive(Debug, uniffi::Error)] #[uniffi(flat_error)]`,Kotlin 侧收到 `FfiException.<变体名>`,`message` = Rust 的 `Display`。**不要动 `tests/common/mod.rs`**(那是 echo sshd,`terminal_roundtrip` 在用)。

- [ ] **Step 1: Cargo.toml**

`[dependencies]` 末尾加:

```toml
# F346-a:导入判据在 store。**不得依赖 mullion-cloud** —— 那是「不回写」的
# 结构性判据(设计 D5,守护在 tests/dependency_direction.rs)。
mullion-store = { path = "../mullion-store" }
```

`[dev-dependencies]` 末尾加:

```toml
toml.workspace = true
base64.workspace = true
tempfile = "3"
```

- [ ] **Step 2: 写失败的测试**

`crates/mullion-ffi/tests/common/demo_pack.rs`:

```rust
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
    AppearancePrefs, Auth, AuthKind, Connection, CredentialId, CredentialRecord, Identity,
    JumpRef, NetworkPrefs, PackFile, ProjectRecord, Protocol, SecretEntry, SessionId,
    SessionRecord, TerminalPrefs,
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
            ("sessions.toml", toml::to_string_pretty(&sessions_file()).unwrap().into_bytes()),
            ("known_hosts.toml", Vec::new()),
        ],
        &blob,
    )
}
```

(`CredentialRecord` 的 `kind` 是 `#[serde(flatten)] AuthKind`;若 `AuthKind::Password` 不是单元变体,按 `crates/mullion-store/src/model.rs:20` 的实际定义构造。)

`crates/mullion-ffi/tests/import_pack.rs`:

```rust
//! F346-a:ffi 层只做类型转换与错误映射;判据的测试在 `mullion-store::phone_import`。
#[path = "common/demo_pack.rs"]
mod demo_pack;

use demo_pack::{demo_pack, pack_text, DEMO_PASSWORD, DEMO_PROJECT};
use mullion_ffi::{import_pack, inspect_pack, FfiError};

fn dir_in(tmp: &tempfile::TempDir) -> String {
    tmp.path().join("vault").to_string_lossy().into_owned()
}

#[test]
fn the_demo_pack_keeps_only_project_reachable_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let s = import_pack(dir_in(&tmp), demo_pack(), DEMO_PASSWORD.into(), vec![7; 32]).unwrap();
    assert_eq!(s.projects, vec![DEMO_PROJECT.to_string()]);
    assert_eq!((s.kept, s.dropped, s.broken_nodes, s.unrecognized), (3, 1, 0, 0));
}

#[test]
fn inspect_tells_whether_a_password_is_needed() {
    assert!(inspect_pack(demo_pack()).unwrap().has_secrets);
    assert!(!inspect_pack(pack_text(Vec::new(), &[])).unwrap().has_secrets);
}

#[test]
fn a_wrong_password_maps_to_wrong_password() {
    let tmp = tempfile::tempdir().unwrap();
    let err = import_pack(dir_in(&tmp), demo_pack(), "nope".into(), vec![7; 32]).unwrap_err();
    assert!(matches!(err, FfiError::WrongPassword), "{err:?}");
}

#[test]
fn a_newer_pack_format_maps_to_needs_newer_app() {
    let err = inspect_pack("format_version = 9\n".into()).unwrap_err();
    assert!(matches!(err, FfiError::NeedsNewerApp(9)), "{err:?}");
}

#[test]
fn a_newer_schema_maps_to_needs_newer_app() {
    let tmp = tempfile::tempdir().unwrap();
    let text = pack_text(vec![("sessions.toml", b"schema_version = 99\n".to_vec())], &[]);
    let err = import_pack(dir_in(&tmp), text, String::new(), vec![7; 32]).unwrap_err();
    assert!(matches!(err, FfiError::NeedsNewerApp(99)), "{err:?}");
}

#[test]
fn garbage_maps_to_bad_pack() {
    let err = inspect_pack("这不是 TOML [[[".into()).unwrap_err();
    assert!(matches!(err, FfiError::BadPack(_)), "{err:?}");
}

#[test]
fn a_data_key_of_the_wrong_length_is_rejected_before_touching_the_dir() {
    for len in [31, 33] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let err = import_pack(
            dir.to_string_lossy().into_owned(),
            demo_pack(),
            DEMO_PASSWORD.into(),
            vec![7; len],
        )
        .unwrap_err();
        assert!(matches!(err, FfiError::Internal(_)), "{len}: {err:?}");
        assert!(!dir.exists(), "{len} 字节密钥:不得创建 dir");
    }
}
```

`crates/mullion-ffi/tests/no_write_back.rs`:

```rust
//! F346-a 设计 D5:手机端只读,不回写。主判据是结构性的(`dependency_direction.rs`
//! 里「ffi 不依赖 mullion-cloud」);这里是辅助的源码守护。
//!
//! 注释只剥**整行**的:剥行尾注释会连同一行的代码一起剥掉(剥多 → 假绿)。
//! 测试模块按本 crate 约定在文件末尾、形如 `#[cfg(test)]\nmod tests`;不是这个
//! 形状就直接失败,而不是把 `#[cfg(test)]` 之后的生产代码一并切掉(切多 → 假绿)。

const FORBIDDEN: &[&str] = &["write_pack", "seal_secrets", "put_no_overwrite", ".save("];

fn production_source() -> String {
    let mut out = String::new();
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let body = match text.find("#[cfg(test)]") {
                Some(i) => {
                    assert!(
                        text[i..].starts_with("#[cfg(test)]\nmod tests"),
                        "{}:`#[cfg(test)]` 后面不是 `mod tests`,守护无法安全切片",
                        path.display()
                    );
                    &text[..i]
                }
                None => &text[..],
            };
            for line in body.lines().filter(|l| !l.trim_start().starts_with("//")) {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

#[test]
fn ffi_production_code_never_calls_a_write_back_api() {
    let src = production_source();
    // 防恒绿:确认真扫到了导入入口(而不是扫了个空目录)。
    assert!(src.contains("install_projects_only"), "解析失效:没扫到导入入口");
    for f in FORBIDDEN {
        assert!(!src.contains(f), "ffi 生产代码出现了回写调用 `{f}`");
    }
}
```

(若工具链的 `Option::is_none_or` 不可用,改成 `!path.extension().is_some_and(|e| e == "rs")`。)

`tests/dependency_direction.rs` 末尾追加:

```rust
/// F346-a 设计 D5:「不回写」的**结构性**判据 —— 不依赖 cloud 就推不了云。
/// F346-b 引入云拉包时要换判据(只允许 GET),届时改这里。
#[test]
fn ffi_never_depends_on_cloud() {
    let t = tree("mullion-ffi");
    assert!(
        has(&t, "mullion-store"),
        "解析失效:ffi 树里找不到 mullion-store\n{t}"
    );
    assert!(
        !has(&t, "mullion-cloud"),
        "mullion-ffi 依赖了 mullion-cloud\n{t}"
    );
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-ffi 2>&1 | tail -20`
Expected:编译失败,`import_pack` / `inspect_pack` / `FfiError::WrongPassword` 等未定义。

- [ ] **Step 4: 实现**

`crates/mullion-ffi/src/error.rs`(把 `terminal.rs` 里 `FfiError` 的定义、`Display`、`Error` 三段**原文挪过来**再加变体;`terminal.rs` 里删除这三段,在 `use` 区加 `use crate::error::FfiError;`,`impl From<TrySendErr> for FfiError` 留在 terminal.rs 原处):

```rust
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
            Self::Internal(e) => write!(f, "内部错误:{e}"),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<StoreError> for FfiError {
    fn from(e: StoreError) -> Self {
        match &e {
            StoreError::WrongPassword => Self::WrongPassword,
            StoreError::UnsupportedPack(v) | StoreError::UnsupportedSchema(v) => {
                Self::NeedsNewerApp(*v)
            }
            StoreError::CorruptPack(_)
            | StoreError::CorruptSecrets(_)
            | StoreError::TomlDe(_)
            | StoreError::Migration(_) => Self::BadPack(e.to_string()),
            _ => Self::Storage(e.to_string()),
        }
    }
}
```

(先核对 terminal.rs 里原有三个变体的文案,挪过来时逐字保持。)

`crates/mullion-ffi/src/import.rs`:

```rust
//! F346-a:手机导入 `.mullionpack`。判据全在 `mullion_store::phone_import`,
//! 这里只做类型转换。**本模块(以及整个 ffi)不得出现任何写出包 / 推云的调用**,
//! 守护见 `tests/no_write_back.rs` 与 `tests/dependency_direction.rs`。

use std::path::Path;

use mullion_store::{install_projects_only, read_pack, InMemoryKey};

use crate::error::FfiError;

/// 选完文件先看一眼:要不要弹口令框。
#[derive(Debug, uniffi::Record)]
pub struct PackInfo {
    pub has_secrets: bool,
}

/// 见 `mullion_store::PhoneImport`。
#[derive(Debug, uniffi::Record)]
pub struct ImportSummary {
    pub projects: Vec<String>,
    pub kept: u32,
    pub dropped: u32,
    pub broken_nodes: u32,
    pub unrecognized: u32,
}

/// 只读:解析包头,不碰任何文件。
#[uniffi::export]
pub fn inspect_pack(pack_text: String) -> Result<PackInfo, FfiError> {
    let pack = read_pack(&pack_text)?;
    Ok(PackInfo {
        has_secrets: !pack.secrets.is_empty(),
    })
}

/// 整份替换 `dir`。`data_key` = Keystore 信封解出的 32 字节数据密钥;
/// 长度不对直接报错,**不碰 `dir`**。耗时(Argon2 + IO),Kotlin 侧放 IO 线程。
#[uniffi::export]
pub fn import_pack(
    dir: String,
    pack_text: String,
    password: String,
    data_key: Vec<u8>,
) -> Result<ImportSummary, FfiError> {
    let key: [u8; 32] = data_key.as_slice().try_into().map_err(|_| {
        FfiError::Internal(format!("数据密钥应为 32 字节,实为 {}", data_key.len()))
    })?;
    let r = install_projects_only(Path::new(&dir), &pack_text, &password, &InMemoryKey(key))?;
    Ok(ImportSummary {
        projects: r.projects,
        kept: r.kept,
        dropped: r.dropped,
        broken_nodes: r.broken_nodes,
        unrecognized: r.unrecognized,
    })
}
```

`crates/mullion-ffi/src/lib.rs`:模块区改为

```rust
mod error;
mod import;
mod runtime;
mod screen;
mod terminal;

pub use error::FfiError;
pub use import::{import_pack, inspect_pack, ImportSummary, PackInfo};
pub use screen::ScreenText;
pub use terminal::{ConnectParams, ScreenListener, Terminal};
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-ffi > /tmp/ffi.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/ffi.log`
Expected:全部 `ok`;`import_pack` 7 条、`no_write_back` 1 条、`dependency_direction` 3 条都在计数里。

- [ ] **Step 6: 守护自检(防恒绿)—— 临时植入,确认会红,再撤掉**

1. 在 `src/import.rs` 末尾临时加一行 `fn _probe() { let _ = mullion_store::write_pack; }`,跑 `cargo test -p mullion-ffi --test no_write_back`,Expected FAIL「出现了回写调用 `write_pack`」;撤掉。
2. 在 `Cargo.toml` `[dependencies]` 临时加 `mullion-cloud = { path = "../mullion-cloud" }`,跑 `cargo test -p mullion-ffi --test dependency_direction ffi_never_depends_on_cloud`,Expected FAIL;撤掉。
3. `git diff --stat` 确认两处临时改动已撤净。

- [ ] **Step 7: Android 交叉编译能过(store 进了 ffi 的依赖树)**

Run: `ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865 cargo ndk -t x86_64 --platform 30 build -p mullion-ffi 2>&1 | tail -3`
Expected:`Finished`。(只验能编;产物别放进 jniLibs,那是 build-rust.sh 的事。)

- [ ] **Step 8: clippy + fmt,提交**

Run: `cargo clippy -p mullion-ffi --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/mullion-ffi Cargo.lock
git commit -m "$(cat <<'EOF'
feat(ffi): 导出 inspect_pack/import_pack,结构性守护 ffi 不依赖 cloud (F346)

FfiError 挪到 error.rs 并加 WrongPassword/NeedsNewerApp/BadPack/Storage/Internal;
源码守护剥整行注释、按 mod tests 形状切测试模块,两处守护均植入验红。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Android 测试资产(演示包)

**Files:**
- Create: `crates/mullion-ffi/examples/make_test_pack.rs`
- Create: `android/app/src/androidTest/assets/test.mullionpack`(生成物,入库)

- [ ] **Step 1: 写 example**

```rust
//! 生成 Android 插桩测试用的 `.mullionpack`(F346-a,全是假数据)。
//!
//!     cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack
//!
//! 内容与预期计数见 `tests/common/demo_pack.rs` 文档。盐是随机的,每次生成字节不同 ——
//! 只在演示包内容变了时重新生成并提交。
#[path = "../tests/common/demo_pack.rs"]
mod demo_pack;

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("用法:make_test_pack <输出路径>");
    if let Some(parent) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&out, demo_pack::demo_pack()).unwrap();
    println!("wrote {out}");
}
```

- [ ] **Step 2: 生成并核对**

Run:
```bash
cargo run -q -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack
grep -cE "192\.168\.|PRIVATE KEY|ghp_|github_pat_" android/app/src/androidTest/assets/test.mullionpack
grep -c "format_version = 1" android/app/src/androidTest/assets/test.mullionpack
```
Expected:`wrote ...`;第一个计数 `0`;第二个 `1`。

- [ ] **Step 3: clippy(example 也在 all-targets 里)+ 提交**

Run: `cargo clippy -p mullion-ffi --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/mullion-ffi/examples/make_test_pack.rs android/app/src/androidTest/assets/test.mullionpack
git commit -m "$(cat <<'EOF'
test(android): 演示包生成器与插桩测试资产,全是假数据 (F346)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Kotlin Keystore 信封 `VaultKey` + 关备份

**Files:**
- Create: `android/app/src/main/java/io/github/kilobitcy/mullion/VaultKey.kt`
- Create: `android/app/src/main/res/xml/data_extraction_rules.xml`
- Modify: `android/app/src/main/AndroidManifest.xml`
- Create: `android/app/src/androidTest/java/io/github/kilobitcy/mullion/VaultKeyTest.kt`

- [ ] **Step 1: 写插桩测试**

`VaultKeyTest.kt`:

```kotlin
package io.github.kilobitcy.mullion

import android.content.pm.ApplicationInfo
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.security.KeyStore

/** F346-a 设计 §5:Keystore 信封。 */
@RunWith(AndroidJUnit4::class)
class VaultKeyTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val keyFile get() = File(ctx.filesDir, VaultKey.KEY_FILE)

    private fun dropAlias() =
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(VaultKey.ALIAS)

    @Before
    fun clean() {
        keyFile.delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        dropAlias()
    }

    @Test
    fun theSameDataKeyComesBackAcrossLoads() {
        val a = VaultKey.load(ctx)
        val b = VaultKey.load(ctx)
        assertFalse(a.reset)
        assertFalse(b.reset)
        assertEquals(32, a.dataKey.size)
        assertArrayEquals(a.dataKey, b.dataKey)
    }

    @Test
    fun theKeyFileIsVersionedAndDoesNotContainThePlainKey() {
        val a = VaultKey.load(ctx)
        val raw = keyFile.readBytes()
        assertEquals(1, raw[0].toInt())
        assertEquals(1 + 12 + 32 + 16, raw.size)
        assertFalse(raw.toList().windowed(32).any { it.toByteArray().contentEquals(a.dataKey) })
    }

    @Test
    fun aLostAliasRegeneratesTheKeyAndClearsTheVault() {
        val a = VaultKey.load(ctx)
        VaultKey.vaultDir(ctx).mkdirs()
        File(VaultKey.vaultDir(ctx), "sessions.toml").writeText("x")
        dropAlias()

        val b = VaultKey.load(ctx)
        assertTrue(b.reset)
        assertFalse(a.dataKey.contentEquals(b.dataKey))
        assertFalse("旧密文用旧钥封的,再也解不开,必须清掉", VaultKey.vaultDir(ctx).exists())
        assertArrayEquals("重新生成后应稳定", b.dataKey, VaultKey.load(ctx).dataKey)
    }

    @Test
    fun anUnknownVersionByteIsTreatedAsLost() {
        VaultKey.load(ctx)
        val raw = keyFile.readBytes().also { it[0] = 2 }
        keyFile.writeBytes(raw)
        assertTrue(VaultKey.load(ctx).reset)
    }

    @Test
    fun backupIsDisabled() {
        // 系统备份会带走 vault-key.bin 而带不走 Keystore 钥。
        assertEquals(0, ctx.applicationInfo.flags and ApplicationInfo.FLAG_ALLOW_BACKUP)
    }
}
```

- [ ] **Step 2: 实现 `VaultKey.kt`**

```kotlin
package io.github.kilobitcy.mullion

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * F346-a 设计 D3:信封。AndroidKeyStore 里一把不可导出的 AES-256-GCM 包装钥,
 * 包住一把随机 32 字节数据密钥;Rust 侧拿数据密钥当 `InMemoryKey` 封 `secrets.enc`。
 *
 * `vault-key.bin` = 版本(1 字节) ‖ iv(12) ‖ 密文。版本字节给 F347 留路:Keystore 密钥
 * 生成后参数不可改,加认证要求只能换新 alias、解旧包、重包。
 *
 * 已知代价(F347 前):数据密钥以 ByteArray 过 JNA,Kotlin 堆与 JNA 缓冲各留一份,不零化。
 */
object VaultKey {
    const val ALIAS = "mullion-vault-wrap-v1"
    const val KEY_FILE = "vault-key.bin"
    const val VAULT_DIR = "vault"
    private const val VERSION: Byte = 1
    private const val IV_LEN = 12
    private const val TAG_BITS = 128

    /** [reset] = 本机原有的密钥解不开,旧数据已清空,需要重新导入。 */
    class Loaded(val dataKey: ByteArray, val reset: Boolean)

    fun vaultDir(ctx: Context) = File(ctx.filesDir, VAULT_DIR)

    @Synchronized
    fun load(ctx: Context): Loaded {
        val file = File(ctx.filesDir, KEY_FILE)
        if (file.exists()) {
            runCatching { unwrap(file.readBytes()) }.getOrNull()?.let { return Loaded(it, reset = false) }
        }
        // 无钥,或解不开(alias 丢了 / 系统还原 / 清除凭据存储):旧密文再也解不开,
        // 连同 vault/ 一起清掉,重导即恢复(设计 D4)。
        val reset = file.exists()
        file.delete()
        vaultDir(ctx).deleteRecursively()
        if (reset) keyStore().deleteEntry(ALIAS)

        val key = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val tmp = File(ctx.filesDir, "$KEY_FILE.tmp")
        tmp.writeBytes(wrap(key))
        check(tmp.renameTo(file)) { "写 $KEY_FILE 失败" }
        return Loaded(key, reset)
    }

    private fun keyStore() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private fun wrappingKey(): SecretKey {
        (keyStore().getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    private fun wrap(dataKey: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, wrappingKey()) // IV 由 Keystore 随机生成
        val iv = c.iv
        check(iv.size == IV_LEN) { "意外的 IV 长度 ${iv.size}" }
        return byteArrayOf(VERSION) + iv + c.doFinal(dataKey)
    }

    private fun unwrap(blob: ByteArray): ByteArray {
        require(blob.size > 1 + IV_LEN && blob[0] == VERSION) { "$KEY_FILE 版本或长度不对" }
        val key = keyStore().getKey(ALIAS, null) as? SecretKey ?: error("包装钥不在了")
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, blob, 1, IV_LEN))
        val out = c.doFinal(blob, 1 + IV_LEN, blob.size - 1 - IV_LEN)
        require(out.size == 32) { "数据密钥长度不对" }
        return out
    }
}
```

- [ ] **Step 3: 关备份**

`android/app/src/main/res/xml/data_extraction_rules.xml`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<!-- F346-a:vault-key.bin 只在本机 Keystore 下解得开;随备份/换机迁出去只会变成
     「本机密钥已失效」。Android 12+ 上 allowBackup=false 不管设备间迁移,所以两处都显式排除。 -->
<data-extraction-rules>
    <cloud-backup>
        <exclude domain="root" path="." />
        <exclude domain="file" path="." />
        <exclude domain="database" path="." />
        <exclude domain="sharedpref" path="." />
        <exclude domain="external" path="." />
    </cloud-backup>
    <device-transfer>
        <exclude domain="root" path="." />
        <exclude domain="file" path="." />
        <exclude domain="database" path="." />
        <exclude domain="sharedpref" path="." />
        <exclude domain="external" path="." />
    </device-transfer>
</data-extraction-rules>
```

`AndroidManifest.xml` 的 `<application` 标签加两个属性:

```xml
    <application
        android:allowBackup="false"
        android:dataExtractionRules="@xml/data_extraction_rules"
        android:label="Mullion"
        android:theme="@android:style/Theme.Material.NoActionBar">
```

(写前用 context7 查一下 `data-extraction-rules` 的 domain 取值是否与上面一致;不一致以官方文档为准。)

- [ ] **Step 4: 跑插桩测试**

Run:
```bash
android/scripts/emulator.sh start
cd android && ./gradlew --no-daemon installDebug installDebugAndroidTest > /tmp/gradle.log 2>&1; tail -3 /tmp/gradle.log
adb shell am instrument -w -r -e class io.github.kilobitcy.mullion.VaultKeyTest io.github.kilobitcy.mullion.test/androidx.test.runner.AndroidJUnitRunner | tail -5
```
Expected:`BUILD SUCCESSFUL`;`OK (5 tests)`。

- [ ] **Step 5: 提交**

```bash
git add android/app/src/main/java/io/github/kilobitcy/mullion/VaultKey.kt android/app/src/main/res/xml/data_extraction_rules.xml android/app/src/main/AndroidManifest.xml android/app/src/androidTest/java/io/github/kilobitcy/mullion/VaultKeyTest.kt
git commit -m "$(cat <<'EOF'
feat(android): Keystore 信封包数据密钥,关闭备份与设备迁移 (F346)

vault-key.bin 带版本字节给 F347 留路;解不开即清 vault/ 重新生成。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Kotlin 导入流程与入口

**Files:**
- Create: `android/app/src/main/java/io/github/kilobitcy/mullion/PackImport.kt`
- Create: `android/app/src/main/java/io/github/kilobitcy/mullion/ImportViewModel.kt`
- Modify: `android/app/src/main/java/io/github/kilobitcy/mullion/MainActivity.kt`
- Create: `android/app/src/androidTest/java/io/github/kilobitcy/mullion/PackImportTest.kt`

背景:uniffi 生成的 Kotlin 在包 `io.github.kilobitcy.mullion.ffi`:顶层函数 `inspectPack(String)`、`importPack(String, String, String, ByteArray)`;记录 `PackInfo(hasSecrets)`、`ImportSummary(projects, kept, dropped, brokenNodes, unrecognized)`(数值是 `UInt`);异常 `FfiException.WrongPassword` 等。**先核对实际签名**(Step 1)。

- [ ] **Step 1: 核对生成的绑定(不碰 android/app/build)**

Run:
```bash
cargo build -q -p mullion-ffi
rm -rf /tmp/uniffi-check
cargo run -q -p mullion-ffi --features bindgen --bin uniffi-bindgen -- generate --library target/debug/libmullion_ffi.so --language kotlin --no-format --out-dir /tmp/uniffi-check
grep -rnE "fun `?(inspectPack|importPack)`?\(|class (PackInfo|ImportSummary)\(|class (WrongPassword|NeedsNewerApp|BadPack)\(" /tmp/uniffi-check | head
```
Expected:能看到上面列出的名字与参数类型。若与本 Task 代码不符,**以生成物为准**改下面的 Kotlin。

- [ ] **Step 2: 写插桩测试**

`PackImportTest.kt`:

```kotlin
package io.github.kilobitcy.mullion

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.ImportSummary
import io.github.kilobitcy.mullion.ffi.inspectPack
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * F346-a:端到端导入 Rust example 生成的演示包(`crates/mullion-ffi/tests/common/demo_pack.rs`)。
 * 预期:项目「演示项目」,保留 3 份凭据,丢弃 1 份。
 */
@RunWith(AndroidJUnit4::class)
class PackImportTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val pack = InstrumentationRegistry.getInstrumentation().context.assets
        .open("test.mullionpack").bufferedReader().use { it.readText() }

    @Before
    fun clean() {
        File(ctx.filesDir, VaultKey.KEY_FILE).delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
    }

    @Test
    fun inspectSeesThatThePackNeedsAPassword() {
        assertTrue(inspectPack(pack).hasSecrets)
    }

    @Test
    fun importKeepsOnlyProjectReachableSecrets() {
        val text = PackImport.run(ctx, pack, "demo-pack-password")
        assertEquals("已导入 1 个项目:演示项目;保留 3 份凭据,丢弃 1 份", text)
        assertEquals(
            setOf("sessions.toml", "known_hosts.toml", "secrets.enc"),
            VaultKey.vaultDir(ctx).list()!!.toSet(),
        )
    }

    @Test
    fun aWrongPasswordIsTyped() {
        assertThrows(FfiException.WrongPassword::class.java) {
            PackImport.run(ctx, pack, "nope")
        }
    }

    @Test
    fun theSummaryMentionsBrokenNodesAndSkippedEntries() {
        val t = PackImport.summaryText(ImportSummary(listOf("甲"), 1u, 0u, 2u, 3u))
        assertTrue(t, t.contains("2 个节点"))
        assertTrue(t, t.contains("跳过 3 条"))
    }
}
```

- [ ] **Step 3: 实现 `PackImport.kt`**

```kotlin
package io.github.kilobitcy.mullion

import android.content.Context
import io.github.kilobitcy.mullion.ffi.ImportSummary
import io.github.kilobitcy.mullion.ffi.importPack

/** F346-a:取数据密钥 → 导入 → 摘要文本。耗时(Argon2 + IO),调用方放 IO 线程。 */
object PackImport {
    fun run(ctx: Context, text: String, password: String): String {
        val key = VaultKey.load(ctx)
        val summary = importPack(VaultKey.vaultDir(ctx).path, text, password, key.dataKey)
        val head = if (key.reset) "本机密钥已失效,旧数据已清空。\n" else ""
        return head + summaryText(summary)
    }

    fun summaryText(s: ImportSummary): String = buildString {
        append("已导入 ${s.projects.size} 个项目")
        if (s.projects.isNotEmpty()) append(":" + s.projects.joinToString("、"))
        append(";保留 ${s.kept} 份凭据,丢弃 ${s.dropped} 份")
        if (s.brokenNodes > 0u) append("\n${s.brokenNodes} 个节点的跳板链解析失败,只导入了节点本身的凭据")
        if (s.unrecognized > 0u) append("\n跳过 ${s.unrecognized} 条无法识别的条目")
    }
}
```

- [ ] **Step 4: 实现 `ImportViewModel.kt`**

```kotlin
package io.github.kilobitcy.mullion

import android.app.Application
import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.inspectPack
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** F346-a:选文件 →(需要时)口令 → 摘要。项目列表界面是 F348。 */
class ImportViewModel(app: Application) : AndroidViewModel(app) {
    var message by mutableStateOf("")
        private set

    /** 非 null = 包里有密文,正等口令。口令错时保留,让用户再输。 */
    var pending by mutableStateOf<String?>(null)
        private set

    var busy by mutableStateOf(false)
        private set

    fun picked(uri: Uri) = launchBusy {
        val text = withContext(Dispatchers.IO) {
            getApplication<Application>().contentResolver.openInputStream(uri)!!
                .use { it.readBytes().toString(Charsets.UTF_8) }
        }
        if (withContext(Dispatchers.IO) { inspectPack(text) }.hasSecrets) {
            pending = text
            message = "请输入包口令"
        } else {
            finish(text, "")
        }
    }

    fun submit(password: String) {
        val text = pending ?: return
        launchBusy { finish(text, password) }
    }

    private suspend fun finish(text: String, password: String) {
        message = withContext(Dispatchers.IO) { PackImport.run(getApplication(), text, password) }
        pending = null
    }

    private fun launchBusy(block: suspend () -> Unit) {
        if (busy) return
        busy = true
        viewModelScope.launch {
            try {
                block()
            } catch (e: FfiException.WrongPassword) {
                message = e.message ?: "口令不对"
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                pending = null
                message = "导入失败:${e.message}"
            } finally {
                busy = false
            }
        }
    }
}
```

- [ ] **Step 5: 入口(`MainActivity.kt`)**

`MainActivity` 里加 `private val importVm: ImportViewModel by viewModels()`,`setContent` 里改成 `Screen(vm, importVm)`;`Screen` 签名改为 `private fun Screen(vm: TerminalViewModel, importVm: ImportViewModel)`,在 `if (vm.status != "已连接") { ... }` 块里「连接」按钮之后加一行 `ImportSection(importVm)`。文件末尾加:

```kotlin
@Composable
private fun ImportSection(vm: ImportViewModel) {
    var packPassword by remember { mutableStateOf("") }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let(vm::picked)
    }
    // .mullionpack 没有登记的 MIME,只能放开到 */*;选错了由 inspect_pack 报「包已损坏」。
    Button({ picker.launch(arrayOf("*/*")) }, Modifier.testTag("import"), enabled = !vm.busy) {
        Text("导入 .mullionpack")
    }
    if (vm.pending != null) {
        OutlinedTextField(
            packPassword, { packPassword = it }, Modifier.fillMaxWidth().testTag("pack-password"),
            label = { Text("包口令") }, visualTransformation = PasswordVisualTransformation(),
        )
        Button({ vm.submit(packPassword); packPassword = "" }, Modifier.testTag("pack-submit"), enabled = !vm.busy) {
            Text("导入")
        }
    }
    if (vm.message.isNotEmpty()) Text(vm.message, Modifier.testTag("import-status"))
}
```

import 区补:

```kotlin
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
```

- [ ] **Step 6: 跑全部插桩测试(e2e)**

Run:
```bash
android/scripts/e2e.sh > /tmp/e2e.log 2>&1; tail -5 /tmp/e2e.log
cargo build -q -p mullion-ffi --example echo_sshd
(target/debug/examples/echo_sshd 2222 > /tmp/echo.log 2>&1 &) ; sleep 1
adb shell am instrument -w -r io.github.kilobitcy.mullion.test/androidx.test.runner.AndroidJUnitRunner | tail -3
pkill -f "examples/echo_sshd 2222" || true
```
Expected:`BUILD SUCCESSFUL`;`OK (11 tests)`(F345 的 2 条 + VaultKey 5 条 + PackImport 4 条)。少于 11 条就是有测试类没被装进去,不算绿。

- [ ] **Step 7: 提交**

```bash
git add android/app/src/main/java/io/github/kilobitcy/mullion/PackImport.kt android/app/src/main/java/io/github/kilobitcy/mullion/ImportViewModel.kt android/app/src/main/java/io/github/kilobitcy/mullion/MainActivity.kt android/app/src/androidTest/java/io/github/kilobitcy/mullion/PackImportTest.kt
git commit -m "$(cat <<'EOF'
feat(android): 导入 .mullionpack 入口,口令错可重输,摘要报保留/丢弃数 (F346)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: 文档、全量绿、收尾

**Files:**
- Modify: `docs/android-build.md`
- Modify: `spec.md:578`

- [ ] **Step 1: `docs/android-build.md`**

「构建与测试」节末尾加:

```markdown
- 导入测试资产:`android/app/src/androidTest/assets/test.mullionpack` 由
  `cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack`
  生成(假数据,内容见 `crates/mullion-ffi/tests/common/demo_pack.rs`);演示包内容变了才重生成
```

「已知坑」节末尾加:

```markdown
- 本地数据:`filesDir/vault/`(与桌面同格式的 `sessions.toml` / `known_hosts.toml` / `secrets.enc`);
  数据密钥由 Keystore 包装钥 `mullion-vault-wrap-v1` 包在 `filesDir/vault-key.bin`(首字节版本号)。
  解不开即清空 `vault/` 重新生成 —— 重导即恢复(F346-a)
- 备份与设备迁移都关了(`allowBackup=false` + `data_extraction_rules.xml`):Keystore 钥带不走
```

- [ ] **Step 2: `spec.md` 的 F346-a 行**

验收栏末尾追加:` **已实现(F346-a 切片)**:store `phone_import` 内存裁剪 + 暂存整份替换;ffi 导出 `inspect_pack`/`import_pack`,结构性守护不依赖 cloud;Keystore 信封;模拟器端到端导入演示包通过。真机 SAF 选文件 + 真实桌面包导入待人工确认`

- [ ] **Step 3: 全量绿**

Run:
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -vE "ok\." ; grep -c "test result: ok" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected:第一条 grep 无输出(无 FAILED/panicked、无非 ok 的 result);clippy、fmt 无输出。

- [ ] **Step 4: 敏感信息扫描**

Run: `git diff main --name-only | xargs grep -nE "192\.168\.|/home/me/brain|PRIVATE KEY|ghp_|github_pat_" 2>/dev/null`
Expected:无输出。

- [ ] **Step 5: 提交**

```bash
git add docs/android-build.md spec.md
git commit -m "$(cat <<'EOF'
docs: F346-a 导入资产生成、本地数据布局与备份策略;spec 状态 (F346)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 6: 交回控制者**

后续(控制者做,不在本计划的实现者范围):起 review 子 agent 做安全终审(CLAUDE.md:涉及凭据的改动完成后必须复核)→ `git merge --squash` 回 main → 推送前再扫一次敏感信息 → 推送;不推 feature 分支;不发桌面 Release、不发 APK(F355)。

## 人工验收清单(无头环境验不了)

- 真机:SAF 选一份**真实桌面导出**的 `.mullionpack` → 输包口令 → 摘要的项目名与计数与桌面一致
- 真机:「设置 → 安全 → 清除凭据」后再导入,出现「本机密钥已失效,旧数据已清空」且导入成功
- 输错口令:提示「口令不对,再输一次」,口令框还在,重输正确后成功
