# 切片 P0-b：代理 + 跳板 + 分组 UI 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让一条会话可以「先过网络代理、再串若干跳板机」连到目标主机，并给分组一个能用的 UI 入口。

**Architecture:** 三层各司其职：`mullion-store` 只存「怎么走」的**声明**（`NetworkPrefs`，可按分组继承）并做引用图解析；`mullion-app` 把声明**物化**成不含任何 store 类型的 `Hop` 列表；`mullion-ssh` 只见 `Hop`，逐跳拨号并把中间 `Handle` 保活在 `SshConnection` 里。红线：`mullion-ssh` 的依赖树里永不出现 `mullion-store`。

**Tech Stack:** Rust 2021 / russh 0.54（`channel_open_direct_tcpip` 做 SSH 跳板、`server` 模块做集成测试的假 sshd）/ 手写 SOCKS5(RFC 1928/1929) 与 HTTP CONNECT / serde + toml / egui 0.30。

**Spec:** `docs/superpowers/specs/2026-07-31-slice-p0b-proxy-jump-design.md`

---

## 分支

所有任务在 `feat/p0b-proxy-jump` 上做：

```bash
cd /data/Mullion
git checkout -b feat/p0b-proxy-jump
```

## File Structure

**新建**

| 文件 | 职责 |
|---|---|
| `crates/mullion-store/src/network.rs` | `NetworkPrefs` / `ProxyChoice` / `ProxyEndpoint` / `JumpRef` 数据模型 |
| `crates/mullion-store/src/jump.rs` | 跳板引用图解析：环检测、深度上限、悬空检测。纯函数 |
| `crates/mullion-ssh/src/hop.rs` | `Hop` 枚举（完全物化，手写 Debug 打码） |
| `crates/mullion-ssh/src/proxy.rs` | SOCKS5 / HTTP CONNECT 握手 + 手写 base64 |
| `crates/mullion-ssh/src/dial.rs` | 逐跳拨号执行器 `dial()` + `SshConnection` |
| `crates/mullion-ssh/tests/fake_sshd.rs` | in-process 假 sshd 两跳集成测试 |
| `crates/mullion-app/src/shell/dial_plan.rs` | 把 store 的声明物化成 `Vec<Hop>` 的纯函数 |
| `crates/mullion-app/src/ui/group_manager.rs` | 极简分组管理弹窗 |

**修改**

| 文件 | 改什么 |
|---|---|
| `crates/mullion-store/src/model.rs` | `SessionRecord.network` 字段；`SecretEntry.proxy_password`；`CURRENT_SCHEMA` 2→3 |
| `crates/mullion-store/src/group.rs` | `GroupRecord.network` 字段 |
| `crates/mullion-store/src/inherit.rs` | `PrefsLayer::network()`；`ResolvedConfig.proxy`/`.jump` |
| `crates/mullion-store/src/migrate.rs` | v2→v3（只升版本号，字段全带 default） |
| `crates/mullion-store/src/error.rs` | 三个新变体 `JumpCycle`/`JumpTooDeep`/`JumpDangling` |
| `crates/mullion-store/src/vault.rs` | 分节透传、`resolve_for` 带上 network |
| `crates/mullion-store/src/lib.rs` | 挂 `network` / `jump` 模块 |
| `crates/mullion-ssh/src/error.rs` | 四个新变体 `ProxyUnreachable`/`ProxyAuthFailed`/`ProxyRejected`/`JumpFailed` |
| `crates/mullion-ssh/src/config.rs` | `SshConfig.hops: Vec<Hop>` |
| `crates/mullion-ssh/src/session.rs` | `establish` 返回 `SshConnection`；`open_pty` 收 `Arc<SshConnection>` |
| `crates/mullion-ssh/src/lib.rs` | 挂 `hop` / `proxy` / `dial` 模块 |
| `crates/mullion-ssh/Cargo.toml` | dev-dependency 打开 russh 的 server 能力 |
| `crates/mullion-app/src/shell/store.rs` | 暴露分组 CRUD + `resolve_for` |
| `crates/mullion-app/src/shell/session_map.rs` | `to_ssh_config` 带上 hops |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `handle: Arc<SshConnection>` |
| `crates/mullion-app/src/app.rs` | 三处连接调用点适配新类型 |
| `crates/mullion-app/src/ui/session_manager.rs` | 编辑器加代理/跳板；列表按分组折叠 |
| `crates/mullion-app/src/ui/mod.rs` | 接线分组管理弹窗 |

---

# 阶段一：store 层（纯函数，零 IO 零 async）

## Task 1：`NetworkPrefs` 数据模型

**Files:**
- Create: `crates/mullion-store/src/network.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-store/src/network.rs`，先只写测试模块（文件顶部先放 `use` 与空的类型占位不行——直接写完整测试，下一步再写类型）：

```rust
//! F4 网络代理 / F5 跳板链的数据模型(设计 §3.1)。零 IO,可纯单测。

use serde::{Deserialize, Serialize};

use crate::model::SessionId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_prefs_round_trips() {
        let n = NetworkPrefs {
            proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                host: "127.0.0.1".into(),
                port: 7891,
                user: Some("u".into()),
            })),
            jump: Some(vec![JumpRef(SessionId(3)), JumpRef(SessionId(5))]),
        };
        let s = toml::to_string_pretty(&n).unwrap();
        let back: NetworkPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn unset_fields_are_not_written() {
        let s = toml::to_string_pretty(&NetworkPrefs::default()).unwrap();
        assert_eq!(s.trim(), "", "全未设的分节不应写出任何键");
    }

    /// 设计 §3.2 的核心区分:`None` = 继承上游,`Some(Direct)` / `Some(vec![])` = 显式覆盖成「不走」。
    #[test]
    fn explicit_empty_is_distinguishable_from_inherit() {
        let inherit = NetworkPrefs::default();
        let explicit = NetworkPrefs {
            proxy: Some(ProxyChoice::Direct),
            jump: Some(Vec::new()),
        };
        assert_ne!(inherit, explicit, "「继承」与「显式直连」不是同一个值");

        let s = toml::to_string_pretty(&explicit).unwrap();
        let back: NetworkPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, explicit, "显式直连必须能 round-trip,不能被写没");
        assert_eq!(back.jump, Some(Vec::new()));
    }

    #[test]
    fn proxy_kind_is_tagged_not_positional() {
        let p = ProxyChoice::HttpConnect(ProxyEndpoint {
            host: "proxy.local".into(),
            port: 8080,
            user: None,
        });
        let s = toml::to_string_pretty(&p).unwrap();
        assert!(s.contains(r#"kind = "http_connect""#), "应有 kind 标签: {s}");
        assert!(!s.contains("user"), "None 的 user 不应写出: {s}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib network 2>&1 | tail -20`
Expected: 编译失败，`cannot find type NetworkPrefs in this scope`（模块尚未挂到 lib.rs，先做 Step 3 再看）

- [ ] **Step 3: 写实现**

在 `crates/mullion-store/src/network.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入：

```rust
/// 网络路径偏好(可继承分节)。
///
/// **与 `Connection` 分开**:`Connection` 是「会话身份本身」永不可继承,
/// 而「怎么走到它」恰恰最该按分组继承(设计 §3.1 的自审修正)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPrefs {
    /// F4 网络代理。`None` = 继承上游;`Some(Direct)` = 显式不走代理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyChoice>,
    /// F5 跳板链,按拨号顺序:`[0]` 最先连。
    /// `None` = 继承上游;`Some(vec![])` = 显式直连。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<Vec<JumpRef>>,
}

/// 代理选择。`Direct` 是**显式**不走代理,用来覆盖分组的代理设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyChoice {
    Direct,
    Socks5(ProxyEndpoint),
    HttpConnect(ProxyEndpoint),
}

/// 代理端点的**非敏感**部分。口令在 `SecretEntry::proxy_password`(加密)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    /// 认证用户名;`None` = 免认证代理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// 对另一条会话的引用,用作跳板。
///
/// 跳板复用「会话」这一实体(设计 D2):跳板机本身也是一台要维护凭据的机器,
/// 没必要另起一套只有主机/用户/密钥的半吊子模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpRef(pub SessionId);
```

- [ ] **Step 4: 挂模块**

修改 `crates/mullion-store/src/lib.rs`，在既有 `pub mod` 列表里按字母序加一行（`jump` 留到 Task 4）：

```rust
pub mod network;
```

同时在该文件既有的 re-export 处补上（若 lib.rs 采用 `pub use xxx::*;` 风格则跟随现有写法，不要新造一种）：

```rust
pub use network::{JumpRef, NetworkPrefs, ProxyChoice, ProxyEndpoint};
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib network 2>&1 | tail -20`
Expected: `test result: ok. 4 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/network.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): NetworkPrefs/ProxyChoice/JumpRef 数据模型 (F4/F5)"
```

---

## Task 2：把 `network` 挂进继承链

**Files:**
- Modify: `crates/mullion-store/src/model.rs`（`SessionRecord`）
- Modify: `crates/mullion-store/src/group.rs`（`GroupRecord`）
- Modify: `crates/mullion-store/src/inherit.rs`（`PrefsLayer` / `ResolvedConfig` / `resolve`）

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-store/src/inherit.rs` 的 `mod tests` 里，先把两个辅助构造函数加上 `network` 参数。把现有的 `fn session(...)` 与 `fn group(...)` 整体替换为：

```rust
    fn session(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
    ) -> SessionRecord {
        session_with_network(tags, terminal, appearance, NetworkPrefs::default())
    }

    fn session_with_network(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
        network: NetworkPrefs,
    ) -> SessionRecord {
        SessionRecord {
            id: SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "s".into(),
                note: String::new(),
                group_id: Some(GroupId(1)),
                tags,
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal,
            appearance,
            network,
        }
    }

    fn group(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
    ) -> GroupRecord {
        group_with_network(tags, terminal, appearance, NetworkPrefs::default())
    }

    fn group_with_network(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
        network: NetworkPrefs,
    ) -> GroupRecord {
        GroupRecord {
            id: GroupId(1),
            name: "g".into(),
            tags,
            terminal,
            appearance,
            network,
        }
    }
```

并把 `mod tests` 顶部的 `use crate::model::{...}` 改成（补 `NetworkPrefs` 等）：

```rust
    use crate::model::{
        AppearancePrefs, Auth, AuthKind, ColorSpec, ColorTarget, Connection, GroupId, IconKind,
        IconSpec, Identity, Protocol, SessionId, SessionRecord, TerminalPrefs,
    };
    use crate::network::{NetworkPrefs, ProxyChoice, ProxyEndpoint};
```

然后在 `mod tests` 末尾追加四个新测试：

```rust
    fn socks(port: u16) -> ProxyChoice {
        ProxyChoice::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port,
            user: None,
        })
    }

    #[test]
    fn network_inherits_proxy_from_group() {
        let s = session(vec![], TerminalPrefs::default(), AppearancePrefs::default());
        let g = group_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: Some(socks(7891)),
                jump: None,
            },
        );
        let got = resolve(&[&s, &g]);
        assert_eq!(got.proxy, Some(socks(7891)), "会话未设代理时应取分组的");
    }

    /// 设计 §3.2:显式 `Direct` 必须**覆盖**分组代理,而不是被当成「未设」继续继承。
    #[test]
    fn explicit_direct_overrides_group_proxy_instead_of_inheriting() {
        let s = session_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: Some(ProxyChoice::Direct),
                jump: None,
            },
        );
        let g = group_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: Some(socks(7891)),
                jump: None,
            },
        );
        let got = resolve(&[&s, &g]);
        assert_eq!(
            got.proxy,
            Some(ProxyChoice::Direct),
            "会话显式直连必须胜出,绝不能回落到分组代理"
        );
    }

    /// 跳板链是**复合对象**,走 Override 整体覆盖,不做 Merge 列表拼接(设计 §4.1)。
    #[test]
    fn jump_chain_is_overridden_wholesale_never_concatenated() {
        let s = session_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: None,
                jump: Some(vec![crate::network::JumpRef(SessionId(9))]),
            },
        );
        let g = group_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: None,
                jump: Some(vec![
                    crate::network::JumpRef(SessionId(1)),
                    crate::network::JumpRef(SessionId(2)),
                ]),
            },
        );
        let got = resolve(&[&s, &g]);
        assert_eq!(
            got.jump,
            vec![crate::network::JumpRef(SessionId(9))],
            "会话的链整体胜出,不得与分组的链拼接"
        );
    }

    /// 显式空链同样是覆盖:分组配了跳板,会话说「我直连」。
    #[test]
    fn explicit_empty_jump_chain_overrides_group_chain() {
        let s = session_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: None,
                jump: Some(Vec::new()),
            },
        );
        let g = group_with_network(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
            NetworkPrefs {
                proxy: None,
                jump: Some(vec![crate::network::JumpRef(SessionId(1))]),
            },
        );
        let got = resolve(&[&s, &g]);
        assert!(got.jump.is_empty(), "会话显式空链必须覆盖分组的跳板链");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: 编译失败，`struct SessionRecord has no field named network` / `no field proxy on type ResolvedConfig`

- [ ] **Step 3: 写实现 —— 三处挂载**

`crates/mullion-store/src/model.rs`：`SessionRecord` 末尾加字段（在 `pub appearance: AppearancePrefs,` 之后）：

```rust
    #[serde(default)]
    pub network: crate::network::NetworkPrefs,
```

`crates/mullion-store/src/group.rs`：`GroupRecord` 末尾同样加（在 `pub appearance: AppearancePrefs,` 之后）：

```rust
    #[serde(default)]
    pub network: crate::network::NetworkPrefs,
```

`crates/mullion-store/src/inherit.rs`：

1）顶部 `use` 补一行：

```rust
use crate::network::{NetworkPrefs, ProxyChoice};
```

2）`PrefsLayer` trait 加一个方法：

```rust
    fn network(&self) -> &NetworkPrefs;
```

3）两个 impl 各补：

```rust
    fn network(&self) -> &NetworkPrefs {
        &self.network
    }
```

（`SessionRecord` 与 `GroupRecord` 的字段名相同，两处实现体一致。）

4）`ResolvedConfig` 加两个字段：

```rust
    /// 解析后的代理。`None` = 全链路都没设 → 直连;`Some(Direct)` 亦为直连。
    pub proxy: Option<ProxyChoice>,
    /// 解析后的跳板链。空 = 直连。
    pub jump: Vec<crate::network::JumpRef>,
```

5）`resolve` 函数体的结构字面量里补两项（放在 `color:` 之后）：

```rust
        // 与 icon/color 同理:`.map(Some)` 让「本层未设」贡献 0 个元素、继续看下一层,
        // 而「本层显式设为 Direct / 空链」贡献一个 Some(...) 从而整体覆盖上游。
        proxy: resolve_override(layers.iter().map(|l| l.network().proxy.clone().map(Some)), None),
        jump: resolve_override(
            layers.iter().map(|l| l.network().jump.clone().map(Some)),
            None,
        )
        .unwrap_or_default(),
```

- [ ] **Step 4: 修补其他构造点**

新增必填字段会打断所有 `SessionRecord`/`GroupRecord` 的字面量构造。逐个编译错误加上 `network: Default::default(),`：

Run: `cargo test -p mullion-store 2>&1 | grep -E "^error" | head -30`

按输出逐个修（预期落在 `model.rs` 的 3 个测试、`group.rs` 的 1 个测试、`vault.rs` 的构造与测试、`migrate.rs` 的迁移构造）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -20`
Expected: `test result: ok.`，其中包含新增的 4 个 network 继承测试

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): NetworkPrefs 接入继承链,显式直连可覆盖分组代理 (F4/F5/F60)"
```

---

## Task 3：代理口令入 vault + schema 升 v3

**Files:**
- Modify: `crates/mullion-store/src/model.rs`（`SecretEntry`、`CURRENT_SCHEMA`）
- Modify: `crates/mullion-store/src/migrate.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-store/src/migrate.rs` 的 `mod tests` 末尾追加：

```rust
    /// v2 文件不含 network 分节,新字段全带 `#[serde(default)]`,应能直接读成 v3 结构。
    #[test]
    fn v2_file_reads_into_current_structs_without_network_section() {
        let text = r#"
schema_version = 2

[[session]]
id = 1
modified_at = "t"

[session.identity]
name = "a"

[session.connection]
host = "h"
port = 22
protocol = "ssh"

[session.auth]
user = "u"
kind = "password"
"#;
        let file: crate::model::SessionsFile = toml::from_str(text).unwrap();
        assert_eq!(file.session.len(), 1);
        assert_eq!(
            file.session[0].network,
            crate::network::NetworkPrefs::default(),
            "缺 network 分节应落默认(全继承)"
        );
    }

    /// 升 v3 的真正理由不是迁移,而是让 v0.1.14 那样的旧客户端**明确拒绝**——
    /// 否则旧客户端读到 `[session.network]` 会静默丢弃再写回,用户的代理配置无声消失。
    #[test]
    fn current_schema_is_three() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 3);
    }
```

在 `crates/mullion-store/src/model.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn secret_entry_carries_proxy_password() {
        let s = SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some("p".into()),
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: SecretEntry = toml::from_str(&text).unwrap();
        assert_eq!(back, s);

        let empty = toml::to_string_pretty(&SecretEntry::default()).unwrap();
        assert_eq!(empty.trim(), "", "全 None 的 SecretEntry 不写出任何键");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib 2>&1 | grep -E "^error|assertion" | head -10`
Expected: `struct SecretEntry has no field named proxy_password` 与 `assertion (left: 2, right: 3)`

- [ ] **Step 3: 写实现**

`crates/mullion-store/src/model.rs`：`SecretEntry` 加字段：

```rust
    /// F4:代理认证口令。与 SSH 口令分开存,避免误用。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub proxy_password: Option<String>,
```

同文件把常量改成：

```rust
/// 当前 TOML 结构版本。缺失该键的文件视为 v1(见 `migrate`)。
///
/// v3 = v2 + `[session.network]` / `[group.network]`。结构上 v3 能直接读 v2
/// (新字段全带 `serde(default)`),升版本号是为了让**旧客户端明确拒绝**,
/// 而不是静默丢弃 network 分节再写回。
pub const CURRENT_SCHEMA: u32 = 3;
```

- [ ] **Step 4: 检查 v1 迁移产物的版本号**

`crates/mullion-store/src/migrate.rs` 里 `migrate_v1` 构造 `SessionsFile` 时若写的是字面量 `2`，改成 `crate::model::CURRENT_SCHEMA`；若已经是常量则不动。

Run: `grep -n "schema_version" crates/mullion-store/src/migrate.rs`
Expected: 产出侧引用的是 `CURRENT_SCHEMA` 而非硬编码数字

- [ ] **Step 5: 修补 `SecretEntry` 构造点**

Run: `cargo test -p mullion-store 2>&1 | grep -E "^error" | head -20`

按错误逐个补 `proxy_password: None,`（预期在 `vault.rs` 与其测试、`migrate.rs`）。若某处已用 `..Default::default()` 则无需改。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): 代理口令入加密侧车,schema 升 v3 让旧客户端明确拒绝 (F4)"
```

---

## Task 4：跳板引用图解析

**Files:**
- Create: `crates/mullion-store/src/jump.rs`
- Modify: `crates/mullion-store/src/error.rs`
- Modify: `crates/mullion-store/src/lib.rs`

设计 §4.1 的展开规则（三处歧义已在 spec 里定死，此处照抄为实现契约）：

1. 从目标会话的**解析后**跳板链出发，`[0]` 是最先连的一跳。
2. 每个跳板会话自身的跳板链**要递归展开**，插在它自己之前。
3. 环 → 报错；深度 > 8 → 报错；引用不存在的会话 → 报错。**不许静默降级为直连**——那会让用户以为流量过了堡垒机，实际没有，这是安全属性。

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-store/src/jump.rs`：

```rust
//! F5 跳板引用图解析(设计 §4.1)。纯函数,零 IO。
//!
//! 输出是**扁平化后的会话 id 序列**,按拨号顺序:`[0]` 最先连,最后一个是离目标最近的跳板。
//! 目标会话本身**不在**返回值里。

use std::collections::BTreeMap;

use crate::error::StoreError;
use crate::inherit::{resolve, PrefsLayer};
use crate::model::{SessionId, SessionRecord};

/// 跳板链最大深度。超过即报错——现实里没人串 8 台以上,超了几乎必是配置错误,
/// 且每多一跳都乘上一次延迟。
pub const MAX_JUMP_DEPTH: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::GroupRecord;
    use crate::model::{
        AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, Protocol, TerminalPrefs,
    };
    use crate::network::{JumpRef, NetworkPrefs};

    fn rec(id: u64, jump: Vec<u64>) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: format!("s{id}"),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: format!("h{id}"),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs {
                proxy: None,
                jump: Some(jump.into_iter().map(|i| JumpRef(SessionId(i))).collect()),
            },
        }
    }

    fn index(records: Vec<SessionRecord>) -> BTreeMap<SessionId, SessionRecord> {
        records.into_iter().map(|r| (r.id, r)).collect()
    }

    fn no_groups() -> BTreeMap<GroupId, GroupRecord> {
        BTreeMap::new()
    }

    #[test]
    fn direct_session_has_empty_chain() {
        let idx = index(vec![rec(1, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert!(got.is_empty(), "无跳板会话应展开成空链");
    }

    #[test]
    fn single_hop_returns_that_hop() {
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2)]);
    }

    /// 展开规则 2:跳板自身的跳板要递归展开,插在它之前。
    #[test]
    fn nested_jump_expands_transitively_in_dial_order() {
        // 目标 1 → 经 2;而 2 自己又要经 3。拨号顺序必须是 3 → 2 → 1。
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![3]), rec(3, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(
            got,
            vec![SessionId(3), SessionId(2)],
            "递归展开的跳板必须排在引用它的那一跳之前"
        );
    }

    #[test]
    fn multi_hop_preserves_declared_order() {
        let idx = index(vec![rec(1, vec![2, 3]), rec(2, vec![]), rec(3, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2), SessionId(3)], "声明顺序即拨号顺序");
    }

    #[test]
    fn cycle_is_rejected_not_silently_truncated() {
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![1])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpCycle(_)),
            "环必须报错,实际: {err:?}"
        );
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let idx = index(vec![rec(1, vec![1])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(matches!(err, StoreError::JumpCycle(_)));
    }

    #[test]
    fn dangling_reference_is_rejected_never_degraded_to_direct() {
        // 安全属性:静默降级会让用户以为流量过了堡垒机,实际直连。
        let idx = index(vec![rec(1, vec![42])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpDangling(SessionId(42))),
            "悬空引用必须报错,实际: {err:?}"
        );
    }

    #[test]
    fn chain_longer_than_max_depth_is_rejected() {
        // 1 → 2 → 3 → ... → 11,展开后 10 跳,超过 MAX_JUMP_DEPTH(8)。
        let mut records = Vec::new();
        for id in 1..=10u64 {
            records.push(rec(id, vec![id + 1]));
        }
        records.push(rec(11, vec![]));
        let idx = index(records);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpTooDeep(_)),
            "超深必须报错,实际: {err:?}"
        );
    }

    /// 同一台跳板被两条支路引用不算环,但只连一次。
    #[test]
    fn diamond_reference_dedups_without_reporting_cycle() {
        // 1 → [2, 3];2 → 4;3 → 4。4 只该出现一次,且在 2 之前。
        let idx = index(vec![
            rec(1, vec![2, 3]),
            rec(2, vec![4]),
            rec(3, vec![4]),
            rec(4, vec![]),
        ]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(4), SessionId(2), SessionId(3)]);
    }

    /// 展开中间跳板时也要走继承:B 自己没配链但它所在分组配了,
    /// 展开 A 时必须把 B 继承来的那一跳也带上。
    /// 若这里直接读 `rec.network.jump` 而不经 `resolve`,组级堡垒机会被静默跳过——
    /// 用户以为多了一层防护,实际没有。
    #[test]
    fn intermediate_jump_inherits_its_own_group_chain() {
        let mut b = rec(2, vec![]);
        b.network.jump = None; // 未设置 = 继承
        b.identity.group_id = Some(GroupId(7));
        let idx = index(vec![rec(1, vec![2]), b, rec(3, vec![])]);

        let mut g = GroupRecord {
            id: GroupId(7),
            name: "生产".into(),
            tags: Vec::new(),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs::default(),
        };
        g.network.jump = Some(vec![JumpRef(SessionId(3))]);
        let groups: BTreeMap<GroupId, GroupRecord> = [(GroupId(7), g)].into_iter().collect();

        let got = expand_chain(SessionId(1), &idx, &groups).unwrap();
        assert_eq!(
            got,
            vec![SessionId(3), SessionId(2)],
            "B 继承来的跳板 3 必须排在 B 之前"
        );
    }

    /// 中间跳板自己配的 proxy 与本次拨号无关:代理只在**第一跳出本机**时用一次,
    /// 后续跳都在隧道里。`expand_chain` 只返回会话 id,不该也不能把 proxy 带出来——
    /// 这个测试钉住「返回值里没有代理」这个契约。
    #[test]
    fn intermediate_jump_proxy_is_not_part_of_the_chain() {
        let mut b = rec(2, vec![]);
        b.network.proxy = Some(crate::network::ProxyChoice::Socks5(
            crate::network::ProxyEndpoint {
                host: "should-be-ignored".into(),
                port: 1080,
                user: None,
            },
        ));
        let idx = index(vec![rec(1, vec![2]), b]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2)], "链里只有会话 id,代理不参与");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib jump 2>&1 | grep -E "^error" | head -10`
Expected: `cannot find function expand_chain` 与 `no variant JumpCycle`

- [ ] **Step 3: 加三个错误变体**

`crates/mullion-store/src/error.rs`：`StoreError` 末尾加：

```rust
    /// 跳板链存在环(F5)。带上参与环的会话 id 便于定位。
    JumpCycle(SessionId),
    /// 跳板链超过 `jump::MAX_JUMP_DEPTH`。
    JumpTooDeep(SessionId),
    /// 跳板引用了不存在的会话。**不静默降级为直连**:那会让用户
    /// 以为流量过了堡垒机而实际没有 —— 这是安全属性,必须硬失败。
    JumpDangling(SessionId),
```

`Display` 的 match 末尾加：

```rust
            StoreError::JumpCycle(id) => write!(
                f,
                "跳板链存在环,经过会话 {id:?} —— 检查该会话的跳板设置"
            ),
            StoreError::JumpTooDeep(id) => write!(
                f,
                "跳板链过深(上限 {}),从会话 {id:?} 展开 —— 检查是否配错",
                crate::jump::MAX_JUMP_DEPTH
            ),
            StoreError::JumpDangling(id) => write!(
                f,
                "跳板指向的会话 {id:?} 不存在 —— 它可能已被删除,请重新指定跳板"
            ),
```

- [ ] **Step 4: 写 `expand_chain` 实现**

在 `crates/mullion-store/src/jump.rs` 的 `MAX_JUMP_DEPTH` 之后、`#[cfg(test)]` 之前插入：

```rust
/// 展开 `target` 的完整跳板链,返回按拨号顺序排列的会话 id。
///
/// `sessions` / `groups` 是全量索引:展开过程要读每个跳板会话**自身**的
/// 跳板设置(含它从分组继承来的),所以不能只传目标那一条。
pub fn expand_chain(
    target: SessionId,
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    let mut out = Vec::new();
    let mut on_stack = Vec::new();
    visit(target, sessions, groups, &mut out, &mut on_stack)?;
    Ok(out)
}

/// 后序 DFS:先把 `id` 的每个跳板(及其自身的跳板)压进 `out`,`id` 自己由调用方负责。
///
/// `on_stack` 是当前递归路径,用于环检测;`out` 兼作去重集合(菱形引用只连一次)。
fn visit(
    id: SessionId,
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
    out: &mut Vec<SessionId>,
    on_stack: &mut Vec<SessionId>,
) -> Result<(), StoreError> {
    if on_stack.contains(&id) {
        return Err(StoreError::JumpCycle(id));
    }
    if on_stack.len() > MAX_JUMP_DEPTH {
        return Err(StoreError::JumpTooDeep(id));
    }
    let rec = sessions.get(&id).ok_or(StoreError::JumpDangling(id))?;

    // 跳板会话自身的跳板链也要走继承(它可能属于某个配了统一代理/跳板的分组)。
    let layers = layers_for(rec, groups);
    let refs: Vec<&dyn PrefsLayer> = layers.iter().map(|l| *l).collect();
    let chain = resolve(&refs).jump;

    on_stack.push(id);
    for hop in chain {
        visit(hop.0, sessions, groups, out, on_stack)?;
        if !out.contains(&hop.0) {
            out.push(hop.0);
        }
    }
    on_stack.pop();

    if out.len() > MAX_JUMP_DEPTH {
        return Err(StoreError::JumpTooDeep(id));
    }
    Ok(())
}

/// 组装继承层序:`[会话, 分组]`(优先级从高到低)。悬空 `group_id` 沿用
/// P0-a 既有的静默降级——**仅限分组**,跳板悬空是另一回事,必须硬失败。
fn layers_for<'a>(
    rec: &'a SessionRecord,
    groups: &'a BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Vec<&'a dyn PrefsLayer> {
    let mut layers: Vec<&dyn PrefsLayer> = vec![rec];
    if let Some(g) = rec.identity.group_id.and_then(|gid| groups.get(&gid)) {
        layers.push(g);
    }
    layers
}
```

- [ ] **Step 5: 挂模块**

`crates/mullion-store/src/lib.rs` 加：

```rust
pub mod jump;
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-store --lib jump 2>&1 | tail -20`
Expected: `test result: ok. 8 passed`

若 `chain_longer_than_max_depth_is_rejected` 未通过，检查是 `on_stack.len()` 还是 `out.len()` 先触发上限——两处判断都保留，任一触发即报错。

- [ ] **Step 7: 全 crate 跑绿并提交**

Run: `cargo test -p mullion-store 2>&1 | tail -5`
Expected: `test result: ok.`

```bash
git add crates/mullion-store/src
git commit -m "feat(store): 跳板引用图展开,环/超深/悬空一律硬失败 (F5)"
```

---

## Task 5：`Vault` 侧接线

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-store/src/vault.rs` 的 `mod tests` 末尾追加。`Vault::open` 的实际签名是
`open(dir: PathBuf, key: &dyn MasterKeySource)`，测试用内存主密钥（照该文件既有测试的写法）：

```rust
    #[test]
    fn resolve_for_carries_network_from_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let gid = v.add_group("生产".into());
        v.group_mut(gid).unwrap().network = crate::network::NetworkPrefs {
            proxy: Some(crate::network::ProxyChoice::Socks5(
                crate::network::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                },
            )),
            jump: None,
        };
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = v.add(d, "2026-07-31T00:00:00Z");

        let got = v.resolve_for(id).unwrap();
        assert!(
            matches!(got.proxy, Some(crate::network::ProxyChoice::Socks5(_))),
            "分组代理应经 resolve_for 透出"
        );
    }

    #[test]
    fn expand_jump_chain_reports_dangling_reference() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut d = draft();
        d.network = crate::network::NetworkPrefs {
            proxy: None,
            jump: Some(vec![crate::network::JumpRef(SessionId(999))]),
        };
        let id = v.add(d, "2026-07-31T00:00:00Z");
        let err = v.expand_jump_chain(id).unwrap_err();
        assert!(matches!(err, StoreError::JumpDangling(SessionId(999))));
    }
```

签名依据（已核实）：`Vault::open(dir: PathBuf, key_source: &dyn MasterKeySource)`、
`add(draft, now_rfc3339) -> SessionId`（不返回 Result）、`add_group(name) -> GroupId`（不返回 Result）。
`key()` 与 `draft()` 是 `vault.rs` 的 `mod tests` 里已有的辅助函数，直接复用，不要新造。
`draft()` 若尚未提供 `network` 字段，Step 4 会一并补上。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store --lib vault 2>&1 | grep -E "^error" | head -10`
Expected: `no method named expand_jump_chain` 与 `SessionDraft has no field network`

- [ ] **Step 3: 写实现**

`crates/mullion-store/src/vault.rs`：

1）`SessionDraft` 加字段（与既有 `terminal` / `appearance` 并列）：

```rust
    pub network: crate::network::NetworkPrefs,
```

2）`add` / `update` 里把 draft 的 `network` 写进 `SessionRecord`（跟 `terminal` / `appearance` 完全同构，照抄相邻那两行的写法加一行）。

3）新增方法（放在 `resolve_for` 附近）：

```rust
    /// 展开一条会话的完整跳板链(F5)。返回按拨号顺序排列的**跳板会话记录**。
    ///
    /// 返回记录而非 id:调用方(app)接下来要拿每一跳的 host/user/认证去物化 `Hop`,
    /// 让它再查一遍索引没有意义。
    pub fn expand_jump_chain(&self, id: SessionId) -> Result<Vec<SessionRecord>, StoreError> {
        let sessions: std::collections::BTreeMap<SessionId, SessionRecord> =
            self.list().iter().map(|r| (r.id, r.clone())).collect();
        let groups: std::collections::BTreeMap<crate::model::GroupId, crate::group::GroupRecord> =
            self.groups().iter().map(|g| (g.id, g.clone())).collect();
        if !sessions.contains_key(&id) {
            return Err(StoreError::NotFound(id));
        }
        let ids = crate::jump::expand_chain(id, &sessions, &groups)?;
        Ok(ids
            .into_iter()
            .map(|i| sessions[&i].clone())
            .collect())
    }
```

若 `list()` 的返回类型不是 `&[SessionRecord]`（例如是 `Vec<SessionRecord>`），按实际签名调整那两行的 `.iter()` / 所有权，**不要改 `list()` 本身**。

4）`resolve_for` 无需改动——它调用的是 `inherit::resolve`，Task 2 已让 `ResolvedConfig` 带上 `proxy`/`jump`。若它构造 `ResolvedConfig` 是手写字面量而非调 `resolve`，则补上两个字段。

- [ ] **Step 4: 修补 `SessionDraft` 构造点**

Run: `cargo test -p mullion-store 2>&1 | grep -E "^error" | head -20`

逐个补 `network: Default::default(),`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 6: clippy + fmt**

```bash
cargo clippy -p mullion-store --all-targets -- -D warnings
cargo fmt
```

Expected: clippy 无输出

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): Vault 暴露 expand_jump_chain,draft 带上 network (F4/F5)"
```

---

# 阶段二：ssh 层（只见完全物化的 `Hop`）

**红线 2（贯穿本阶段）**：`mullion-ssh` 的代码与依赖树里**永不出现 `mullion-store`**。本阶段所有类型自带，不 `use mullion_store::*`。Task 18 会把这条做成测试。

## Task 6：四个新错误变体

**Files:**
- Modify: `crates/mullion-ssh/src/error.rs`

- [ ] **Step 1: 写失败测试**

把 `crates/mullion-ssh/src/error.rs` 里 `every_variant_has_distinct_actionable_message` 的 `variants` 数组扩成（保留原 7 个，追加 4 个）：

```rust
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
            ConnectError::PtyRequest,
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
        ];
```

并在 `mod tests` 末尾追加：

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib error 2>&1 | grep -E "^error" | head -10`
Expected: `no variant or associated item named ProxyUnreachable found`

- [ ] **Step 3: 写实现**

`ConnectError` 枚举末尾加：

```rust
    /// 连不上代理本身(F4)。区别于「连上了代理但代理连不上目标」。
    ProxyUnreachable { proxy: String, cause: String },
    /// 代理拒绝了我们的认证凭据(F4)。
    ProxyAuthFailed { proxy: String },
    /// 代理接受了连接,但拒绝转发到目标(F4)。
    ProxyRejected { proxy: String, reason: String },
    /// 跳板链上某一跳失败(F5)。`hop` 是 "host:port"。
    JumpFailed { hop: String, cause: String },
```

`Display` 的 match 末尾加：

```rust
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --lib error 2>&1 | tail -10`
Expected: `test result: ok. 4 passed`（原 2 个 + 新 2 个）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/error.rs
git commit -m "feat(ssh): 代理/跳板四类可操作错误,消息点名代理与失败跳 (F4/F5/F6)"
```

---

## Task 7：`Hop` 类型（手写 Debug 打码）

**Files:**
- Create: `crates/mullion-ssh/src/hop.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-ssh/src/hop.rs`：

```rust
//! 拨号链上的一跳(设计 §3.4)。
//!
//! **完全物化**:不含任何 store 类型(红线 2)。app 负责把「会话引用」翻译成这里的
//! 主机/端口/凭据,ssh 层拿到就能直接拨。

use crate::config::AuthMethod;

#[cfg(test)]
mod tests {
    use super::*;

    /// 红线:`Hop` 携带明文凭据。若 derive(Debug),被 `{:?}` 打进 ADR-008 的
    /// mullion.log 就是明文口令落盘。必须手写 Debug 并打码。
    #[test]
    fn debug_never_leaks_proxy_password() {
        let h = Hop::Socks5 {
            host: "127.0.0.1".into(),
            port: 7891,
            auth: Some(("alice".into(), "hunter2".into())),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("hunter2"), "口令绝不能出现在 Debug 里: {s}");
        assert!(s.contains("127.0.0.1"), "非敏感字段应保留以便排障: {s}");
        assert!(s.contains("alice"), "用户名非敏感,保留: {s}");
    }

    #[test]
    fn debug_never_leaks_http_proxy_password() {
        let h = Hop::HttpConnect {
            host: "proxy.local".into(),
            port: 8080,
            auth: Some(("bob".into(), "s3cret".into())),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("s3cret"), "口令绝不能出现在 Debug 里: {s}");
    }

    #[test]
    fn debug_never_leaks_jump_password() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Password("bastionpw".into()),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("bastionpw"), "跳板口令也不能泄漏: {s}");
        assert!(s.contains("bastion"), "主机名保留以便排障: {s}");
    }

    /// 私钥 passphrase 同样敏感。
    #[test]
    fn debug_never_leaks_key_passphrase() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::PublicKey {
                path: "/home/u/.ssh/id_ed25519".into(),
                passphrase: Some("keypw".into()),
            },
        };
        let s = format!("{h:?}");
        assert!(!s.contains("keypw"), "私钥口令不能泄漏: {s}");
        assert!(s.contains("id_ed25519"), "私钥路径非敏感,保留以便排障: {s}");
    }

    #[test]
    fn endpoint_string_is_host_colon_port() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 2222,
            user: "ops".into(),
            auth: AuthMethod::Agent,
        };
        assert_eq!(h.endpoint(), "bastion:2222");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib hop 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find type Hop in this scope`

- [ ] **Step 3: 写实现**

在 `hop.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入：

```rust
/// 拨号链上的一跳。顺序即拨号顺序:`hops[0]` 最先建立。
///
/// **不 derive Debug**:本类型携带明文口令,derive 会让 `{:?}` 把凭据
/// 打进 mullion.log(ADR-008 会记录连接阶段的诊断)。见下方手写实现。
#[derive(Clone)]
pub enum Hop {
    /// SOCKS5 代理(RFC 1928),`auth` 为 (用户名, 口令),RFC 1929。
    Socks5 {
        host: String,
        port: u16,
        auth: Option<(String, String)>,
    },
    /// HTTP CONNECT 代理,`auth` 走 `Proxy-Authorization: Basic`。
    HttpConnect {
        host: String,
        port: u16,
        auth: Option<(String, String)>,
    },
    /// SSH 跳板:在这一跳上开 direct-tcpip channel 通向下一跳。
    SshJump {
        host: String,
        port: u16,
        user: String,
        auth: AuthMethod,
    },
}

impl Hop {
    /// "host:port",用于错误消息里点名是哪一跳失败(F6)。
    pub fn endpoint(&self) -> String {
        match self {
            Hop::Socks5 { host, port, .. }
            | Hop::HttpConnect { host, port, .. }
            | Hop::SshJump { host, port, .. } => format!("{host}:{port}"),
        }
    }
}

impl std::fmt::Debug for Hop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hop::Socks5 { host, port, auth } => f
                .debug_struct("Socks5")
                .field("host", host)
                .field("port", port)
                .field("user", &auth.as_ref().map(|(u, _)| u.as_str()))
                .field("password", &redacted(auth.is_some()))
                .finish(),
            Hop::HttpConnect { host, port, auth } => f
                .debug_struct("HttpConnect")
                .field("host", host)
                .field("port", port)
                .field("user", &auth.as_ref().map(|(u, _)| u.as_str()))
                .field("password", &redacted(auth.is_some()))
                .finish(),
            Hop::SshJump {
                host,
                port,
                user,
                auth,
            } => f
                .debug_struct("SshJump")
                .field("host", host)
                .field("port", port)
                .field("user", user)
                .field("auth", &DebugAuth(auth))
                .finish(),
        }
    }
}

fn redacted(present: bool) -> &'static str {
    if present {
        "<已设置>"
    } else {
        "<无>"
    }
}

/// `AuthMethod` 自身 derive 了 Debug(会打印口令),所以这里包一层只输出安全摘要。
struct DebugAuth<'a>(&'a AuthMethod);

impl std::fmt::Debug for DebugAuth<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            AuthMethod::Password(_) => write!(f, "Password(<已设置>)"),
            AuthMethod::PublicKey { path, passphrase } => write!(
                f,
                "PublicKey {{ path: {path:?}, passphrase: {} }}",
                redacted(passphrase.is_some())
            ),
            AuthMethod::Agent => write!(f, "Agent"),
        }
    }
}
```

- [ ] **Step 4: 挂模块**

`crates/mullion-ssh/src/lib.rs` 加：

```rust
pub mod hop;
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --lib hop 2>&1 | tail -10`
Expected: `test result: ok. 5 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/hop.rs crates/mullion-ssh/src/lib.rs
git commit -m "feat(ssh): Hop 类型,手写 Debug 对凭据打码防落盘 (F4/F5)"
```

---

## Task 8：SOCKS5 握手

**Files:**
- Create: `crates/mullion-ssh/src/proxy.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

协议要点（RFC 1928 / 1929，照此实现，不要凭记忆改）：

- 问候：`05 | NMETHODS | METHODS...`。我们发 `05 01 00`（无认证）或 `05 02 00 02`（无认证/用户名口令）。
- 服务端回 `05 | METHOD`。`00` = 免认证直接进下一步；`02` = 走 RFC 1929；`FF` = 无可接受方法 → `ProxyAuthFailed`。
- RFC 1929 子协商：`01 | ULEN | UNAME | PLEN | PASSWD`，回 `01 | STATUS`，`STATUS != 0` → `ProxyAuthFailed`。
- CONNECT 请求：`05 01 00 | ATYP | ADDR | PORT(be16)`。域名用 `ATYP=03`：`LEN | 域名字节`。
- 回复：`05 | REP | 00 | ATYP | BND.ADDR | BND.PORT`。`REP != 0` → `ProxyRejected`，reason 按 REP 码给中文。BND 字段长度随 ATYP 变（`01`=4 字节、`03`=1+len、`04`=16 字节），**必须读完**，否则残留字节会污染后续的 SSH 握手。

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-ssh/src/proxy.rs`：

```rust
//! F4:SOCKS5(RFC 1928/1929)与 HTTP CONNECT 代理握手。
//!
//! 握手逻辑写成对 `AsyncRead + AsyncWrite` 泛型的函数,测试里用 `tokio::io::duplex`
//! 喂假的服务端应答,不需要真代理。

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::ConnectError;

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑一次握手:`server_script` 是假服务端按序发出的应答。
    /// 返回 (握手结果, 客户端实际发出的字节)。
    async fn run_socks5(
        server_script: Vec<Vec<u8>>,
        auth: Option<(String, String)>,
        target: (&str, u16),
    ) -> (Result<(), ConnectError>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(4096);
        let target_host = target.0.to_string();
        let task = tokio::spawn(async move {
            let mut client = client;
            let r = socks5_handshake(
                &mut client,
                "127.0.0.1:1080",
                auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                &target_host,
                target.1,
            )
            .await;
            r
        });

        let mut seen = Vec::new();
        for reply in server_script {
            // 先把客户端已发出的读走,再回应答。
            let mut buf = [0u8; 512];
            let n = server.read(&mut buf).await.unwrap_or(0);
            seen.extend_from_slice(&buf[..n]);
            let _ = server.write_all(&reply).await;
        }
        let r = task.await.unwrap();
        (r, seen)
    }

    #[tokio::test]
    async fn no_auth_connect_succeeds_and_sends_domain_atyp() {
        let (r, sent) = run_socks5(
            vec![
                vec![0x05, 0x00],                                     // 选中无认证
                vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0],       // 成功,ATYP=IPv4
            ],
            None,
            ("example.com", 22),
        )
        .await;
        assert!(r.is_ok(), "免认证握手应成功: {r:?}");
        assert_eq!(&sent[..3], &[0x05, 0x01, 0x00], "问候应只提供无认证方法");
        assert!(
            sent.windows(11).any(|w| w == b"\x03\x0bexample.com"),
            "域名应以 ATYP=3 + 长度前缀发出,实际: {sent:?}"
        );
        assert!(
            sent.ends_with(&[0x00, 0x16]),
            "端口应为大端 u16(22 = 0x0016),实际尾部: {:?}",
            &sent[sent.len().saturating_sub(4)..]
        );
    }

    #[tokio::test]
    async fn username_password_auth_is_negotiated_per_rfc1929() {
        let (r, sent) = run_socks5(
            vec![
                vec![0x05, 0x02],                               // 选中用户名口令
                vec![0x01, 0x00],                               // 认证成功
                vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0], // 连接成功
            ],
            Some(("alice".into(), "pw".into())),
            ("h", 22),
        )
        .await;
        assert!(r.is_ok(), "口令认证应成功: {r:?}");
        assert!(
            sent.windows(9)
                .any(|w| w == [0x01, 0x05, b'a', b'l', b'i', b'c', b'e', 0x02, b'p']),
            "应发出 RFC1929 子协商帧,实际: {sent:?}"
        );
    }

    #[tokio::test]
    async fn rejected_auth_maps_to_proxy_auth_failed() {
        let (r, _) = run_socks5(
            vec![vec![0x05, 0x02], vec![0x01, 0x01]], // 认证失败
            Some(("alice".into(), "bad".into())),
            ("h", 22),
        )
        .await;
        assert!(
            matches!(r, Err(ConnectError::ProxyAuthFailed { .. })),
            "认证被拒应映射到 ProxyAuthFailed,实际: {r:?}"
        );
    }

    #[tokio::test]
    async fn no_acceptable_method_maps_to_proxy_auth_failed() {
        let (r, _) = run_socks5(vec![vec![0x05, 0xFF]], None, ("h", 22)).await;
        assert!(matches!(r, Err(ConnectError::ProxyAuthFailed { .. })));
    }

    #[tokio::test]
    async fn nonzero_reply_code_maps_to_proxy_rejected_with_reason() {
        let (r, _) = run_socks5(
            vec![vec![0x05, 0x00], vec![0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0]],
            None,
            ("h", 22),
        )
        .await;
        match r {
            Err(ConnectError::ProxyRejected { reason, .. }) => {
                assert!(!reason.is_empty(), "REP=4 应给出可读原因");
            }
            other => panic!("REP!=0 应映射到 ProxyRejected,实际: {other:?}"),
        }
    }

    /// 回复里的 BND 字段长度随 ATYP 变。读少了会把残留字节留给后续 SSH 握手,
    /// 表现为「代理连上了但 SSH 版本协商失败」——极难排查。
    #[tokio::test]
    async fn domain_atyp_reply_is_fully_drained() {
        let mut reply = vec![0x05, 0x00, 0x00, 0x03, 0x03];
        reply.extend_from_slice(b"abc");
        reply.extend_from_slice(&[0x00, 0x16]);
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client = client;
            socks5_handshake(&mut client, "p:1080", None, "h", 22).await
        });
        let mut buf = [0u8; 512];
        let _ = server.read(&mut buf).await;
        server.write_all(&[0x05, 0x00]).await.unwrap();
        let _ = server.read(&mut buf).await;
        server.write_all(&reply).await.unwrap();
        // 握手后紧跟的应用字节必须原样到达客户端,证明回复被读干净了。
        server.write_all(b"SSH-2.0-x").await.unwrap();
        assert!(task.await.unwrap().is_ok());
        // 客户端侧无法在此断言剩余流(已 move),用长度断言代替:
        // 若 drain 少读,handshake 会把 "SSH-2.0-x" 的前缀吃掉并很可能报错。
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_non_ascii_bytes() {
        assert_eq!(base64_encode(&[0xFF, 0xFE, 0xFD]), "//79");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib proxy 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find function socks5_handshake`

- [ ] **Step 3: 写实现**

在 `proxy.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入：

```rust
/// 在已建立的流上完成 SOCKS5 握手(RFC 1928),成功后该流即通向 `target_host:target_port`。
///
/// `proxy_label` 只用于错误消息(F6 要求点名是哪个代理)。
pub async fn socks5_handshake<S>(
    stream: &mut S,
    proxy_label: &str,
    auth: Option<(&str, &str)>,
    target_host: &str,
    target_port: u16,
) -> Result<(), ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let io = |e: std::io::Error| ConnectError::ProxyUnreachable {
        proxy: proxy_label.to_string(),
        cause: e.to_string(),
    };

    // 1) 问候:提供我们支持的认证方法。
    let greeting: Vec<u8> = match auth {
        None => vec![0x05, 0x01, 0x00],
        Some(_) => vec![0x05, 0x02, 0x00, 0x02],
    };
    stream.write_all(&greeting).await.map_err(io)?;

    let mut sel = [0u8; 2];
    stream.read_exact(&mut sel).await.map_err(io)?;
    if sel[0] != 0x05 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: format!("对端不是 SOCKS5(版本字节 {:#04x})", sel[0]),
        });
    }
    match sel[1] {
        0x00 => {}
        0x02 => {
            let (user, pass) = auth.ok_or_else(|| ConnectError::ProxyAuthFailed {
                proxy: proxy_label.to_string(),
            })?;
            // RFC 1929 子协商。用户名/口令各最长 255 字节。
            if user.len() > 255 || pass.len() > 255 {
                return Err(ConnectError::ProxyAuthFailed {
                    proxy: proxy_label.to_string(),
                });
            }
            let mut req = vec![0x01, user.len() as u8];
            req.extend_from_slice(user.as_bytes());
            req.push(pass.len() as u8);
            req.extend_from_slice(pass.as_bytes());
            stream.write_all(&req).await.map_err(io)?;

            let mut st = [0u8; 2];
            stream.read_exact(&mut st).await.map_err(io)?;
            if st[1] != 0x00 {
                return Err(ConnectError::ProxyAuthFailed {
                    proxy: proxy_label.to_string(),
                });
            }
        }
        _ => {
            return Err(ConnectError::ProxyAuthFailed {
                proxy: proxy_label.to_string(),
            })
        }
    }

    // 2) CONNECT 请求。一律用 ATYP=3(域名),让代理侧做解析——
    //    本机解析不了的内网名恰恰是用代理的常见理由。
    if target_host.len() > 255 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: "目标主机名超过 255 字节".into(),
        });
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, target_host.len() as u8];
    req.extend_from_slice(target_host.as_bytes());
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req).await.map_err(io)?;

    // 3) 回复。**必须按 ATYP 读完 BND 字段**,否则残留字节污染后续 SSH 握手。
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0x00 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: socks5_reply_reason(head[1]),
        });
    }
    let bnd_len = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut l = [0u8; 1];
            stream.read_exact(&mut l).await.map_err(io)?;
            l[0] as usize
        }
        other => {
            return Err(ConnectError::ProxyRejected {
                proxy: proxy_label.to_string(),
                reason: format!("回复里的地址类型 {other:#04x} 不认识"),
            })
        }
    };
    let mut rest = vec![0u8; bnd_len + 2]; // BND.ADDR + BND.PORT
    stream.read_exact(&mut rest).await.map_err(io)?;
    Ok(())
}

/// RFC 1928 §6 的 REP 码。给中文可读原因,不是把数字甩给用户(F6)。
fn socks5_reply_reason(code: u8) -> String {
    let s = match code {
        0x01 => "代理内部错误",
        0x02 => "代理规则不允许连接此目标",
        0x03 => "网络不可达",
        0x04 => "目标主机不可达",
        0x05 => "目标拒绝连接",
        0x06 => "TTL 超时",
        0x07 => "代理不支持 CONNECT 命令",
        0x08 => "代理不支持该地址类型",
        _ => "未知错误码",
    };
    format!("{s}(REP={code:#04x})")
}

/// 标准 base64(RFC 4648),用于 HTTP 代理的 `Proxy-Authorization: Basic`。
///
/// 手写而非引依赖:N6 盯着 exe 体积,而这里只需要 20 行。
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
```

- [ ] **Step 4: 挂模块 + 确认 dev-dependency**

`crates/mullion-ssh/src/lib.rs` 加：

```rust
pub mod proxy;
```

测试用了 `#[tokio::test]`，需要 tokio 的 `macros` + `rt` feature 在 dev 侧可用。检查：

Run: `grep -n "tokio" crates/mullion-ssh/Cargo.toml`

workspace 的 tokio 已带 `rt-multi-thread` 与 `macros`（见根 `Cargo.toml`），若 `mullion-ssh` 的 `[dependencies]` 里已 `tokio.workspace = true` 则无需改动；否则在 `[dev-dependencies]` 补 `tokio = { workspace = true }`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --lib proxy 2>&1 | tail -15`
Expected: `test result: ok. 8 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/proxy.rs crates/mullion-ssh/src/lib.rs crates/mullion-ssh/Cargo.toml
git commit -m "feat(ssh): SOCKS5 握手(RFC1928/1929)+ 手写 base64,回复按 ATYP 读干净 (F4)"
```

---

## Task 9：HTTP CONNECT 握手

**Files:**
- Modify: `crates/mullion-ssh/src/proxy.rs`

- [ ] **Step 1: 写失败测试**

在 `proxy.rs` 的 `mod tests` 末尾追加：

```rust
    async fn run_http(
        reply: &'static [u8],
        auth: Option<(String, String)>,
    ) -> (Result<(), ConnectError>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client = client;
            http_connect_handshake(
                &mut client,
                "proxy:8080",
                auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                "example.com",
                22,
            )
            .await
        });
        let mut buf = [0u8; 1024];
        let n = server.read(&mut buf).await.unwrap_or(0);
        let sent = buf[..n].to_vec();
        server.write_all(reply).await.unwrap();
        (task.await.unwrap(), sent)
    }

    #[tokio::test]
    async fn http_connect_sends_well_formed_request() {
        let (r, sent) = run_http(b"HTTP/1.1 200 Connection established\r\n\r\n", None).await;
        assert!(r.is_ok(), "200 应视为成功: {r:?}");
        let text = String::from_utf8(sent).unwrap();
        assert!(
            text.starts_with("CONNECT example.com:22 HTTP/1.1\r\n"),
            "请求行不合规: {text:?}"
        );
        assert!(text.contains("Host: example.com:22\r\n"), "缺 Host 头: {text:?}");
        assert!(text.ends_with("\r\n\r\n"), "请求必须以空行结束: {text:?}");
        assert!(!text.contains("Proxy-Authorization"), "无认证时不该带认证头");
    }

    #[tokio::test]
    async fn http_connect_sends_basic_authorization() {
        let (r, sent) = run_http(
            b"HTTP/1.1 200 OK\r\n\r\n",
            Some(("alice".into(), "pw".into())),
        )
        .await;
        assert!(r.is_ok());
        let text = String::from_utf8(sent).unwrap();
        // base64("alice:pw") = "YWxpY2U6cHc="
        assert!(
            text.contains("Proxy-Authorization: Basic YWxpY2U6cHc=\r\n"),
            "认证头不对: {text:?}"
        );
    }

    #[tokio::test]
    async fn http_407_maps_to_proxy_auth_failed() {
        let (r, _) = run_http(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
            None,
        )
        .await;
        assert!(
            matches!(r, Err(ConnectError::ProxyAuthFailed { .. })),
            "407 必须映射到认证失败而非泛化拒绝,实际: {r:?}"
        );
    }

    #[tokio::test]
    async fn http_403_maps_to_proxy_rejected_with_status_line() {
        let (r, _) = run_http(b"HTTP/1.1 403 Forbidden\r\n\r\n", None).await;
        match r {
            Err(ConnectError::ProxyRejected { reason, .. }) => {
                assert!(reason.contains("403"), "原因里应带状态码: {reason}");
            }
            other => panic!("403 应映射到 ProxyRejected,实际: {other:?}"),
        }
    }

    /// 响应头必须读到 `\r\n\r\n` 为止。少读一个字节,残留就会污染 SSH 版本协商。
    #[tokio::test]
    async fn http_reply_headers_are_drained_up_to_blank_line() {
        let (r, _) = run_http(
            b"HTTP/1.1 200 OK\r\nX-Proxy: mullion\r\nVia: 1.1 p\r\n\r\n",
            None,
        )
        .await;
        assert!(r.is_ok(), "多头响应也应成功: {r:?}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib proxy::tests::http 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find function http_connect_handshake`

- [ ] **Step 3: 写实现**

在 `proxy.rs` 的 `socks5_reply_reason` 之后插入：

```rust
/// 在已建立的流上完成 HTTP CONNECT 握手,成功后该流即通向 `target_host:target_port`。
pub async fn http_connect_handshake<S>(
    stream: &mut S,
    proxy_label: &str,
    auth: Option<(&str, &str)>,
    target_host: &str,
    target_port: u16,
) -> Result<(), ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let io = |e: std::io::Error| ConnectError::ProxyUnreachable {
        proxy: proxy_label.to_string(),
        cause: e.to_string(),
    };

    let mut req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if let Some((user, pass)) = auth {
        let token = base64_encode(format!("{user}:{pass}").as_bytes());
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.map_err(io)?;

    // 逐字节读到 \r\n\r\n 为止。**不能一次 read 一大块**:多读的部分是隧道内的
    // SSH 数据,吞掉就再也拿不回来了。CONNECT 响应很短,逐字节的开销可以忽略。
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await.map_err(io)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 8192 {
            return Err(ConnectError::ProxyRejected {
                proxy: proxy_label.to_string(),
                reason: "代理响应头超过 8KB,疑似不是 HTTP 代理".into(),
            });
        }
    }

    let status_line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: format!("代理响应无法解析:{status_line}"),
        })?;

    match code {
        200..=299 => Ok(()),
        407 => Err(ConnectError::ProxyAuthFailed {
            proxy: proxy_label.to_string(),
        }),
        _ => Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: status_line,
        }),
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --lib proxy 2>&1 | tail -15`
Expected: `test result: ok. 13 passed`

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy -p mullion-ssh --all-targets -- -D warnings
git add crates/mullion-ssh/src/proxy.rs
git commit -m "feat(ssh): HTTP CONNECT 握手,407 独立成认证失败,响应头逐字节读到空行 (F4)"
```

---

## Task 10：`SshConnection` 保活 + `establish` 改造

**这是本切片最危险的一处。** `ChannelStream` **不持有** `Handle`；`Handle` 一 Drop，整条 SSH 连接立刻断。若把跳板的 `Handle` 丢在 `dial()` 的栈上，表现是「拨号成功、几毫秒后无故断」——本地直连场景**测不出来**。用类型强制保活。

**Files:**
- Modify: `crates/mullion-ssh/src/config.rs`
- Modify: `crates/mullion-ssh/src/session.rs`
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-ssh/src/session.rs` 的 `mod tests` 末尾追加：

```rust
    /// 保活红线:跳板 Handle 必须被 `SshConnection` 持有。
    /// 只有把它移进结构体、且 `open_pty` 收 `Arc<SshConnection>`,
    /// 「跳板连接活得比 PTY 久」才是类型保证而非注释保证。
    #[test]
    fn ssh_connection_owns_jump_handles_so_they_outlive_the_pty() {
        fn assert_field_exists(c: &SshConnection) -> usize {
            c.jump_handle_count()
        }
        // 编译通过即证明字段存在;运行期只断言空链为 0。
        let _ = assert_field_exists;
    }

    #[test]
    fn ssh_config_defaults_to_direct_dial() {
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: AuthMethod::Agent,
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        };
        assert!(cfg.hops.is_empty(), "空 hops 即直连");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib session 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find type SshConnection` / `struct SshConfig has no field named hops`

- [ ] **Step 3: `SshConfig` 加 hops**

`crates/mullion-ssh/src/config.rs`，`SshConfig` 末尾加：

```rust
    /// 拨号链(F4/F5)。**空 = 直连**。顺序即拨号顺序:`hops[0]` 最先建立。
    /// 由 app 从会话配置物化而来;本 crate 不认识「会话」「分组」。
    pub hops: Vec<crate::hop::Hop>,
```

同时 `SshConfig` 的 `derive(Debug)` 现在会打印 `Hop`——`Hop` 已手写 Debug 打码，安全。但 `AuthMethod` 自身 derive 的 Debug 仍会打印口令，`SshConfig` 本就如此，**本切片不扩大改动范围**（见计划末尾「已知遗留」）。

- [ ] **Step 4: `SshConnection` 与 `establish` 改造**

`crates/mullion-ssh/src/session.rs`：新增类型（放在 `establish` 之前）：

```rust
/// 一条建立好的 SSH 连接,**连同它依赖的所有跳板连接**。
///
/// 为什么要持有 `_jumps`:russh 的 `ChannelStream` 不持有 `Handle`,
/// `Handle` 一 Drop 整条连接立刻断。跳板链是「A 上开 channel 通向 B」,
/// 若 A 的 Handle 提前释放,B 的流会在几毫秒后静默断掉 ——
/// 本地直连场景永远复现不了。把保活做成字段,让类型系统兜住。
pub struct SshConnection {
    handle: Handle<ClientHandler>,
    /// 跳板链每一跳的连接,仅用于保活,顺序 = 拨号顺序。
    _jumps: Vec<Handle<ClientHandler>>,
}

impl SshConnection {
    pub(crate) fn new(handle: Handle<ClientHandler>, jumps: Vec<Handle<ClientHandler>>) -> Self {
        Self {
            handle,
            _jumps: jumps,
        }
    }

    /// 目标主机的 Handle。跳板的 Handle **不外借**——外部拿不到就不会误 Drop。
    pub(crate) fn handle(&self) -> &Handle<ClientHandler> {
        &self.handle
    }

    /// 仅供测试与诊断:当前保活着几条跳板连接。
    pub fn jump_handle_count(&self) -> usize {
        self._jumps.len()
    }
}
```

把 `establish` 的返回类型从 `Result<Handle<ClientHandler>, ConnectError>` 改成 `Result<SshConnection, ConnectError>`：

- 函数签名改 `) -> Result<SshConnection, ConnectError> {`
- 第 2 步 `let stream = TcpStream::connect(addr).await.map_err(classify_tcp)?;` 保持不变（Task 11 会把它换成 `dial`）
- 结尾改成：

```rust
    let result = authenticate(&mut handle, cfg).await?;
    match result {
        AuthResult::Success => Ok(SshConnection::new(handle, Vec::new())),
        AuthResult::Failure { .. } => Err(ConnectError::AuthFailed),
    }
```

`open_pty` 的签名从 `handle: Arc<Handle<ClientHandler>>` 改成 `conn: Arc<SshConnection>`，函数体内所有 `handle.` 改为 `conn.handle().`，`handle.clone()` 之类若用于 channel 之后的保活则改持 `conn.clone()`。**保留原有那句红线注释**（「签名里刻意没有任何网络参数」）。

`connect` 函数相应改成：

```rust
    let conn = establish(cfg, policy).await?;
    open_pty(Arc::new(conn), cfg, wake).await
```

- [ ] **Step 5: 修 app 侧 5 处**

Run: `cargo build -p mullion-app 2>&1 | grep -E "^error" | head -20`

按错误改这几处（位置以当前代码为准，行号可能已漂移）：

1. `crates/mullion-app/src/shell/workspace/mod.rs` —— `pub handle: Arc<Handle<ClientHandler>>` 改为：

```rust
    /// 目标主机连接(含跳板保活)。**必须整条持有**:Drop 即断连。
    pub handle: Arc<mullion_ssh::session::SshConnection>,
```

字段名保留 `handle` 不改，避免波及所有引用点（Scope Discipline）。

2. `crates/mullion-app/src/app.rs` 的 `use` 里，若导入了 `russh::client::Handle` / `ClientHandler` 且改后不再使用，删掉；改导入 `SshConnection`。

3. `app.rs` 中 `establish(...)` 的接收变量类型跟着变（多为 `let handle = ...`，无需显式类型则不用改）。

4. 两处 `open_pty(handle.clone(), ...)` / `open_pty(handle, ...)` 传入的已是 `Arc<...>`，类型自动适配。

5. `mullion-app/Cargo.toml` 若为了 `Handle` 类型直接依赖了 `russh`，且改后不再用到，**保留不动**（可能被 known_hosts 等其他处使用；只在编译器报 unused 时才动）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-ssh 2>&1 | tail -5`
Expected: `test result: ok.`

Run: `cargo build -p mullion-app 2>&1 | tail -3`
Expected: `Finished`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-ssh/src crates/mullion-app/src
git commit -m "refactor(ssh): establish 返回 SshConnection,跳板 Handle 由类型强制保活 (F5)"
```

---

## Task 11：`dial()` 逐跳串联

**Files:**
- Create: `crates/mullion-ssh/src/dial.rs`
- Modify: `crates/mullion-ssh/src/session.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

拨号语义（设计 §5.1）：

1. `hops` 为空 → 直连目标，行为与 P0-a 完全一致。
2. 从本机 TCP 连到 `hops[0]` 的地址。
3. 逐跳推进：代理跳做握手，SSH 跳先在当前流上完成 SSH 握手+认证，再 `channel_open_direct_tcpip` 开向下一跳。
4. 最后一跳完成后，流通向目标的 `host:port`，交给 `connect_stream` 做目标的 SSH 握手。

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-ssh/src/dial.rs`：

```rust
//! F4/F5:逐跳拨号(设计 §5.1)。
//!
//! 产出一个「已经通向目标 host:port」的双向流,以及沿途 SSH 跳板的 Handle
//! (交给 `SshConnection` 保活)。目标自身的 SSH 握手不在这里做。

use std::sync::Arc;

use russh::client::{self, Handle};
use tokio::net::TcpStream;

use crate::error::{classify_tcp, ConnectError};
use crate::hop::Hop;
use crate::known_hosts::HostKeyPolicy;
use crate::session::ClientHandler;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthMethod;

    /// 第一跳的地址决定了本机 TCP 连到哪。写错这一步,整条链都是错的。
    #[test]
    fn first_tcp_target_is_first_hop_not_final_destination() {
        let hops = vec![
            Hop::Socks5 {
                host: "127.0.0.1".into(),
                port: 7891,
                auth: None,
            },
            Hop::SshJump {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: AuthMethod::Agent,
            },
        ];
        assert_eq!(first_tcp_target(&hops, "target", 2222), ("127.0.0.1".to_string(), 7891));
    }

    #[test]
    fn empty_hops_dial_the_destination_directly() {
        assert_eq!(first_tcp_target(&[], "target", 2222), ("target".to_string(), 2222));
    }

    /// 每一跳要连的「下一站」是它后面那一跳的地址;最后一跳连目标。
    #[test]
    fn next_stop_after_each_hop_walks_the_chain() {
        let hops = vec![
            Hop::Socks5 {
                host: "p".into(),
                port: 1080,
                auth: None,
            },
            Hop::SshJump {
                host: "b".into(),
                port: 22,
                user: "o".into(),
                auth: AuthMethod::Agent,
            },
        ];
        assert_eq!(next_stop(&hops, 0, "target", 2222), ("b".to_string(), 22));
        assert_eq!(
            next_stop(&hops, 1, "target", 2222),
            ("target".to_string(), 2222)
        );
    }

    #[test]
    fn single_hop_goes_straight_to_destination() {
        let hops = vec![Hop::HttpConnect {
            host: "p".into(),
            port: 8080,
            auth: None,
        }];
        assert_eq!(
            next_stop(&hops, 0, "target", 22),
            ("target".to_string(), 22)
        );
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --lib dial 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find function first_tcp_target`

- [ ] **Step 3: 写选路纯函数**

在 `dial.rs` 的 `use` 之后插入：

```rust
/// 本机第一条 TCP 该连哪：有跳则连第一跳，没跳则直连目标。
pub(crate) fn first_tcp_target(hops: &[Hop], host: &str, port: u16) -> (String, u16) {
    match hops.first() {
        Some(Hop::Socks5 { host, port, .. })
        | Some(Hop::HttpConnect { host, port, .. })
        | Some(Hop::SshJump { host, port, .. }) => (host.clone(), *port),
        None => (host.to_string(), port),
    }
}

/// 第 `idx` 跳完成后，这条流要通向哪：下一跳的地址，或（已是最后一跳时）目标。
pub(crate) fn next_stop(hops: &[Hop], idx: usize, host: &str, port: u16) -> (String, u16) {
    match hops.get(idx + 1) {
        Some(Hop::Socks5 { host, port, .. })
        | Some(Hop::HttpConnect { host, port, .. })
        | Some(Hop::SshJump { host, port, .. }) => (host.clone(), *port),
        None => (host.to_string(), port),
    }
}
```

- [ ] **Step 4: 写执行器**

`dial.rs` 继续追加：

```rust
/// 拨号器产出:通向目标的流 + 沿途 SSH 跳板的 Handle(必须由调用方保活)。
pub struct Dialed {
    pub stream: DialStream,
    pub jumps: Vec<Handle<ClientHandler>>,
}

/// 拨号链的产物流。直连/代理链末端是裸 TCP;经 SSH 跳板则是 channel 流。
pub enum DialStream {
    Tcp(TcpStream),
    Channel(russh::ChannelStream<client::Msg>),
}

/// 按 `hops` 逐跳建立,直到流通向 `host:port`。
///
/// `policy` 用于**跳板自身**的主机密钥校验(F3 对跳板同样生效,设计 §5.3):
/// 跳板被换掉照样是中间人。
pub async fn dial(
    hops: &[Hop],
    host: &str,
    port: u16,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<Dialed, ConnectError> {
    let (first_host, first_port) = first_tcp_target(hops, host, port);
    let addr = resolve_one(&first_host, first_port).await?;
    let tcp = TcpStream::connect(addr).await.map_err(classify_tcp)?;
    tcp.set_nodelay(true)
        .map_err(|e| ConnectError::Io(format!("set_nodelay 失败: {e}")))?;

    let mut stream = DialStream::Tcp(tcp);
    let mut jumps = Vec::new();

    for (idx, hop) in hops.iter().enumerate() {
        let (nh, np) = next_stop(hops, idx, host, port);
        stream = advance(stream, hop, &nh, np, &policy, &mut jumps).await?;
    }

    Ok(Dialed { stream, jumps })
}

async fn resolve_one(host: &str, port: u16) -> Result<std::net::SocketAddr, ConnectError> {
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ConnectError::DnsResolution(e.to_string()))?;
    addrs
        .next()
        .ok_or_else(|| ConnectError::DnsResolution(format!("{host} 无解析结果")))
}

/// 在当前流上跨过一跳,返回通向 `next_host:next_port` 的新流。
async fn advance(
    stream: DialStream,
    hop: &Hop,
    next_host: &str,
    next_port: u16,
    policy: &Arc<dyn HostKeyPolicy>,
    jumps: &mut Vec<Handle<ClientHandler>>,
) -> Result<DialStream, ConnectError> {
    match hop {
        Hop::Socks5 { auth, .. } => {
            let label = hop.endpoint();
            let pair = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
            match stream {
                DialStream::Tcp(mut s) => {
                    crate::proxy::socks5_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Tcp(s))
                }
                DialStream::Channel(mut s) => {
                    crate::proxy::socks5_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Channel(s))
                }
            }
        }
        Hop::HttpConnect { auth, .. } => {
            let label = hop.endpoint();
            let pair = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
            match stream {
                DialStream::Tcp(mut s) => {
                    crate::proxy::http_connect_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Tcp(s))
                }
                DialStream::Channel(mut s) => {
                    crate::proxy::http_connect_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Channel(s))
                }
            }
        }
        Hop::SshJump {
            host, user, auth, ..
        } => {
            let label = hop.endpoint();
            let fail = |cause: String| ConnectError::JumpFailed {
                hop: label.clone(),
                cause,
            };
            // 跳板自己的 SSH 握手 + 认证。主机密钥同样过 policy(F3)。
            let handle = match stream {
                DialStream::Tcp(s) => {
                    crate::session::handshake_and_auth(s, host, user, auth, policy.clone())
                        .await
                        .map_err(|e| fail(e.to_string()))?
                }
                DialStream::Channel(s) => {
                    crate::session::handshake_and_auth(s, host, user, auth, policy.clone())
                        .await
                        .map_err(|e| fail(e.to_string()))?
                }
            };
            // 在跳板上开一条通向下一站的转发通道。
            // originator 字段填本地占位:sshd 只记日志,不参与路由。
            let channel = handle
                .channel_open_direct_tcpip(next_host, next_port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| fail(e.to_string()))?;
            jumps.push(handle);
            Ok(DialStream::Channel(channel.into_stream()))
        }
    }
}

impl tokio::io::AsyncRead for DialStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for DialStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}
```

`advance` 里用了 `crate::session::handshake_and_auth`——把 `session.rs` 里 `establish` 的第 3、4 步抽成一个泛型函数（**抽取，不是新写一套**，避免两条认证路径漂移）：

```rust
/// 在已建立的流上完成 SSH 握手 + 认证。目标主机与跳板共用此函数,
/// 避免两条认证路径漂移(例如只在其中一条修了 PUBKEY_HASH)。
pub(crate) async fn handshake_and_auth<S>(
    stream: S,
    host: &str,
    user: &str,
    auth: &AuthMethod,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<Handle<ClientHandler>, ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let outcome = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        host: host.to_string(),
        policy,
        outcome: outcome.clone(),
    };
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| host_key_or(&outcome, e))?;
    match authenticate_with(&mut handle, user, auth).await? {
        AuthResult::Success => Ok(handle),
        AuthResult::Failure { .. } => Err(ConnectError::AuthFailed),
    }
}
```

并把现有 `authenticate(handle, cfg)` 改写成薄壳，真正逻辑挪进 `authenticate_with(handle, user, auth)`：

```rust
async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
) -> Result<AuthResult, ConnectError> {
    authenticate_with(handle, &cfg.user, &cfg.auth).await
}
```

`authenticate_with` 的函数体 = 原 `authenticate` 的函数体，把 `cfg.user` 换成 `user`、`&cfg.auth` 换成 `auth`，其余（含 `PUBKEY_HASH` 的使用）一字不动。

- [ ] **Step 5: `establish` 走 `dial`**

`session.rs` 的 `establish` 改成：

```rust
pub async fn establish(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<SshConnection, ConnectError> {
    let dialed = crate::dial::dial(&cfg.hops, &cfg.host, cfg.port, policy.clone()).await?;
    let handle =
        handshake_and_auth(dialed.stream, &cfg.host, &cfg.user, &cfg.auth, policy).await?;
    Ok(SshConnection::new(handle, dialed.jumps))
}
```

原来 `establish` 里的 DNS / TCP / set_nodelay 三步已迁进 `dial::dial`（`hops` 为空时行为与原来完全一致：直接解析目标并连），删掉 `establish` 里的对应代码，**保留那条 set_nodelay 的红线注释**（迁到 `dial.rs` 的同一行上）。

- [ ] **Step 6: 挂模块并跑测试**

`crates/mullion-ssh/src/lib.rs` 加：

```rust
pub mod dial;
```

Run: `cargo test -p mullion-ssh 2>&1 | tail -10`
Expected: `test result: ok.`

若 `channel_open_direct_tcpip` 的实际签名与上面不符（russh 版本漂移），**先看编译器给出的签名再改**，不要猜：

```bash
grep -rn "fn channel_open_direct_tcpip" ~/.cargo/registry/src/*/russh-0.54*/src/client/mod.rs
```

- [ ] **Step 7: clippy + 提交**

```bash
cargo clippy -p mullion-ssh --all-targets -- -D warnings
git add crates/mullion-ssh/src
git commit -m "feat(ssh): dial 逐跳串联代理与 SSH 跳板,目标与跳板共用认证路径 (F4/F5)"
```

---

# 阶段三：app 层（物化 + UI）

## Task 12：`dial_plan.rs` 物化纯函数

这是红线 2 的**枢纽**：store 类型止于此，`Hop` 由此产生。写成纯函数，可脱离 GUI 与网络单测。

**Files:**
- Create: `crates/mullion-app/src/shell/dial_plan.rs`
- Modify: `crates/mullion-app/src/shell/mod.rs`

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/shell/dial_plan.rs`：

```rust
//! 把 store 的「拨号声明」物化成 ssh 的 `Hop` 列表(设计 §4)。
//!
//! **红线 2 的枢纽**:store 类型只出现在入参,`Hop` 只出现在出参。
//! `mullion-ssh` 因此永远不需要认识「会话」「分组」。纯函数,零 IO。

use mullion_ssh::config::AuthMethod;
use mullion_ssh::hop::Hop;
use mullion_store::{AuthKind, ProxyChoice, SecretEntry, SessionRecord};

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        Auth, Connection, Identity, NetworkPrefs, Protocol, ProxyEndpoint, SessionId,
    };

    fn rec(id: u64, host: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: host.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "ops".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
        }
    }

    fn pw(p: &str) -> SecretEntry {
        SecretEntry {
            password: Some(p.into()),
            passphrase: None,
            proxy_password: None,
        }
    }

    #[test]
    fn direct_session_produces_no_hops() {
        let hops = build_hops(None, &[], &|_| None);
        assert!(hops.is_empty(), "无代理无跳板应产出空链");
    }

    /// 代理排在所有跳板之前:先出本机网络,再谈跳板。
    #[test]
    fn proxy_comes_before_every_jump() {
        let proxy = ProxyChoice::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port: 7891,
            user: None,
        });
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(Some(&proxy), &jumps, &|_| Some(pw("bp")));
        assert_eq!(hops.len(), 2);
        assert!(matches!(hops[0], Hop::Socks5 { .. }), "代理必须在最前");
        assert!(matches!(hops[1], Hop::SshJump { .. }));
    }

    /// `Direct` 是显式直连,不该物化出任何代理跳。
    #[test]
    fn explicit_direct_produces_no_proxy_hop() {
        let hops = build_hops(Some(&ProxyChoice::Direct), &[], &|_| None);
        assert!(hops.is_empty(), "Direct 不是一跳,不该出现在链上");
    }

    #[test]
    fn jump_order_is_preserved_as_dial_order() {
        let jumps = vec![rec(2, "b1"), rec(3, "b2")];
        let hops = build_hops(None, &jumps, &|_| Some(pw("x")));
        match (&hops[0], &hops[1]) {
            (Hop::SshJump { host: a, .. }, Hop::SshJump { host: b, .. }) => {
                assert_eq!((a.as_str(), b.as_str()), ("b1", "b2"));
            }
            other => panic!("应为两个 SshJump,实际: {other:?}"),
        }
    }

    /// 跳板的凭据取自**跳板自己那条会话**的 secret,不是目标会话的。
    #[test]
    fn jump_credentials_come_from_the_jump_session_not_the_target() {
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(None, &jumps, &|id| {
            assert_eq!(id, SessionId(2), "应查跳板会话的 secret");
            Some(pw("bastion-pw"))
        });
        match &hops[0] {
            Hop::SshJump { auth, .. } => {
                assert!(matches!(auth, AuthMethod::Password(p) if p == "bastion-pw"));
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 跳板会话没存密码 → 退回 agent,而不是拿空串去认证(那必然 AuthFailed 且信息误导)。
    #[test]
    fn jump_without_stored_password_falls_back_to_agent() {
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(None, &jumps, &|_| None);
        match &hops[0] {
            Hop::SshJump { auth, .. } => assert!(matches!(auth, AuthMethod::Agent)),
            other => panic!("实际: {other:?}"),
        }
    }

    #[test]
    fn socks5_proxy_credentials_are_materialized() {
        let proxy = ProxyChoice::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port: 7891,
            user: Some("alice".into()),
        });
        let secret = SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some("ppw".into()),
        };
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], &|_| None, Some(&secret));
        match &hops[0] {
            Hop::Socks5 { auth, .. } => {
                assert_eq!(
                    auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                    Some(("alice", "ppw"))
                );
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 代理配了用户名但没存口令 → 按免认证发起(空口令几乎必被拒,还会把
    /// 「没配口令」误报成「口令错」)。
    #[test]
    fn proxy_user_without_password_degrades_to_anonymous() {
        let proxy = ProxyChoice::HttpConnect(ProxyEndpoint {
            host: "p".into(),
            port: 8080,
            user: Some("alice".into()),
        });
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], &|_| None, None);
        match &hops[0] {
            Hop::HttpConnect { auth, .. } => assert!(auth.is_none()),
            other => panic!("实际: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib dial_plan 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find function build_hops`

- [ ] **Step 3: 写实现**

在 `dial_plan.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入：

```rust
/// 物化拨号链。`secret_of` 用于查**每一跳自己那条会话**的凭据。
///
/// `proxy_secret` 是目标会话的 secret(代理口令只存会话级,见设计 §3.3)。
pub fn build_hops_with_proxy_secret(
    proxy: Option<&ProxyChoice>,
    jumps: &[SessionRecord],
    secret_of: &dyn Fn(mullion_store::SessionId) -> Option<SecretEntry>,
    proxy_secret: Option<&SecretEntry>,
) -> Vec<Hop> {
    let mut hops = Vec::new();

    // 代理排在最前:先出本机网络,才谈得上连跳板。
    if let Some(choice) = proxy {
        let pw = proxy_secret.and_then(|s| s.proxy_password.clone());
        match choice {
            // Direct 是「显式不走代理」,不是一跳。
            ProxyChoice::Direct => {}
            ProxyChoice::Socks5(ep) => hops.push(Hop::Socks5 {
                host: ep.host.clone(),
                port: ep.port,
                auth: pair(ep.user.as_deref(), pw),
            }),
            ProxyChoice::HttpConnect(ep) => hops.push(Hop::HttpConnect {
                host: ep.host.clone(),
                port: ep.port,
                auth: pair(ep.user.as_deref(), pw),
            }),
        }
    }

    for rec in jumps {
        hops.push(Hop::SshJump {
            host: rec.connection.host.clone(),
            port: rec.connection.port,
            user: rec.auth.user.clone(),
            auth: jump_auth(rec, secret_of(rec.id)),
        });
    }

    hops
}

/// 无代理口令的便捷入口(测试与「目标会话无 secret」时用)。
pub fn build_hops(
    proxy: Option<&ProxyChoice>,
    jumps: &[SessionRecord],
    secret_of: &dyn Fn(mullion_store::SessionId) -> Option<SecretEntry>,
) -> Vec<Hop> {
    build_hops_with_proxy_secret(proxy, jumps, secret_of, None)
}

/// 用户名与口令**必须成对**才发认证:只有用户名就拿空口令去谈,几乎必被拒,
/// 且会把「没配口令」误报成「口令错」。
fn pair(user: Option<&str>, pw: Option<String>) -> Option<(String, String)> {
    match (user, pw) {
        (Some(u), Some(p)) => Some((u.to_string(), p)),
        _ => None,
    }
}

/// 跳板的认证方式。凭据取自**跳板自己那条会话**。
fn jump_auth(rec: &SessionRecord, secret: Option<SecretEntry>) -> AuthMethod {
    match &rec.auth.kind {
        AuthKind::Password => match secret.and_then(|s| s.password) {
            Some(p) => AuthMethod::Password(p),
            // 没存密码就退回 agent:拿空串去认证只会得到一条误导性的 AuthFailed。
            None => AuthMethod::Agent,
        },
        AuthKind::PublicKey { path, .. } => AuthMethod::PublicKey {
            path: path.clone(),
            passphrase: secret.and_then(|s| s.passphrase),
        },
    }
}
```

- [ ] **Step 4: 挂模块**

`crates/mullion-app/src/shell/mod.rs` 加：

```rust
pub mod dial_plan;
```

若 `mullion_store` 的 lib 未 re-export `ProxyChoice` / `NetworkPrefs` / `ProxyEndpoint`（Task 1 已加），补上再编译。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib dial_plan 2>&1 | tail -15`
Expected: `test result: ok. 8 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/shell
git commit -m "feat(app): dial_plan 把会话声明物化成 Hop,凭据取自各跳自身会话 (F4/F5)"
```

---

## Task 13：`SessionStore` 暴露分组与拨号链

**Files:**
- Modify: `crates/mullion-app/src/shell/store.rs`
- Modify: `crates/mullion-app/src/shell/session_map.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/shell/store.rs` 的 `mod tests` 末尾追加。先把既有 `fn draft()` 里的 `SecretEntry` 补上新字段（Task 3 已加字段，此处同步）：

```rust
            secret: Some(SecretEntry {
                password: Some("pw".into()),
                passphrase: None,
                proxy_password: None,
            }),
```

并在 `SessionDraft` 字面量里补 `network: Default::default(),`。然后追加测试：

```rust
    #[test]
    fn group_crud_is_reachable_from_app_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let gid = store.add_group("生产".into());
        assert_eq!(store.groups().len(), 1);
        assert_eq!(store.groups()[0].name, "生产");
        store.delete_group(gid).unwrap();
        assert!(store.groups().is_empty());
    }

    /// 会话经分组继承来的代理,必须一路落到 `SshConfig.hops`。
    #[test]
    fn ssh_config_carries_hops_from_inherited_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let gid = store.add_group("生产".into());
        store.set_group_proxy(
            gid,
            Some(mullion_store::ProxyChoice::Socks5(
                mullion_store::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                },
            )),
        );
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = store.add(d, "2026-07-31T00:00:00Z");

        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(cfg.hops.len(), 1, "继承来的代理应成为一跳");
        assert!(matches!(cfg.hops[0], mullion_ssh::hop::Hop::Socks5 { .. }));
    }

    /// 跳板悬空必须硬失败,不许静默直连(安全属性,设计 §6)。
    #[test]
    fn dangling_jump_makes_connect_fail_instead_of_going_direct() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let mut d = draft();
        d.network = mullion_store::NetworkPrefs {
            proxy: None,
            jump: Some(vec![mullion_store::JumpRef(mullion_store::SessionId(999))]),
        };
        let id = store.add(d, "2026-07-31T00:00:00Z");
        assert!(
            store.ssh_config_for(id).is_err(),
            "悬空跳板必须报错,绝不能悄悄直连"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib store 2>&1 | grep -E "^error" | head -10`
Expected: `no method named add_group found for struct SessionStore`

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/shell/store.rs`：`impl SessionStore` 里追加：

```rust
    pub fn groups(&self) -> &[mullion_store::GroupRecord] {
        self.vault.groups()
    }

    pub fn add_group(&mut self, name: String) -> mullion_store::GroupId {
        self.vault.add_group(name)
    }

    pub fn rename_group(&mut self, id: mullion_store::GroupId, name: String) -> bool {
        match self.vault.group_mut(id) {
            Some(g) => {
                g.name = name;
                true
            }
            None => false,
        }
    }

    /// 设置分组代理(F4)。分组只持有可继承字段,代理正是其中之一。
    pub fn set_group_proxy(
        &mut self,
        id: mullion_store::GroupId,
        proxy: Option<mullion_store::ProxyChoice>,
    ) {
        if let Some(g) = self.vault.group_mut(id) {
            g.network.proxy = proxy;
        }
    }

    pub fn delete_group(&mut self, id: mullion_store::GroupId) -> Result<(), StoreError> {
        self.vault.delete_group(id)
    }

    /// 解析后的配置(含继承来的代理/跳板)。
    pub fn resolved(
        &self,
        id: SessionId,
    ) -> Result<mullion_store::ResolvedConfig, StoreError> {
        self.vault.resolve_for(id)
    }
```

把 `ssh_config_for` 改成带上 hops：

```rust
    /// 取会话 → 用其(已解密的)secret 组 SshConfig(双击连接用)。
    ///
    /// 拨号链在这里物化:代理来自继承解析,跳板来自引用图展开。
    /// 跳板悬空/成环会在此**硬失败**——静默直连会让用户以为流量过了堡垒机(设计 §6)。
    pub fn ssh_config_for(&self, id: SessionId) -> Result<SshConfig, StoreOpenError> {
        let rec = self.vault.get(id).ok_or(StoreOpenError::NotFound(id))?;
        let secret = self.vault.secret(id);
        let mut cfg = to_ssh_config(rec, secret)?;

        let resolved = self.vault.resolve_for(id)?;
        let jumps = self.vault.expand_jump_chain(id)?;
        cfg.hops = super::dial_plan::build_hops_with_proxy_secret(
            resolved.proxy.as_ref(),
            &jumps,
            &|jid| self.vault.secret(jid).cloned(),
            secret,
        );
        Ok(cfg)
    }
```

`crates/mullion-app/src/shell/session_map.rs`：`to_ssh_config` 构造 `SshConfig` 时补一行（拨号链由上面的 `ssh_config_for` 填，这里给空）：

```rust
        hops: Vec::new(),
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib store 2>&1 | tail -15`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell
git commit -m "feat(app): SessionStore 暴露分组 CRUD,ssh_config_for 物化拨号链 (F4/F5/F60)"
```

---

## Task 14：编辑表单的 network 字段

**注意 P0-a 留下的坑**：`Vault::update` 对五个分节是**整体替换**而非合并。`network` 是第六个分节，同样必须由表单显式带回，否则「编辑任意会话 → 静默清空代理/跳板设置」。守护测试 `editing_a_session_preserves_fields_the_form_cannot_edit` 必须扩展到覆盖它。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager.rs`

- [ ] **Step 1: 写失败测试**

在 `session_manager.rs` 的 `mod tests` 里，把既有的 `editing_a_session_preserves_fields_the_form_cannot_edit` **保留不动**，另外追加：

```rust
    /// 表单能编代理与跳板了,它们必须真的往返一次而不被吃掉。
    #[test]
    fn editor_round_trips_proxy_and_jump_chain() {
        let mut rec = record_for_test();
        rec.network = mullion_store::NetworkPrefs {
            proxy: Some(mullion_store::ProxyChoice::Socks5(
                mullion_store::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: Some("alice".into()),
                },
            )),
            jump: Some(vec![mullion_store::JumpRef(mullion_store::SessionId(2))]),
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network, rec.network, "代理与跳板必须原样往返");
    }

    /// 分组代理下,会话选「不使用代理」必须落成显式 `Direct` 而非 `None`——
    /// 落成 `None` 会继续继承分组代理,与用户所选相反。
    #[test]
    fn choosing_no_proxy_writes_explicit_direct_not_inherit() {
        let mut buf = EditorBuffer::default();
        buf.port = "22".into();
        buf.proxy_mode = ProxyModeUi::Direct;
        let draft = build_draft(&buf).unwrap();
        assert_eq!(
            draft.network.proxy,
            Some(mullion_store::ProxyChoice::Direct),
            "「不使用代理」是覆盖,不是不设置"
        );
    }

    #[test]
    fn choosing_inherit_leaves_proxy_unset() {
        let mut buf = EditorBuffer::default();
        buf.port = "22".into();
        buf.proxy_mode = ProxyModeUi::Inherit;
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network.proxy, None, "「跟随分组」= 不设置");
    }

    #[test]
    fn proxy_port_must_be_a_valid_number() {
        let mut buf = EditorBuffer::default();
        buf.port = "22".into();
        buf.proxy_mode = ProxyModeUi::Socks5;
        buf.proxy_host = "127.0.0.1".into();
        buf.proxy_port = "abc".into();
        let err = build_draft(&buf).unwrap_err();
        assert!(err.contains("代理端口"), "错误消息应点名是代理端口: {err}");
    }
```

`record_for_test()` 是既有守护测试里构造 `SessionRecord` 的方式；若该文件用的是内联字面量，就照抄它并补 `network` 字段，**不要**新造一套构造器。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find type ProxyModeUi` / `no field proxy_mode`

- [ ] **Step 3: 写实现**

`session_manager.rs`：加 UI 枚举（放在 `AuthKindUi` 之后）：

```rust
/// 编辑表单里的代理选择。**四态**,不是三态:
/// 「跟随分组」与「不使用代理」必须分开,前者是不设置(继承),
/// 后者是显式 `Direct`(覆盖分组)。合并二者会让用户无法在有分组代理时单独直连。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyModeUi {
    Inherit,
    Direct,
    Socks5,
    HttpConnect,
}
```

`EditorBuffer` 加字段（放在 `passphrase` 之后、透传字段之前）：

```rust
    pub proxy_mode: ProxyModeUi,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_password: String,
    /// 跳板链,按拨号顺序。UI 用下拉逐个添加/删除。
    pub jump_chain: Vec<SessionId>,
    /// 跳板链是否被用户显式设过。`false` = 沿用继承(写回 `None`)。
    pub jump_set: bool,
```

`Default` 补：

```rust
            proxy_mode: ProxyModeUi::Inherit,
            proxy_host: String::new(),
            proxy_port: "1080".to_string(),
            proxy_user: String::new(),
            proxy_password: String::new(),
            jump_chain: Vec::new(),
            jump_set: false,
```

`from_record` 在 `match &rec.auth.kind` 之前插入：

```rust
        match &rec.network.proxy {
            None => buf.proxy_mode = ProxyModeUi::Inherit,
            Some(mullion_store::ProxyChoice::Direct) => buf.proxy_mode = ProxyModeUi::Direct,
            Some(mullion_store::ProxyChoice::Socks5(ep)) => {
                buf.proxy_mode = ProxyModeUi::Socks5;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
            Some(mullion_store::ProxyChoice::HttpConnect(ep)) => {
                buf.proxy_mode = ProxyModeUi::HttpConnect;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
        }
        if let Some(chain) = &rec.network.jump {
            buf.jump_set = true;
            buf.jump_chain = chain.iter().map(|j| j.0).collect();
        }
```

（代理口令不回填：store 不明文回吐凭据，与密码框同一约定。留空 = 不改，UI 上给提示。）

`build_draft` 在 `Ok(SessionDraft {` 之前插入：

```rust
    let proxy = match buf.proxy_mode {
        ProxyModeUi::Inherit => None,
        ProxyModeUi::Direct => Some(mullion_store::ProxyChoice::Direct),
        ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect => {
            let pport: u16 = buf
                .proxy_port
                .trim()
                .parse()
                .map_err(|_| "代理端口非法,须为 1-65535 的整数".to_string())?;
            let ep = mullion_store::ProxyEndpoint {
                host: buf.proxy_host.trim().to_string(),
                port: pport,
                user: if buf.proxy_user.trim().is_empty() {
                    None
                } else {
                    Some(buf.proxy_user.trim().to_string())
                },
            };
            Some(if buf.proxy_mode == ProxyModeUi::Socks5 {
                mullion_store::ProxyChoice::Socks5(ep)
            } else {
                mullion_store::ProxyChoice::HttpConnect(ep)
            })
        }
    };
    let jump = if buf.jump_set {
        Some(buf.jump_chain.iter().map(|id| mullion_store::JumpRef(*id)).collect())
    } else {
        None
    };
```

`SessionDraft` 字面量补一项（放在 `appearance` 之后）：

```rust
        network: mullion_store::NetworkPrefs { proxy, jump },
```

代理口令并入 secret：`build_draft` 里两处构造 `SecretEntry` 都要带上（并处理「只配了代理口令、SSH 用 agent」的情形）。把 `let (auth, secret) = match buf.auth_kind { ... };` 之后紧接着插入：

```rust
    // 代理口令与 SSH 凭据存在同一个 SecretEntry 里。即使 SSH 侧没有任何凭据,
    // 只要配了代理口令就得建一个 entry,否则口令无处存。
    let proxy_password = if buf.proxy_password.is_empty() {
        None
    } else {
        Some(buf.proxy_password.clone())
    };
    let secret = match (secret, proxy_password) {
        (Some(mut s), pp) => {
            s.proxy_password = pp;
            Some(s)
        }
        (None, Some(pp)) => Some(SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some(pp),
        }),
        (None, None) => None,
    };
```

同时把上面两处 `SecretEntry { password: ..., passphrase: ... }` 字面量各补 `proxy_password: None,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | tail -15`
Expected: `test result: ok.`，含既有的 `editing_a_session_preserves_fields_the_form_cannot_edit`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager.rs
git commit -m "feat(app): 编辑表单支持代理与跳板,「不使用代理」落显式 Direct (F4/F5)"
```

---

## Task 15：编辑器 UI —— 代理与跳板控件

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager.rs`（`show_editor`）

UI 无法自动验证（CLAUDE.md「你无法验证的东西」）。本任务只保证**编译通过 + 逻辑测试已在 Task 14 覆盖**，视觉与手感进人工验收清单。

- [ ] **Step 1: 加代理控件**

在 `show_editor` 的表单 Grid 里，认证相关行之后追加（`egui::Grid` 的每一行是「label + 控件 + `ui.end_row()`」，照抄该文件既有行的写法）：

```rust
            ui.label("代理");
            egui::ComboBox::from_id_salt("editor_proxy_mode")
                .selected_text(match buf.proxy_mode {
                    ProxyModeUi::Inherit => "跟随分组",
                    ProxyModeUi::Direct => "不使用代理",
                    ProxyModeUi::Socks5 => "SOCKS5",
                    ProxyModeUi::HttpConnect => "HTTP CONNECT",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "跟随分组");
                    ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "不使用代理");
                    ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
                    ui.selectable_value(
                        &mut buf.proxy_mode,
                        ProxyModeUi::HttpConnect,
                        "HTTP CONNECT",
                    );
                });
            ui.end_row();

            if matches!(buf.proxy_mode, ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect) {
                ui.label("代理地址");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut buf.proxy_host).desired_width(160.0));
                    ui.label(":");
                    ui.add(egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(60.0));
                });
                ui.end_row();

                ui.label("代理用户名");
                ui.text_edit_singleline(&mut buf.proxy_user);
                ui.end_row();

                ui.label("代理口令");
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_password).password(true));
                ui.end_row();
            }
```

`ComboBox::from_id_salt` 是 egui 0.30 的写法（0.28 之前叫 `from_id_source`）。照抄该文件既有 ComboBox 那几行的 API，不要混用。

- [ ] **Step 2: 加跳板链控件**

紧接其后：

```rust
            ui.label("跳板");
            ui.vertical(|ui| {
                if !buf.jump_set {
                    ui.horizontal(|ui| {
                        ui.label("跟随分组");
                        if ui.button("改为自定义").clicked() {
                            buf.jump_set = true;
                        }
                    });
                } else {
                    let mut remove_at = None;
                    for (i, id) in buf.jump_chain.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let name = sessions
                                .iter()
                                .find(|r| r.id == *id)
                                .map(|r| r.identity.name.clone())
                                // 悬空引用在 UI 上就点出来,不要等到连接时才报错。
                                .unwrap_or_else(|| format!("<已删除的会话 {:?}>", id));
                            ui.label(format!("{}. {name}", i + 1));
                            if ui.button("移除").clicked() {
                                remove_at = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove_at {
                        buf.jump_chain.remove(i);
                    }
                    egui::ComboBox::from_id_salt("editor_jump_add")
                        .selected_text("添加跳板…")
                        .show_ui(ui, |ui| {
                            for rec in sessions {
                                // 不能把自己当自己的跳板(那是环)。
                                if Some(rec.id) == editing_id {
                                    continue;
                                }
                                if ui.button(&rec.identity.name).clicked() {
                                    buf.jump_chain.push(rec.id);
                                }
                            }
                        });
                    if ui.button("恢复为跟随分组").clicked() {
                        buf.jump_set = false;
                        buf.jump_chain.clear();
                    }
                }
            });
            ui.end_row();
```

这需要 `show_editor` 能拿到会话列表与当前编辑的 id。若现有签名是 `show_editor(ctx, ui_state)`，改成：

```rust
pub fn show_editor(ctx: &egui::Context, ui_state: &mut UiState, sessions: &[SessionRecord])
```

`editing_id` 从 `ui_state` 里既有的编辑态字段取（`show` 里判断新建/编辑用的就是它）；调用点在 `ui/mod.rs`，把 `sessions` 一并传进去。

- [ ] **Step 3: 编译并跑测试**

Run: `cargo test -p mullion-app 2>&1 | tail -10`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/ui
git commit -m "feat(app): 会话编辑器加代理与跳板链控件,悬空跳板在表单内即标红 (F4/F5)"
```

---

## Task 16：分组 UI

P0-a 把分组数据结构做完了但**没有任何 UI 入口**，用户看不到它。本任务给最小可用入口：列表按分组折叠 + 一个极简管理弹窗。

**Files:**
- Create: `crates/mullion-app/src/ui/group_manager.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/ui/session_manager.rs`（列表分组 + 编辑器的分组下拉）

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/group_manager.rs`，先写分组逻辑的纯函数与测试：

```rust
//! F60:极简分组管理弹窗(新建 / 改名 / 删除)+ 会话列表的分组归集。
//!
//! 与 `session_manager` 同构:UI 只写「意图」到 `UiState`,由 `app.rs` 在借用释放后施加。

use mullion_store::{GroupId, GroupRecord, SessionRecord};

/// 一次分组操作的意图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupIntent {
    Add(String),
    Rename(GroupId, String),
    Delete(GroupId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        Auth, AuthKind, Connection, Identity, NetworkPrefs, Protocol, SessionId,
    };

    fn rec(id: u64, name: &str, group: Option<u64>) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: group.map(GroupId),
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
        }
    }

    fn grp(id: u64, name: &str) -> GroupRecord {
        GroupRecord {
            id: GroupId(id),
            name: name.into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
        }
    }

    #[test]
    fn sessions_are_bucketed_by_group_in_group_order() {
        let groups = vec![grp(1, "生产"), grp(2, "测试")];
        let sessions = vec![rec(10, "a", Some(2)), rec(11, "b", Some(1))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 2, "只该有两个非空桶");
        assert_eq!(got[0].0, Some(GroupId(1)), "桶序跟随分组顺序");
        assert_eq!(got[0].1[0].identity.name, "b");
        assert_eq!(got[1].0, Some(GroupId(2)));
    }

    /// 未分组的会话必须仍然可见,且排在最后——否则用户会以为会话丢了。
    #[test]
    fn ungrouped_sessions_go_to_a_trailing_bucket() {
        let groups = vec![grp(1, "生产")];
        let sessions = vec![rec(10, "a", None), rec(11, "b", Some(1))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].0, None, "未分组桶排最后");
        assert_eq!(got[1].1[0].identity.name, "a");
    }

    /// 悬空 group_id(分组被删)不能让会话消失。P0-a 的 `resolve_for` 对此静默降级,
    /// 列表也必须跟着降级到「未分组」而不是漏掉这一条。
    #[test]
    fn session_with_dangling_group_id_falls_into_ungrouped_not_dropped() {
        let groups = vec![grp(1, "生产")];
        let sessions = vec![rec(10, "orphan", Some(99))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, None);
        assert_eq!(got[0].1[0].identity.name, "orphan");
    }

    #[test]
    fn empty_groups_produce_no_buckets() {
        let groups = vec![grp(1, "空组")];
        let got = group_sessions(&groups, &[]);
        assert!(got.is_empty(), "没有会话就不该渲染任何桶");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib group_manager 2>&1 | grep -E "^error" | head -5`
Expected: `cannot find function group_sessions`

- [ ] **Step 3: 写归集函数**

在 `group_manager.rs` 的 `GroupIntent` 之后插入：

```rust
/// 把会话按分组归集。返回 `(分组, 该组会话)`,分组顺序跟随 `groups`,
/// 未分组(含 group_id 悬空)的会话归入末尾的 `None` 桶。空桶不返回。
pub fn group_sessions<'a>(
    groups: &[GroupRecord],
    sessions: &'a [SessionRecord],
) -> Vec<(Option<GroupId>, Vec<&'a SessionRecord>)> {
    let mut out: Vec<(Option<GroupId>, Vec<&SessionRecord>)> = Vec::new();
    for g in groups {
        let bucket: Vec<&SessionRecord> = sessions
            .iter()
            .filter(|s| s.identity.group_id == Some(g.id))
            .collect();
        if !bucket.is_empty() {
            out.push((Some(g.id), bucket));
        }
    }
    // 悬空 group_id 也落这里:分组被删后会话不能从列表里消失。
    let known: Vec<GroupId> = groups.iter().map(|g| g.id).collect();
    let orphans: Vec<&SessionRecord> = sessions
        .iter()
        .filter(|s| match s.identity.group_id {
            None => true,
            Some(g) => !known.contains(&g),
        })
        .collect();
    if !orphans.is_empty() {
        out.push((None, orphans));
    }
    out
}
```

- [ ] **Step 4: 写弹窗**

`group_manager.rs` 继续追加：

```rust
/// 分组管理弹窗。只写意图,不碰 store。
pub fn show(ctx: &egui::Context, ui_state: &mut crate::ui::UiState, groups: &[GroupRecord]) {
    let mut open = ui_state.group_manager_open;
    egui::Window::new("分组管理")
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("新建分组");
                ui.text_edit_singleline(&mut ui_state.group_name_buf);
                let name = ui_state.group_name_buf.trim().to_string();
                if ui.add_enabled(!name.is_empty(), egui::Button::new("添加")).clicked() {
                    ui_state.group_intent = Some(GroupIntent::Add(name));
                    ui_state.group_name_buf.clear();
                }
            });
            ui.separator();
            for g in groups {
                ui.horizontal(|ui| {
                    ui.label(&g.name);
                    if ui.button("删除").clicked() {
                        ui_state.group_intent = Some(GroupIntent::Delete(g.id));
                    }
                });
            }
            if groups.is_empty() {
                ui.label("还没有分组。分组用来给一批会话共享代理、跳板与终端偏好。");
            }
        });
    ui_state.group_manager_open = open;
}
```

`GroupIntent::Rename` 本切片先不接 UI（`SessionStore::rename_group` 已就位，留给需要时接）。**不要**因为「暂时用不到」就删掉它——Task 13 的实现与本枚举已经配对。

- [ ] **Step 5: 接线 `UiState` 与 `app.rs`**

`crates/mullion-app/src/ui/mod.rs`：`UiState` 加三个字段：

```rust
    pub group_manager_open: bool,
    pub group_name_buf: String,
    pub group_intent: Option<crate::ui::group_manager::GroupIntent>,
```

（若 `UiState` 有手写 `Default`，同步补 `false` / `String::new()` / `None`。）

同文件加模块声明与渲染调用：

```rust
pub mod group_manager;
```

在 `build_ui` 里既有 `session_manager::show(...)` 调用之后加：

```rust
    group_manager::show(ctx, ui_state, groups);
```

`build_ui` / `UiFrame` 因此要多收一个 `groups: &[GroupRecord]`，调用链上游（`app.rs` 的 `render_frame`）从 `store.groups()` 取；store 不可用时传 `&[]`。

`app.rs` 在 `render_frame` 返回、借用释放之后施加意图（与既有 `SaveIntent` 处理同构）：

```rust
    if let Some(intent) = ui_state.group_intent.take() {
        if let Some(store) = self.store.as_mut() {
            match intent {
                GroupIntent::Add(name) => {
                    store.add_group(name);
                }
                GroupIntent::Rename(id, name) => {
                    store.rename_group(id, name);
                }
                GroupIntent::Delete(id) => {
                    if let Err(e) = store.delete_group(id) {
                        ui_state.last_error = Some(e.to_string());
                    }
                }
            }
            if let Err(e) = store.save() {
                ui_state.last_error = Some(e.to_string());
            }
        }
    }
```

`self.store` 的实际字段名以 `app.rs` 现有代码为准（既有 `SaveIntent` 处理里用的就是它），`ui_state.last_error` 同理。

- [ ] **Step 6: 会话列表按分组折叠**

`session_manager.rs` 的 `show`：把平铺的 `egui::Grid::new("session_list_grid")` 包进按桶的 `CollapsingHeader`。`show` 的签名多收 `groups: &[GroupRecord]`，主体改成：

```rust
            for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
                let title = match gid {
                    Some(id) => groups
                        .iter()
                        .find(|g| g.id == id)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "未分组".to_string()),
                    None => "未分组".to_string(),
                };
                egui::CollapsingHeader::new(format!("{title}({})", bucket.len()))
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::Grid::new(format!("session_list_grid_{gid:?}"))
                            .num_columns(5)
                            .striped(true)
                            .show(ui, |ui| {
                                for rec in &bucket {
                                    // ↓ 原来 for 循环体里的每行渲染代码原样搬进来
                                }
                            });
                    });
            }
```

Grid 的 id 必须按桶区分（`session_list_grid_{gid:?}`），否则多个 Grid 撞 id，egui 会把它们的布局叠在一起。

编辑器的分组下拉：`show_editor` 的表单里加一行，把 `preserved_group_id` 变成可编辑：

```rust
            ui.label("分组");
            egui::ComboBox::from_id_salt("editor_group")
                .selected_text(
                    buf.preserved_group_id
                        .and_then(|id| groups.iter().find(|g| g.id == id))
                        .map(|g| g.name.as_str())
                        .unwrap_or("未分组"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                    for g in groups {
                        ui.selectable_value(&mut buf.preserved_group_id, Some(g.id), &g.name);
                    }
                });
            ui.end_row();
```

字段仍叫 `preserved_group_id`（它现在真的可编辑了，但改名会波及守护测试与多处引用，超出本切片范围）。在其 doc 注释末尾补一句：

```rust
    // 注:`preserved_group_id` 自 P0-b 起可由编辑器下拉修改,名字沿用未改以免波及守护测试。
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | tail -10`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/ui crates/mullion-app/src/app.rs
git commit -m "feat(app): 分组管理弹窗 + 会话列表按分组折叠 + 编辑器分组下拉 (F60)"
```

---

## Task 17：菜单入口接线

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`（菜单栏）

- [ ] **Step 1: 加菜单项**

在菜单栏里「会话管理」旁边（照抄它那几行的写法）加：

```rust
                if ui.button("分组管理…").clicked() {
                    ui_state.group_manager_open = true;
                    ui.close_menu();
                }
```

`ui.close_menu()` 是 egui 0.30 的写法；若既有「会话管理」那行用的是别的收尾方式，照抄它。

- [ ] **Step 2: 编译并跑测试**

Run: `cargo test -p mullion-app 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 菜单栏加分组管理入口 (F60)"
```

---

# 阶段四：验收与发布

## Task 18：红线守护 + 领域陷阱回归

设计 §2 的**红线 2**（`mullion-ssh` 永不认识 `mullion-store`）目前只靠人自觉。本任务把它做成会失败的测试。

**Files:**
- Create: `crates/mullion-ssh/tests/no_store_dependency.rs`

- [ ] **Step 1: 写守护测试**

新建 `crates/mullion-ssh/tests/no_store_dependency.rs`：

```rust
//! 红线 2 的机械守护:`mullion-ssh` 的依赖树里永不出现 `mullion-store`。
//!
//! 为什么要一个测试:P0-b 让 ssh 认识了「跳板」这个概念,最省事的写法是直接
//! 收一个 `SessionRecord`。那样 ssh 就依赖了 store,依赖方向从单向变成网状,
//! 「布局/键码 bug 能脱离窗口写测试」这条项目根基就没了(CLAUDE.md 架构不变量)。
//! 靠人自觉守不住,靠这个测试守。

use std::process::Command;

#[test]
fn ssh_crate_never_depends_on_store() {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", "mullion-ssh", "--edges", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree 应能执行");
    assert!(
        out.status.success(),
        "cargo tree 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(
        !tree.contains("mullion-store"),
        "红线 2 被打破:mullion-ssh 依赖了 mullion-store。\n\
         跳板信息必须由 app 的 dial_plan 物化成 Hop 再传进来。\n依赖树:\n{tree}"
    );
    // 顺带钉住整条红线:ssh 也不该认识 core/term/app。
    for forbidden in ["mullion-core", "mullion-term", "mullion-app"] {
        assert!(
            !tree.contains(forbidden),
            "mullion-ssh 依赖了 {forbidden},违反单向依赖 app → {{core,term,ssh,store}}"
        );
    }
}
```

`--edges normal` 排掉 dev-dependencies；即便将来 ssh 的测试用到别的 crate 也不会误报。

- [ ] **Step 2: 跑它**

Run: `cargo test -p mullion-ssh --test no_store_dependency 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

若这里失败，说明前面某个任务把 store 类型漏进了 ssh —— **不要放宽这个测试**，去改那处实现（`Hop` 应当只有 `String`/`u16`/`AuthMethod` 这类原始类型）。

- [ ] **Step 3: 领域陷阱回归**

P0-b 动了 `establish` 的返回类型与 `open_pty` 的入参，`SshConnection` 换成 `Arc` 共享。T1（`Event::PtyWrite` 回写 SSH channel）正在这条链路上。

Run: `cargo test -p mullion-term emulator::tests::pty_write_is_collected 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

Run: `cargo test -p mullion-app --lib -- reflow_emits_resize redraw_is_frame_capped 2>&1 | tail -5`
Expected: `test result: ok. 2 passed`（T3 / T4）

- [ ] **Step 4: 全绿**

```bash
cargo test --workspace > /tmp/p0b-test.log 2>&1
grep -nE "test result|FAILED|panicked" /tmp/p0b-test.log | tail -20
```
Expected: 每行 `test result: ok.`，无 `FAILED`、无 `panicked`

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: 无输出（除 `Finished` 行）

```bash
cargo fmt --check
```
Expected: 无输出

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/tests/no_store_dependency.rs
git commit -m "test(ssh): 红线 2 机械守护 —— ssh 依赖树里不得出现 store (F4/F5)"
```

---

## Task 19：假代理的端到端握手测试

代理握手是本切片**最容易写错又最难靠眼睛发现**的一段：SOCKS5 回复的 BND 字段长度随 ATYP 变化，HTTP CONNECT 的响应头不能多读一个字节。Task 8/9 的单测只覆盖了纯函数，本任务把真实的「连上去、握手、拿到可用流」跑通。

**范围说明（诚实标注）**：这里只测**代理**两跳。SSH 跳板那一跳需要一个真 sshd，`russh::server` 虽然可用（无需额外 feature），但搭一个能完成密钥交换的假 sshd 代码量远超收益，且**本计划中的假 sshd 代码未经编译验证**——因此不写。**两跳 SSH 跳板进人工验收清单（Task 20）**，不假装它被自动测过。

**Files:**
- Create: `crates/mullion-ssh/tests/proxy_handshake.rs`
- Modify: `crates/mullion-ssh/Cargo.toml`

- [ ] **Step 1: 加 dev-dependency**

`crates/mullion-ssh/Cargo.toml` 末尾追加：

```toml
# 假代理服务器要 accept 连接并读写字节。tokio 的 workspace features
# 已含 net/io-util/rt-multi-thread/macros,这里只是让 test target 能用上。
[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "io-util"] }
```

- [ ] **Step 2: 写测试**

新建 `crates/mullion-ssh/tests/proxy_handshake.rs`：

```rust
//! 代理握手的端到端测试:起一个进程内假代理,让 `dial` 真的连上去握手。
//!
//! 这段协议的坑不在逻辑而在**读多读少**:SOCKS5 回复的 BND 长度随 ATYP 变,
//! HTTP CONNECT 的响应头多读一个字节就吞掉了隧道里的 SSH 数据。单测覆盖不到,
//! 只有真收发才暴露。

use mullion_ssh::dial::dial;
use mullion_ssh::hop::Hop;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 起一个假的「目标服务器」,连上来就发 `banner`,然后回读一行并原样回显。
async fn spawn_echo_target(banner: &'static [u8]) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        s.write_all(banner).await.unwrap();
        let mut buf = [0u8; 16];
        let n = s.read(&mut buf).await.unwrap();
        s.write_all(&buf[..n]).await.unwrap();
    });
    port
}

/// 假 SOCKS5 代理。`bnd_atyp` 决定回复里用哪种地址类型——这正是最易错处。
async fn spawn_socks5(bnd_atyp: u8, target_port: u16) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();

        // 1) 方法协商:读 VER/NMETHODS/METHODS,回 05 00(免认证)。
        let mut head = [0u8; 2];
        c.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x05, "客户端必须发 SOCKS5");
        let mut methods = vec![0u8; head[1] as usize];
        c.read_exact(&mut methods).await.unwrap();
        c.write_all(&[0x05, 0x00]).await.unwrap();

        // 2) CONNECT 请求:VER CMD RSV ATYP ADDR PORT。
        let mut req = [0u8; 4];
        c.read_exact(&mut req).await.unwrap();
        assert_eq!(&req[..3], &[0x05, 0x01, 0x00], "应为 CONNECT");
        match req[3] {
            0x01 => { let mut a = [0u8; 4]; c.read_exact(&mut a).await.unwrap(); }
            0x03 => {
                let mut n = [0u8; 1];
                c.read_exact(&mut n).await.unwrap();
                let mut a = vec![0u8; n[0] as usize];
                c.read_exact(&mut a).await.unwrap();
            }
            0x04 => { let mut a = [0u8; 16]; c.read_exact(&mut a).await.unwrap(); }
            other => panic!("未知 ATYP {other}"),
        }
        let mut p = [0u8; 2];
        c.read_exact(&mut p).await.unwrap();

        // 3) 回成功。BND 字段按 bnd_atyp 变长——客户端必须按 ATYP 读完。
        let mut reply = vec![0x05, 0x00, 0x00, bnd_atyp];
        match bnd_atyp {
            0x01 => reply.extend_from_slice(&[127, 0, 0, 1]),
            0x03 => { reply.push(9); reply.extend_from_slice(b"localhost"); }
            0x04 => reply.extend_from_slice(&[0u8; 16]),
            other => panic!("未知 ATYP {other}"),
        }
        reply.extend_from_slice(&[0x00, 0x00]);
        c.write_all(&reply).await.unwrap();

        // 4) 双向转发到真目标。
        let mut up = tokio::net::TcpStream::connect(("127.0.0.1", target_port))
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut c, &mut up).await.ok();
    });
    port
}

/// 假 HTTP CONNECT 代理。响应头后**紧跟**隧道数据,用来抓「多读一块」的 bug。
async fn spawn_http_connect(target_port: u16, require_auth: bool) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();
        // 逐字节读到 \r\n\r\n,不能多读——多读的就是隧道数据。
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while c.read_exact(&mut b).await.is_ok() {
            head.push(b[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head).to_string();
        assert!(head.starts_with("CONNECT "), "应为 CONNECT 请求: {head}");
        if require_auth && !head.contains("Proxy-Authorization: Basic ") {
            c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
            return;
        }
        c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let mut up = tokio::net::TcpStream::connect(("127.0.0.1", target_port))
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut c, &mut up).await.ok();
    });
    port
}

async fn assert_tunnel_works(hop: Hop, target_port: u16) {
    let dialed = dial(&[hop], "127.0.0.1", target_port, policy())
        .await
        .expect("拨号应成功");
    let mut s = dialed.stream;
    let mut banner = [0u8; 6];
    s.read_exact(&mut banner).await.expect("应读到目标 banner");
    assert_eq!(&banner, b"SSH-2.", "握手后第一个字节必须是目标的,不是代理残留");
    s.write_all(b"ping").await.unwrap();
    let mut echo = [0u8; 4];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"ping");
}

fn socks5(port: u16) -> Hop {
    Hop::Socks5 { host: "127.0.0.1".into(), port, auth: None }
}

/// 本测试全是代理跳,不涉及 SSH 握手,主机密钥策略永远用不上——
/// 但 `dial` 的签名要一个,给个最简实现。
/// (`HostKeyPolicy` 是 trait,实参类型 `Arc<dyn HostKeyPolicy>`,**没有** `Default`。)
struct NoPolicy;
impl mullion_ssh::known_hosts::HostKeyPolicy for NoPolicy {
    fn decide<'a>(
        &'a self,
        _host: &'a str,
        _algo: &'a str,
        _fp: &'a mullion_ssh::known_hosts::Fingerprint,
    ) -> mullion_ssh::known_hosts::HostKeyFuture<'a> {
        Box::pin(std::future::ready(
            mullion_ssh::known_hosts::HostKeyDecision::Accept,
        ))
    }
}

fn policy() -> std::sync::Arc<dyn mullion_ssh::known_hosts::HostKeyPolicy> {
    std::sync::Arc::new(NoPolicy)
}

/// 三种 ATYP 各跑一遍:BND 长度算错会让残留字节污染 banner,断言当场炸。
#[tokio::test]
async fn socks5_tunnel_survives_every_bnd_address_type() {
    for atyp in [0x01u8, 0x03, 0x04] {
        let target = spawn_echo_target(b"SSH-2.0-fake\r\n").await;
        let proxy = spawn_socks5(atyp, target).await;
        assert_tunnel_works(socks5(proxy), target).await;
    }
}

#[tokio::test]
async fn http_connect_tunnel_does_not_swallow_tunnel_bytes() {
    let target = spawn_echo_target(b"SSH-2.0-fake\r\n").await;
    let proxy = spawn_http_connect(target, false).await;
    assert_tunnel_works(
        Hop::HttpConnect { host: "127.0.0.1".into(), port: proxy, auth: None },
        target,
    )
    .await;
}

/// 407 必须映射成 `ProxyAuthFailed`,不能混进泛化的「代理拒绝」——
/// 这两种情况用户的下一步动作完全不同(F6:每个错都要可行动)。
#[tokio::test]
async fn http_connect_407_maps_to_proxy_auth_failed() {
    let target = spawn_echo_target(b"SSH-2.0-fake\r\n").await;
    let proxy = spawn_http_connect(target, true).await;
    let err = dial(
        &[Hop::HttpConnect { host: "127.0.0.1".into(), port: proxy, auth: None }],
        "127.0.0.1",
        target,
        policy(),
    )
    .await
    .expect_err("无凭据应被 407 拒绝");
    assert!(
        matches!(err, mullion_ssh::error::ConnectError::ProxyAuthFailed { .. }),
        "407 应映射成 ProxyAuthFailed,实际: {err:?}"
    );
}

/// 带凭据时 407 不该发生:验证 Basic 头真的被发出去了(手写 base64 的守护)。
#[tokio::test]
async fn http_connect_sends_basic_credentials() {
    let target = spawn_echo_target(b"SSH-2.0-fake\r\n").await;
    let proxy = spawn_http_connect(target, true).await;
    assert_tunnel_works(
        Hop::HttpConnect {
            host: "127.0.0.1".into(),
            port: proxy,
            auth: Some(("alice".into(), "secret".into())),
        },
        target,
    )
    .await;
}

/// 代理端口没人监听 → 必须是「代理连不上」,不能报成「目标连不上」,
/// 否则用户会去查目标主机而真正的问题在代理(F6)。
#[tokio::test]
async fn unreachable_proxy_blames_the_proxy_not_the_target() {
    // 绑一个端口再立刻释放,拿到一个几乎必然无人监听的号。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    let err = dial(&[socks5(dead)], "example.invalid", 22, policy())
        .await
        .expect_err("代理不可达应失败");
    let msg = format!("{err}");
    assert!(msg.contains("代理"), "错误消息应点名代理: {msg}");
}
```

`dial()` 的第四个参数是 `Arc<dyn HostKeyPolicy>`（与 `establish` 一致，Task 11 定义）。
`Dialed.stream` 是 `DialStream`，其 `AsyncRead`/`AsyncWrite` impl 在 Task 11 已给出。
`HostKeyPolicy` / `HostKeyFuture` / `HostKeyDecision` / `Fingerprint` 都是 `mullion-ssh`
已有的公开项（`crates/mullion-ssh/src/known_hosts.rs:100-126`），不需要新增。

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-ssh --test proxy_handshake 2>&1 | tail -15`
Expected: `test result: ok. 6 passed`

若 `socks5_tunnel_survives_every_bnd_address_type` 在某个 ATYP 上失败，去 Task 8 的 `read_socks5_reply` 找长度计算，**不要**改测试里的 ATYP 集合。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/tests/proxy_handshake.rs crates/mullion-ssh/Cargo.toml
git commit -m "test(ssh): 进程内假代理端到端握手,覆盖三种 SOCKS5 BND 与 HTTP 407 (F4/F6)"
```

---

## Task 19b：假 sshd 两跳集成测试（**先读源码再写**）

设计 §8.1 要求用 `russh::server` 起假 sshd 做真实两跳，理由是 §5.2 的 Handle 保活 bug
纯单测抓不到。**这个任务与本计划其余任务不同：它的代码没有写在这里**，因为
`russh::server` 的 trait 签名未经编译验证——按 CLAUDE.md「API 漂移」纪律，
凭记忆写 russh server 的代码是明确禁止的。

已核实的前置条件：`russh` 的 `server` 模块只被 `#[cfg(not(target_arch = "wasm32"))]`
gate（`lib_inner.rs:64-66`），在我们 `default-features = false, features = ["ring","flate2","rsa"]`
的配置下**无需额外 feature**，作 dev-dependency 引入不进发布产物。

**Files:**
- Create: `crates/mullion-ssh/tests/two_hop_jump.rs`

- [ ] **Step 1: 读实际 API（不要跳过）**

```bash
RUSSH=$(ls -d ~/.cargo/registry/src/*/russh-0.54.*)
echo "$RUSSH"
grep -nE "pub (async )?fn |pub trait |pub struct Config" "$RUSSH/src/server/mod.rs" | head -60
grep -rnE "pub fn random|pub struct PrivateKey" ~/.cargo/registry/src/*/russh-keys-*/src/lib.rs | head
```

记下四件事再往下写：
1. `server::Handler` 的 `auth_password` / `channel_open_session` / `channel_open_direct_tcpip` /
   `pty_request` / `shell_request` 的**确切签名**（是否 `async fn`、返回 `Result<(Self, ...)>`
   还是 `Result<bool>`、`session` 是 `&mut Session` 还是 `Session`）
2. `server::Server` trait 怎么起监听（`run_on_address` / `run_on_socket` 的签名）
3. `server::Config` 的字段名与 `keys` 的类型
4. 生成一把临时主机密钥的正确写法（`PrivateKey::random` 需要哪个 rng 参数）

- [ ] **Step 2: 写两跳测试**

在 `crates/mullion-ssh/tests/two_hop_jump.rs` 里，用 Step 1 读到的**实际签名**实现：

- 假 sshd A（扮演跳板）：接受任意口令认证；实现 `channel_open_direct_tcpip`，
  把 channel 双向转发到请求里指定的 `host:port`
- 假 sshd B（扮演目标）：接受任意口令认证；`channel_open_session` + `shell_request`
  后写出一行 `hello`
- 测试：`SshConfig` 的 `hops` 放一个 `Hop::SshJump` 指向 A，`host/port` 指向 B，
  `HostKeyPolicy` 用「一律接受」的测试变体

必须包含的三条断言（少一条这个任务就没达到目的）：

```rust
// 1) 跳板 Handle 真的被 SshConnection 抓住了。
//    若 dial() 把它丢在栈上,这里是 0——这正是 §5.2 那个「连上了几毫秒后无故断」的 bug。
assert_eq!(conn.jump_handle_count(), 1, "跳板 Handle 必须被连接持有");

// 2) 隧道真的通到了目标,而不是停在跳板上。
assert!(output.contains("hello"), "应读到目标 sshd 的输出");

// 3) 保活:等待明显超过一次 keepalive 周期后连接仍可用。
//    Handle 被 Drop 的表现就是这里读不到东西。
tokio::time::sleep(std::time::Duration::from_secs(3)).await;
assert!(conn.is_alive(), "3 秒后连接仍应存活");
```

若 `SshConnection` 没有 `is_alive()`，用「再开一个 channel 成功」代替——**不要**删掉这条断言。

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-ssh --test two_hop_jump 2>&1 | tail -15`
Expected: `test result: ok. 1 passed`

- [ ] **Step 4: 若受阻**

`russh::server` 的 API 若与预期偏差过大（例如 Handler 要求实现十几个方法、
或需要额外的密钥/算法配置），**报 BLOCKED，不要硬凑**。回退方案：

1. 跳过本任务，在 Task 20 的 Release notes 里**明确写出**「SSH 跳板路径没有自动化测试」
   （notes 模板里已经这么写了，保持不动）
2. 把跳板保活列为实机验收的**必验项**（清单里已有「放置 2 分钟不掉线」）
3. 在 `docs/superpowers/plans/` 的本计划末尾追加一行偏差记录，说明为何跳过

这不是失败——这是在「无法验证就不假装验证了」和「花两天跟 API 搏斗」之间做的取舍。

- [ ] **Step 5: 提交（若完成）**

```bash
git add crates/mullion-ssh/tests/two_hop_jump.rs crates/mullion-ssh/Cargo.toml
git commit -m "test(ssh): 假 sshd 两跳集成测试,钉住跳板 Handle 保活 (F5)"
```

---

## Task 20：版本 bump、交叉编译、发布

按 CLAUDE.md「交付约定」一条龙做完，不中途询问。

**Files:**
- Modify: `Cargo.toml`（`workspace.package.version`）

- [ ] **Step 1: bump 版本**

`/data/Mullion/Cargo.toml`：

```toml
version = "0.1.15"
```

```bash
cargo check --workspace 2>&1 | tail -3   # 让 Cargo.lock 跟着更新
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.15(SSH 代理与跳板链,会话分组 UI)"
```

- [ ] **Step 2: 跑绿（发版前的硬门槛）**

```bash
cargo test --workspace > /tmp/p0b-release.log 2>&1
grep -cE "^test result: ok" /tmp/p0b-release.log
grep -nE "FAILED|panicked" /tmp/p0b-release.log | head
```
Expected: 第一条输出为各 crate 测试套数量，第二条**无输出**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | grep -c "^warning\|^error"
cargo fmt --check
```
Expected: `0`；`fmt --check` 无输出

不绿不发。这是 CLAUDE.md 的硬约束，不许「先发了再说」。

- [ ] **Step 3: 交叉编译**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -3
```
Expected: `Finished \`release\` profile`

- [ ] **Step 4: objdump 依赖验收**

```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep "DLL Name"
```
Expected: 只出现系统 DLL（`KERNEL32.dll` / `USER32.dll` / `ntdll.dll` / `bcrypt.dll` / `d3d12.dll` 等）。

**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` 即为不合格**——用户机器上没有这两个 DLL，exe 双击直接闪退。修法见 `docs/cross-compile-windows.md`（静态链接 mingw runtime），修完重跑 Step 3。

- [ ] **Step 5: 备好产物与校验和**

```bash
mkdir -p /tmp/rel-0115
cp target/x86_64-pc-windows-gnu/release/mullion.exe /tmp/rel-0115/
cd /tmp/rel-0115 && sha256sum mullion.exe > mullion.exe.sha256 && cat mullion.exe.sha256
```

- [ ] **Step 6: 写 Release notes**

写入 `/tmp/rel-0115/notes.md`（把 Step 5 拿到的实际 sha256 填进「校验」段；其余原样）：

```markdown
切片 **P0-b：SSH 代理与跳板**。这一版让 Mullion 能穿代理、经堡垒机连内网机器，
并第一次把「分组」暴露到界面上。

## 改了什么

- **代理（F4）**：会话可走 SOCKS5 或 HTTP CONNECT 代理，支持用户名口令认证
- **跳板（F5）**：会话可指定一条 SSH 跳板链（多跳），跳板本身就是一条已保存的会话，
  凭据取自跳板自己那条会话
- **分组 UI（F60）**：菜单栏新增「分组管理」；会话列表按分组折叠；
  编辑器可给会话选分组
- **继承**：代理与跳板可在分组上设一次，组内会话自动继承。
  会话上的「不使用代理」是**显式覆盖**，会盖掉分组的代理设置；
  「跟随分组」才是继承
- **配置升到 schema v3**：新增 `[session.network]` 分节。
  v3 能直接读 v2，但**旧版本客户端会拒绝打开 v3 文件**——
  别用 0.1.14 或更早的版本去开这一版写过的配置

## 安全相关的取舍

- 跳板引用**悬空就硬失败**（引用的会话被删了 → 连接报错，绝不静默直连）。
  静默降级会让你以为流量过了堡垒机而实际上是直连，这是安全属性，不做「贴心」处理
- 跳板链**成环或超过 8 跳直接报错**，不尝试自作聪明地截断
- 代理口令与 SSH 凭据一样进加密凭据文件，不落明文；日志里所有凭据打码

## 人工验收清单

无头容器里验不了的，需要你在 Windows 11 实机确认：

### 1. 代理（本版重点）

- [ ] 配一条走 SOCKS5 代理的会话（可用本地 `127.0.0.1:7891` 试），能连上
- [ ] 配一条走 HTTP CONNECT 代理的会话，能连上
- [ ] 代理填错端口 → 错误消息**点名是代理连不上**，而不是说目标主机连不上
- [ ] 代理要认证但没填口令 → 错误消息说的是**代理认证失败**，不是 SSH 认证失败

### 2. 跳板（自动测试未覆盖，务必人工验）

**这一项没有自动化测试覆盖**（进程内假 sshd 成本过高，见计划 Task 19），
完全依赖你这次实测：

- [ ] 单跳：A 会话经堡垒机 B 连到内网 C，能连上、能正常敲命令
- [ ] 两跳：经 B → D 再到 C，能连上
- [ ] **连上后放置 2 分钟不动，连接不掉线**（跳板连接的保活；若这里掉线，
      是 `SshConnection` 没抓住跳板 Handle，务必反馈）
- [ ] 关掉窗口后，`ps` 看不到残留连接（跳板 channel 没泄漏）
- [ ] 删掉被引用的跳板会话，再连引用它的会话 → **报错**，不是悄悄直连
- [ ] 把 A 的跳板设成 A 自己 → 报错提示成环

### 3. 代理 + 跳板组合

- [ ] 同时配代理和跳板：先出代理再谈跳板，能连上

### 4. 分组

- [ ] 菜单栏「分组管理」能新建、删除分组
- [ ] 会话列表按分组折叠，未分组的会话在最后一组，**一条不少**
- [ ] 在分组上设代理，组内会话不设 → 会话继承到该代理
- [ ] 组内某会话选「不使用代理」→ 该会话直连，其余仍走组代理
- [ ] 删掉一个分组，组内会话**仍在列表里**（落到「未分组」），没有消失

### 5. 回归（这一版不该动到的地方）

- [ ] 编辑一条已有会话再保存，代理/跳板/分组/备注/图标**都不丢**
      （`Vault::update` 是整节替换，透传漏一个就静默清空）
- [ ] 直连会话（不配代理不配跳板）一切如常
- [ ] 连接、分屏、划选复制粘贴、滚动回溯与上一版一致
- [ ] 远端 tmux 里跑 Claude Code：不闪、字形/CJK 对齐正常、Shift+Enter 能换行

## 校验

```
sha256  <填入 Step 5 输出的实际值>  mullion.exe
```

## 首次运行

exe 未签名，每个新版本都会被 SmartScreen 拦一次。下载后先解除锁定：

```powershell
Unblock-File .\mullion.exe
```

详见 `docs/cross-compile-windows.md`。
```

- [ ] **Step 7: 发 Release**

标题**只能是纯版本号**，不带破折号、摘要或 emoji。

```bash
cd /tmp/rel-0115
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.15 \
  mullion.exe mullion.exe.sha256 -t "v0.1.15" -F notes.md --repo kilobitcy/Mullion
```
Expected: 输出 Release URL

（本机 DNS 解析不了 github，`gh` 必须带 `HTTPS_PROXY`；GitHub Actions 因账单锁不可用，
不要试图走 `release.yml`。）

- [ ] **Step 8: 报告**

给用户：Release 链接 + sha256 + 上面的人工验收清单，并**明确说明**：

- 代理握手已有进程内端到端测试覆盖
- **SSH 跳板路径没有自动化测试**，两跳、保活、channel 泄漏三项完全依赖实机验收
- 渲染/不闪/输入法/手感一如既往无法自动验证

- [ ] **Step 9: 收尾**

Run: `superpowers:finishing-a-development-branch`（分支 `feat/p0b-proxy-jump`，
项目惯例是 squash 入 main）

---

## 附：本计划与设计 spec 的对照

对照 `docs/superpowers/specs/2026-07-31-slice-p0b-proxy-jump-design.md` 逐节核过一遍：

| 设计 spec 章节 | 落地任务 |
|---|---|
| §3.1 store 侧新开可继承分节 `NetworkPrefs` | Task 1、Task 2 |
| §3.2 「未设置」与「显式设为空」可区分 | Task 2（`explicit_direct_overrides_group_proxy_instead_of_inheriting`、`explicit_empty_jump_chain_overrides_group_chain`） |
| §3.3 schema 升 v3 | Task 3 |
| §3.4 ssh 侧 `dial.rs` 的 `Hop` | Task 7、Task 11 |
| §4 三层分工（store 只给引用图，app 物化，ssh 只见 `Hop`） | Task 4、Task 12 |
| §4.1 跳板引用图展开规则（环/深度/悬空/递归/去重） | Task 4（10 个测试） |
| §5.1 拨号执行流程 | Task 11 |
| §5.2 Handle 保活（最危险处） | Task 10（结构）、Task 19b（真实两跳断言） |
| §5.3 跳板机主机密钥校验走同一 `HostKeyPolicy` | Task 11（目标与跳板共用 `handshake_and_auth`，policy 逐跳传入）；TOFU 弹窗次数进人工验收 |
| §6 错误处理（分段可定位，F6） | Task 6、Task 8、Task 9、Task 19 |
| §7.1 会话编辑器（代理 + 跳板） | Task 14、Task 15 |
| §7.2 会话管理器按分组折叠 | Task 16 |
| §7.3 极简分组管理 | Task 16、Task 17 |
| §7.4 与 P0-a 遗留物衔接（`preserved_*` 透传） | Task 14（守护测试保留）、Task 16（`preserved_group_id` 变可编辑） |
| §8 测试策略全表 | Task 1–4、Task 12、Task 18、Task 19 |
| §8.1 in-process 两跳集成测试 | Task 19b（**有条件**，见下） |
| §9 人工验收清单 | Task 20 的 Release notes |
| §10 与架构不变量的关系（红线 2） | Task 12（物化边界）、Task 18（机械守护） |

### 与设计 spec 的两处偏离

1. **§8.1 的两跳集成测试降级为「先读源码再写、可 BLOCKED」**（Task 19b）。
   spec 要求「计划阶段必须先验证，不能假定可行」——已验证 `russh::server`
   模块可用且无需额外 feature，但**具体 trait 签名未经编译验证**。按 CLAUDE.md
   「API 漂移」纪律，不把凭记忆写的 russh server 代码塞进计划。作为补偿，
   Task 19 用纯 tokio 假代理把**代理握手**这段（真正的字节级易错点）端到端测透了，
   这部分零风险、必定可写。

2. **代理握手测试的覆盖比 spec §8 表格更细**：spec 只要求「域名/IPv4/IPv6 三种地址类型」，
   Task 19 额外对**三种 BND 回复地址类型**各跑一遍隧道。因为真正的 bug 在
   *读回复时按 ATYP 算长度*，而不是*发请求时选 ATYP*——读少了残留字节会污染
   后续 SSH 版本协商，且症状极难归因。

### spec 中本切片不做的部分

§11「留给后续期的问题」三项（P3-a 自动重连的拨号链重放、P2-a 图标/配色接上后
`preserved_*` 退休、代理口令的分组级共享）本计划**不涉及**，与 spec 一致。
