# 切片 P0-a：会话 store 地基 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `SessionRecord` 从扁平结构重构成分节嵌套、引入单层分组（F60）与两种继承策略、
并带 `schema_version` 一次性迁移旧 TOML，一次把会话配置的数据格式定死。

**Architecture:** 全部工作在 `mullion-store`（零 UI / 零 async / 仅同步 IO），逻辑为纯函数可无窗口单测。
继承通过 `PrefsLayer` trait + 有序 layer 序列表达，未来插入新的继承层（F64 环境等级）
不改 `resolve()` 签名。`mullion-app` 只做字段访问路径的机械适配，不加新 UI。

**Tech Stack:** Rust / serde 1 / toml 0.8.23 / tempfile（已在 dev-dependencies）

**设计依据:** `docs/superpowers/specs/2026-07-30-session-management-roadmap-design.md` §4.1–§4.3

---

## 前置事实（已实测，不要重新怀疑）

用 toml 0.8.23 跑过探针，以下三条已验证，实现时可直接依赖：

1. 内部标签枚举（`#[serde(tag = "kind")]`）**可以**嵌套在 table 内序列化并 round-trip。
   配合 `#[serde(flatten)]` 产出的形态最扁平：
   ```toml
   [session.auth]
   user = "u"
   kind = "public_key"
   path = "/k"
   has_passphrase = true
   ```
2. `Option<T>` + `skip_serializing_if = "Option::is_none"` 在嵌套分节里正常跳过。
3. **结构体字段声明顺序无关**——toml 0.8 的 serializer 自动把标量提前、table 置后。
   标量字段写在 table 字段之后也不报错。

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-store/src/model.rs` | 会话记录与各分节类型 | 改（重构） |
| `crates/mullion-store/src/group.rs` | `GroupRecord`（F60） | 新建 |
| `crates/mullion-store/src/inherit.rs` | `PrefsLayer` + 两种 resolve 策略 + `ResolvedConfig` | 新建 |
| `crates/mullion-store/src/migrate.rs` | v1 结构定义与迁移函数 | 新建 |
| `crates/mullion-store/src/vault.rs` | 接入迁移、分组 CRUD | 改 |
| `crates/mullion-store/src/lib.rs` | 导出新模块 | 改 |
| `crates/mullion-app/src/shell/session_map.rs` | 字段路径适配 | 改 |
| `crates/mullion-app/src/shell/store.rs` | 字段路径适配 | 改 |
| `crates/mullion-app/src/ui/session_manager.rs` | 字段路径适配 + `build_draft` 改分节 | 改 |
| `crates/mullion-app/src/ui/mod.rs` | 字段路径适配 | 改 |
| `crates/mullion-app/src/app.rs` | 字段路径适配（**只改字段路径，不碰事件循环**） | 改 |

**继承策略（来自设计 §4.1，实现时按此表落位）**

| 字段 | 策略 |
|---|---|
| 标量（scrollback 等） | Override：按优先级取第一个 `Some`，全 `None` 用内置默认 |
| `tags` | Merge：各层并集，保序去重，**上游在前** |
| 复合对象（`icon` / `color`） | Override，**整体继承或整体覆盖**，禁止字段级部分覆盖 |

**可继承边界（来自设计 §4.2）**：分组只持有 `tags` / `terminal` / `appearance`。
`host` / `port` / `user` / `auth` / `name` / `id` 永不可继承。

---

## Task 1: 可继承分节类型

**Files:**
- Modify: `crates/mullion-store/src/model.rs`

- [ ] **Step 1: 写失败测试**

在 `model.rs` 的 `mod tests` 里追加：

```rust
#[test]
fn prefs_sections_skip_none_fields() {
    let t = TerminalPrefs { scrollback: None };
    let s = toml::to_string_pretty(&t).unwrap();
    assert_eq!(s.trim(), "", "全 None 的分节不应写出任何键");

    let a = AppearancePrefs {
        icon: Some(IconSpec { kind: IconKind::Emoji, value: "🐧".into() }),
        color: None,
    };
    let s = toml::to_string_pretty(&a).unwrap();
    assert!(s.contains("emoji"), "icon 应写出: {s}");
    assert!(!s.contains("color"), "None 的 color 不应写出: {s}");
    let back: AppearancePrefs = toml::from_str(&s).unwrap();
    assert_eq!(back, a);
}

#[test]
fn color_spec_round_trips_with_targets() {
    let c = ColorSpec {
        hex: "#E5484D".into(),
        apply_to: vec![ColorTarget::Tab, ColorTarget::StatusBar],
    };
    let s = toml::to_string_pretty(&c).unwrap();
    let back: ColorSpec = toml::from_str(&s).unwrap();
    assert_eq!(back, c);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib model 2>&1 | tail -20`
Expected: 编译失败，`cannot find type TerminalPrefs in this scope`

- [ ] **Step 3: 实现**

在 `model.rs` 的 `SessionsFile` 之前插入：

```rust
/// 分组稳定主键。新建时取现有 max+1(见 vault)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId(pub u64);

/// 图标来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconKind {
    /// 内置图标库中的名字(如 "ubuntu")。
    Builtin,
    /// 单个 emoji 字符。
    Emoji,
    /// 用户提供的图片路径。
    Custom,
}

/// 图标规格。**复合对象:只能整体继承或整体覆盖**(设计 §4.1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconSpec {
    pub kind: IconKind,
    pub value: String,
}

/// 颜色的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorTarget {
    Tab,
    ListItem,
    PaneTitle,
    StatusBar,
}

/// 颜色规格。**复合对象:只能整体继承或整体覆盖**(设计 §4.1)——
/// 明确不支持「只覆盖 hex、沿用上游的 apply_to」这类字段级部分覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorSpec {
    pub hex: String,
    #[serde(default)]
    pub apply_to: Vec<ColorTarget>,
}

/// 终端偏好(可继承分节)。字段一律 `Option`,`None` 即继承上游。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalPrefs {
    /// 滚动回溯行数(F17)。内置默认见 `inherit::DEFAULT_SCROLLBACK`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<u32>,
}

/// 外观偏好(可继承分节)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearancePrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorSpec>,
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib model 2>&1 | tail -20`
Expected: PASS，新增 2 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/model.rs
git commit -m "feat(store): 可继承分节类型 TerminalPrefs/AppearancePrefs (F60)"
```

---

## Task 2: SessionRecord 拆分节 + GroupRecord（F60）

`SessionsFile` 里要放 `Vec<GroupRecord>`，所以这两件事在同一次编译里才成立，
合成一个任务做（各自的测试仍然分开写）。

**Files:**
- Modify: `crates/mullion-store/src/model.rs`
- Create: `crates/mullion-store/src/group.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

**替换** `model.rs` 中已有的 `session_toml_round_trips` 测试为：

```rust
#[test]
fn session_toml_round_trips() {
    let rec = SessionRecord {
        id: SessionId(7),
        modified_at: "2026-07-25T00:00:00Z".into(),
        identity: Identity {
            name: "dev".into(),
            note: "跳板后".into(),
            group_id: Some(GroupId(2)),
            tags: vec!["prod".into()],
        },
        connection: Connection {
            host: "192.0.2.10".into(),
            port: 22,
            protocol: Protocol::Ssh,
        },
        auth: Auth {
            user: "user".into(),
            kind: AuthKind::PublicKey {
                path: "/path/to/key.pem".into(),
                has_passphrase: false,
            },
        },
        terminal: TerminalPrefs { scrollback: Some(5000) },
        appearance: AppearancePrefs::default(),
    };
    let file = SessionsFile {
        schema_version: CURRENT_SCHEMA,
        group: Vec::new(),
        session: vec![rec.clone()],
    };
    let s = toml::to_string_pretty(&file).unwrap();
    let back: SessionsFile = toml::from_str(&s).unwrap();
    assert_eq!(back.session, vec![rec]);
}

#[test]
fn auth_kind_flattens_into_auth_section() {
    let rec = SessionRecord {
        id: SessionId(1),
        modified_at: "t".into(),
        identity: Identity { name: "a".into(), note: String::new(), group_id: None, tags: Vec::new() },
        connection: Connection { host: "h".into(), port: 22, protocol: Protocol::Ssh },
        auth: Auth { user: "u".into(), kind: AuthKind::Password },
        terminal: TerminalPrefs::default(),
        appearance: AppearancePrefs::default(),
    };
    let file = SessionsFile { schema_version: CURRENT_SCHEMA, group: Vec::new(), session: vec![rec] };
    let s = toml::to_string_pretty(&file).unwrap();
    assert!(s.contains("[session.auth]"), "应有 auth 分节: {s}");
    assert!(s.contains(r#"kind = "password""#), "kind 应平铺进 auth 分节: {s}");
    assert!(!s.contains("[session.auth.kind]"), "不应多出一层 table: {s}");
}
```

`group.rs` 是新文件，定义与 `mod tests` 一并创建，其测试代码见 Step 3 的文件全文。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib model 2>&1 | tail -20`
Expected: 编译失败，`struct SessionRecord has no field named identity`

- [ ] **Step 3: 实现**

**替换** `model.rs` 中的 `SessionRecord` 与 `SessionsFile` 定义：

```rust
/// 身份与组织(分节)。`name` 不可继承(设计 §4.2)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// 所属分组;`None` = 未分组(不参与继承)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    /// 标签。继承策略为 **Merge**(设计 §4.1)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// 连接目标(分节)。**永不可继承**——连接目标是会话身份本身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
}

/// 认证(分节)。**永不可继承**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub user: String,
    /// 平铺进 `[session.auth]`,不额外产生一层 table。
    #[serde(flatten)]
    pub kind: AuthKind,
}

/// 一条会话(非敏感字段)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    /// RFC3339;由调用方(app)注入,store 不持有时钟。
    pub modified_at: String,
    pub identity: Identity,
    pub connection: Connection,
    pub auth: Auth,
    #[serde(default)]
    pub terminal: TerminalPrefs,
    #[serde(default)]
    pub appearance: AppearancePrefs,
}

/// 当前 TOML 结构版本。缺失该键的文件视为 v1(见 `migrate`)。
pub const CURRENT_SCHEMA: u32 = 2;

fn schema_v1() -> u32 {
    1
}

/// sessions.toml 的顶层结构:产生 `[[group]]` 与 `[[session]]` 数组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsFile {
    /// 旧文件没有这个键 → 解析为 1 → 触发迁移。
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    #[serde(default)]
    pub group: Vec<crate::group::GroupRecord>,
    #[serde(default)]
    pub session: Vec<SessionRecord>,
}
```

同时把 `empty_toml_parses_to_no_sessions` 改为：

```rust
#[test]
fn empty_toml_parses_to_no_sessions() {
    let back: SessionsFile = toml::from_str("").unwrap();
    assert!(back.session.is_empty(), "空文件应解析为零会话,不报错");
    assert!(back.group.is_empty());
    assert_eq!(back.schema_version, 1, "缺 schema_version 的文件视为 v1");
}
```

创建 `crates/mullion-store/src/group.rs`：

```rust
//! 单层分组(F60)。分组只持有**可继承**字段(设计 §4.2):
//! tags / terminal / appearance。连接目标与凭据永不进分组。

use serde::{Deserialize, Serialize};

use crate::model::{AppearancePrefs, GroupId, TerminalPrefs};

/// 一个分组。不嵌套——单层结构(设计 D1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRecord {
    pub id: GroupId,
    pub name: String,
    /// 继承策略 Merge。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub terminal: TerminalPrefs,
    #[serde(default)]
    pub appearance: AppearancePrefs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColorSpec, ColorTarget};

    #[test]
    fn group_round_trips() {
        let g = GroupRecord {
            id: GroupId(3),
            name: "生产".into(),
            tags: vec!["prod".into()],
            terminal: TerminalPrefs { scrollback: Some(50_000) },
            appearance: AppearancePrefs {
                icon: None,
                color: Some(ColorSpec {
                    hex: "#E5484D".into(),
                    apply_to: vec![ColorTarget::Tab],
                }),
            },
        };
        let s = toml::to_string_pretty(&g).unwrap();
        let back: GroupRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, g);
    }
}
```

最后在 `crates/mullion-store/src/lib.rs` 的既有 `pub mod` 列表里按字母序插入：

```rust
pub mod group;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib 2>&1 | tail -20`
Expected: PASS，model 与 group 的测试全通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/model.rs crates/mullion-store/src/group.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): SessionRecord 拆分节 + 单层分组 GroupRecord (F60)"
```

---

## Task 3: resolve_override（标量继承）

**Files:**
- Create: `crates/mullion-store/src/inherit.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

创建 `crates/mullion-store/src/inherit.rs`：

```rust
//! 继承解析(设计 §4.1)。全部纯函数,零 IO。
//!
//! **层序约定**:所有 `sources` 一律按**优先级从高到低**传入(会话在前、分组在后)。
//! `resolve_merge_list` 内部负责反转,以产出「上游在前」的结果顺序。

/// 滚动回溯的内置默认(F17,spec 写的 10000)。
pub const DEFAULT_SCROLLBACK: u32 = 10_000;

/// 标量继承:按优先级取第一个 `Some`;全 `None` 则用内置默认。
///
/// 复合对象(`IconSpec`/`ColorSpec`)也走这里——**整体覆盖**,
/// 不支持字段级部分覆盖(设计 §4.1 硬限制)。
pub fn resolve_override<T>(sources: impl IntoIterator<Item = Option<T>>, builtin: T) -> T {
    sources.into_iter().flatten().next().unwrap_or(builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_takes_highest_priority_some() {
        let got = resolve_override([Some(5u32), Some(9), None], DEFAULT_SCROLLBACK);
        assert_eq!(got, 5, "会话层(最高优先级)应胜出");
    }

    #[test]
    fn override_falls_through_none_to_next_layer() {
        let got = resolve_override([None, Some(9u32)], DEFAULT_SCROLLBACK);
        assert_eq!(got, 9, "会话未设时应取分组值");
    }

    #[test]
    fn override_falls_back_to_builtin_when_all_none() {
        let got = resolve_override([None, None::<u32>], DEFAULT_SCROLLBACK);
        assert_eq!(got, DEFAULT_SCROLLBACK, "全未设时应取内置默认");
    }

    #[test]
    fn override_supports_more_than_two_layers() {
        // 为 F64「环境等级隐含默认」预留:插入第三层不需要改签名。
        let got = resolve_override([None, None, Some(7u32)], DEFAULT_SCROLLBACK);
        assert_eq!(got, 7);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: 失败，`file not found for module inherit`

- [ ] **Step 3: 在 lib.rs 挂载模块**

```rust
pub mod inherit;
```

本任务的 `inherit.rs` 刻意**不写** `use crate::model::...`——此刻还没有类型用得上它，
写了就是 `unused_imports` 警告，在 `-D warnings` 下直接变错误。Task 5 需要时再加。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: PASS，4 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/inherit.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 标量继承 resolve_override,支持任意层数"
```

---

## Task 4: resolve_merge_list（列表继承）

**Files:**
- Modify: `crates/mullion-store/src/inherit.rs`

- [ ] **Step 1: 写失败测试**

在 `inherit.rs` 的 `mod tests` 里追加：

```rust
#[test]
fn merge_puts_upstream_first_and_dedups() {
    // 入参按优先级从高到低:会话在前、分组在后。
    let got = resolve_merge_list([
        vec!["web01".to_string(), "prod".to_string()],
        vec!["prod".to_string(), "华东".to_string()],
    ]);
    assert_eq!(
        got,
        vec!["prod".to_string(), "华东".to_string(), "web01".to_string()],
        "结果应「上游在前」,且重复项只保留一次"
    );
}

#[test]
fn merge_of_empty_layers_is_empty() {
    let got = resolve_merge_list(Vec::<Vec<String>>::new());
    assert!(got.is_empty());
}

#[test]
fn merge_preserves_order_within_a_layer() {
    let got = resolve_merge_list([vec!["b".to_string(), "a".to_string()]]);
    assert_eq!(got, vec!["b".to_string(), "a".to_string()], "层内顺序不排序");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: 编译失败，`cannot find function resolve_merge_list`

- [ ] **Step 3: 实现**

在 `inherit.rs` 的 `resolve_override` 之后追加：

```rust
/// 列表继承:各层并集,保序去重。
///
/// 入参同样按**优先级从高到低**传入,但产出顺序为「上游在前」——
/// 分组的 `prod` 排在会话的 `web01` 之前,读起来符合「从大类到具体」的直觉。
pub fn resolve_merge_list(sources: impl IntoIterator<Item = Vec<String>>) -> Vec<String> {
    let layers: Vec<Vec<String>> = sources.into_iter().collect();
    let mut out: Vec<String> = Vec::new();
    for layer in layers.into_iter().rev() {
        for item in layer {
            if !out.contains(&item) {
                out.push(item);
            }
        }
    }
    out
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: PASS，7 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/inherit.rs
git commit -m "feat(store): 列表继承 resolve_merge_list,上游在前保序去重"
```

---

## Task 5: PrefsLayer + resolve 组装

**Files:**
- Modify: `crates/mullion-store/src/inherit.rs`

- [ ] **Step 1: 写失败测试**

在 `inherit.rs` 的 `mod tests` 里，紧跟已有的 `use super::*;` 之后追加：

```rust
use crate::group::GroupRecord;
use crate::model::{
    AppearancePrefs, Auth, AuthKind, ColorSpec, ColorTarget, Connection, GroupId, IconKind,
    IconSpec, Identity, Protocol, SessionId, SessionRecord, TerminalPrefs,
};

fn session(tags: Vec<String>, terminal: TerminalPrefs, appearance: AppearancePrefs) -> SessionRecord {
    SessionRecord {
        id: SessionId(1),
        modified_at: "t".into(),
        identity: Identity {
            name: "s".into(),
            note: String::new(),
            group_id: Some(GroupId(1)),
            tags,
        },
        connection: Connection { host: "h".into(), port: 22, protocol: Protocol::Ssh },
        auth: Auth { user: "u".into(), kind: AuthKind::Password },
        terminal,
        appearance,
    }
}

fn group(tags: Vec<String>, terminal: TerminalPrefs, appearance: AppearancePrefs) -> GroupRecord {
    GroupRecord { id: GroupId(1), name: "g".into(), tags, terminal, appearance }
}

#[test]
fn resolve_uses_group_when_session_unset() {
    let s = session(vec![], TerminalPrefs { scrollback: None }, AppearancePrefs::default());
    let g = group(vec![], TerminalPrefs { scrollback: Some(50_000) }, AppearancePrefs::default());
    let got = resolve(&[&s, &g]);
    assert_eq!(got.scrollback, 50_000);
}

#[test]
fn resolve_without_group_falls_back_to_builtin() {
    let s = session(vec![], TerminalPrefs { scrollback: None }, AppearancePrefs::default());
    let got = resolve(&[&s]);
    assert_eq!(got.scrollback, DEFAULT_SCROLLBACK, "未分组会话应取内置默认");
}

#[test]
fn resolve_merges_tags_from_both_layers() {
    let s = session(vec!["web01".into()], TerminalPrefs::default(), AppearancePrefs::default());
    let g = group(vec!["prod".into()], TerminalPrefs::default(), AppearancePrefs::default());
    let got = resolve(&[&s, &g]);
    assert_eq!(got.tags, vec!["prod".to_string(), "web01".to_string()]);
}

/// 钉死设计 §4.1 的硬限制:复合对象整体覆盖,不做字段级合并。
#[test]
fn composite_color_is_replaced_wholesale_never_field_merged() {
    let s = session(
        vec![],
        TerminalPrefs::default(),
        AppearancePrefs {
            icon: None,
            color: Some(ColorSpec { hex: "#111111".into(), apply_to: vec![] }),
        },
    );
    let g = group(
        vec![],
        TerminalPrefs::default(),
        AppearancePrefs {
            icon: Some(IconSpec { kind: IconKind::Builtin, value: "ubuntu".into() }),
            color: Some(ColorSpec {
                hex: "#E5484D".into(),
                apply_to: vec![ColorTarget::Tab, ColorTarget::StatusBar],
            }),
        },
    );
    let got = resolve(&[&s, &g]);

    let color = got.color.expect("应有颜色");
    assert_eq!(color.hex, "#111111", "会话的 hex 胜出");
    assert!(
        color.apply_to.is_empty(),
        "整体覆盖:会话的空 apply_to 必须原样保留,绝不能合并分组的 apply_to"
    );
    // icon 会话未设 → 整体取分组的
    assert_eq!(got.icon.unwrap().value, "ubuntu");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: 编译失败，`cannot find function resolve` / `cannot find trait PrefsLayer`

- [ ] **Step 3: 实现**

在 `inherit.rs` 顶部（模块文档注释之后）加上：

```rust
use crate::model::{AppearancePrefs, TerminalPrefs};
```

并在文件末尾（`mod tests` 之前）追加：

```rust
/// 一层可继承偏好的来源。会话、分组都实现它;
/// 将来 F64「环境等级隐含默认」只需再实现一个类型并塞进 layers,
/// **`resolve` 的签名不变**(设计 §4.1)。
pub trait PrefsLayer {
    fn tags(&self) -> &[String];
    fn terminal(&self) -> &TerminalPrefs;
    fn appearance(&self) -> &AppearancePrefs;
}

impl PrefsLayer for crate::model::SessionRecord {
    fn tags(&self) -> &[String] {
        &self.identity.tags
    }
    fn terminal(&self) -> &TerminalPrefs {
        &self.terminal
    }
    fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }
}

impl PrefsLayer for crate::group::GroupRecord {
    fn tags(&self) -> &[String] {
        &self.tags
    }
    fn terminal(&self) -> &TerminalPrefs {
        &self.terminal
    }
    fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }
}

/// 继承解析后的最终配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub scrollback: u32,
    pub tags: Vec<String>,
    pub icon: Option<crate::model::IconSpec>,
    pub color: Option<crate::model::ColorSpec>,
}

/// 沿 `layers`(优先级从高到低)解析出最终配置。
///
/// 调用方负责组装层序,当前为 `[会话, 分组]`;未分组时只传 `[会话]`。
pub fn resolve(layers: &[&dyn PrefsLayer]) -> ResolvedConfig {
    ResolvedConfig {
        scrollback: resolve_override(
            layers.iter().map(|l| l.terminal().scrollback),
            DEFAULT_SCROLLBACK,
        ),
        tags: resolve_merge_list(layers.iter().map(|l| l.tags().to_vec())),
        icon: resolve_override(layers.iter().map(|l| l.appearance().icon.clone()), None),
        color: resolve_override(layers.iter().map(|l| l.appearance().color.clone()), None),
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib inherit 2>&1 | tail -20`
Expected: PASS，11 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/inherit.rs
git commit -m "feat(store): PrefsLayer 层序抽象 + resolve 组装,复合对象整体覆盖钉死"
```

---

## Task 6: v1 → v2 迁移

**Files:**
- Create: `crates/mullion-store/src/migrate.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

创建 `crates/mullion-store/src/migrate.rs`：

```rust
//! v1(扁平) → v2(分节)的一次性迁移。
//!
//! v1 结构在此**独立定义**并冻结:不能靠 v2 的 `SessionRecord` 去读旧文件,
//! 因为旧结构除 `note` 外均无 `#[serde(default)]`,缺字段即解析失败。

use serde::Deserialize;

use crate::error::StoreError;
use crate::model::{
    AppearancePrefs, Auth, AuthKind, Connection, Identity, Protocol, SessionId, SessionRecord,
    SessionsFile, TerminalPrefs, CURRENT_SCHEMA,
};

/// v1 的一条会话。**冻结,不要再改**。
#[derive(Debug, Deserialize)]
struct V1Record {
    id: SessionId,
    name: String,
    host: String,
    port: u16,
    protocol: Protocol,
    user: String,
    #[serde(default)]
    note: String,
    modified_at: String,
    auth: AuthKind,
}

/// v1 的顶层文件。
#[derive(Debug, Deserialize)]
struct V1File {
    #[serde(default)]
    session: Vec<V1Record>,
}

/// 只探测版本号,不解析其余内容(未知字段被 serde 忽略)。
#[derive(Debug, Deserialize)]
pub struct SchemaProbe {
    #[serde(default = "one")]
    pub schema_version: u32,
}

fn one() -> u32 {
    1
}

/// 把 v1 文本迁移成 v2 结构。分组为空(v1 没有分组概念),
/// 可继承分节全部留 `None` —— 即「继承/未设置」,行为与迁移前一致。
pub fn migrate_v1(text: &str) -> Result<SessionsFile, StoreError> {
    let old: V1File = toml::from_str(text)?;
    let session = old
        .session
        .into_iter()
        .map(|r| SessionRecord {
            id: r.id,
            modified_at: r.modified_at,
            identity: Identity {
                name: r.name,
                note: r.note,
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: r.host,
                port: r.port,
                protocol: r.protocol,
            },
            auth: Auth {
                user: r.user,
                kind: r.auth,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
        })
        .collect();
    Ok(SessionsFile {
        schema_version: CURRENT_SCHEMA,
        group: Vec::new(),
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.1.12 真实写出的格式。
    const V1_TEXT: &str = r#"
[[session]]
id = 7
name = "dev"
host = "192.0.2.10"
port = 22
protocol = "ssh"
user = "user"
note = "跳板后"
modified_at = "2026-07-25T00:00:00Z"

[session.auth]
kind = "public_key"
path = "/path/to/key.pem"
has_passphrase = false
"#;

    #[test]
    fn migrate_preserves_every_v1_field() {
        let out = migrate_v1(V1_TEXT).unwrap();
        assert_eq!(out.schema_version, CURRENT_SCHEMA);
        assert_eq!(out.session.len(), 1);
        let s = &out.session[0];
        assert_eq!(s.id, SessionId(7));
        assert_eq!(s.identity.name, "dev");
        assert_eq!(s.identity.note, "跳板后");
        assert_eq!(s.connection.host, "192.0.2.10");
        assert_eq!(s.connection.port, 22);
        assert_eq!(s.connection.protocol, Protocol::Ssh);
        assert_eq!(s.auth.user, "user");
        assert_eq!(s.modified_at, "2026-07-25T00:00:00Z");
        assert!(matches!(
            &s.auth.kind,
            AuthKind::PublicKey { has_passphrase: false, .. }
        ));
    }

    #[test]
    fn migrated_prefs_are_all_unset_so_behavior_is_unchanged() {
        let out = migrate_v1(V1_TEXT).unwrap();
        let s = &out.session[0];
        assert_eq!(s.terminal, TerminalPrefs::default(), "迁移不得凭空写入偏好值");
        assert_eq!(s.appearance, AppearancePrefs::default());
        assert!(s.identity.group_id.is_none());
        assert!(s.identity.tags.is_empty());
        assert!(out.group.is_empty());
    }

    #[test]
    fn migrated_file_round_trips_as_v2() {
        let out = migrate_v1(V1_TEXT).unwrap();
        let text = toml::to_string_pretty(&out).unwrap();
        let back: SessionsFile = toml::from_str(&text).unwrap();
        assert_eq!(back, out, "迁移产物必须能按 v2 原样读回");
    }

    #[test]
    fn probe_reads_version_without_full_parse() {
        let p: SchemaProbe = toml::from_str(V1_TEXT).unwrap();
        assert_eq!(p.schema_version, 1, "缺键视为 v1");
        let p2: SchemaProbe = toml::from_str("schema_version = 2").unwrap();
        assert_eq!(p2.schema_version, 2);
    }

    #[test]
    fn empty_v1_file_migrates_to_empty_v2() {
        let out = migrate_v1("").unwrap();
        assert!(out.session.is_empty());
        assert_eq!(out.schema_version, CURRENT_SCHEMA);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib migrate 2>&1 | tail -20`
Expected: 失败，`file not found for module migrate`

- [ ] **Step 3: 在 lib.rs 挂载模块**

```rust
pub mod migrate;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store --lib migrate 2>&1 | tail -20`
Expected: PASS，5 个测试通过

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/migrate.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): v1→v2 一次性迁移,旧结构冻结在 migrate 模块"
```

---

## Task 7: Vault 接入迁移与备份

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

在 `vault.rs` 的 `mod tests` 里追加：

```rust
const V1_ON_DISK: &str = r#"
[[session]]
id = 1
name = "old"
host = "h"
port = 22
protocol = "ssh"
user = "u"
note = ""
modified_at = "2026-07-25T00:00:00Z"

[session.auth]
kind = "password"
"#;

#[test]
fn open_migrates_v1_file_and_writes_backup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sessions.toml"), V1_ON_DISK).unwrap();

    let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();

    assert_eq!(vault.list().len(), 1, "迁移后会话应仍在");
    assert_eq!(vault.list()[0].identity.name, "old");
    assert!(
        dir.path().join("sessions.toml.bak").exists(),
        "迁移前必须留备份"
    );
    let bak = std::fs::read_to_string(dir.path().join("sessions.toml.bak")).unwrap();
    assert!(bak.contains("name = \"old\""), "备份应是原始 v1 内容");

    let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
    assert!(now.contains("schema_version = 2"), "磁盘上应已是 v2");
}

#[test]
fn opening_v2_file_does_not_create_backup() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        v.add(draft_pw("a", "p"), "t");
        v.save().unwrap();
    }
    std::fs::remove_file(dir.path().join("sessions.toml.bak")).ok();
    let _v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    assert!(
        !dir.path().join("sessions.toml.bak").exists(),
        "已是 v2 不应重复备份"
    );
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib vault 2>&1 | tail -30`
Expected: 编译失败（`identity` 字段不存在于旧 `SessionDraft` 路径）或迁移断言失败

- [ ] **Step 3: 实现**

`vault.rs` 顶部 import 改为：

```rust
use crate::group::GroupRecord;
use crate::migrate::{migrate_v1, SchemaProbe};
use crate::model::{
    AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, Protocol, SecretEntry,
    SessionId, SessionRecord, SessionsFile, TerminalPrefs, CURRENT_SCHEMA,
};
```

（`Protocol` 只被 `mod tests` 的 `draft_pw` 用到。若 clippy 报 `unused_imports`，
说明本体代码没用它——把它挪进 `mod tests` 的 use 里，不要保留在文件顶部。）

`Vault` 结构体加 `groups` 字段：

```rust
pub struct Vault {
    dir: PathBuf,
    groups: Vec<GroupRecord>,
    sessions: Vec<SessionRecord>,
    secrets: SecretMap,
    key: [u8; 32],
}
```

`SessionDraft` 改为分节形态：

```rust
/// 新建/编辑会话的输入(不含 id/modified_at,由 vault 分配/注入)。
pub struct SessionDraft {
    pub identity: Identity,
    pub connection: Connection,
    pub auth: Auth,
    pub terminal: TerminalPrefs,
    pub appearance: AppearancePrefs,
    /// 敏感部分(密码/口令);无则 None。
    pub secret: Option<SecretEntry>,
}
```

`open()` 的会话读取段替换为：

```rust
let sessions_path = dir.join("sessions.toml");
let mut migrated = false;
let (groups, sessions) = if sessions_path.exists() {
    let text = fs::read_to_string(&sessions_path)?;
    let probe: SchemaProbe = toml::from_str(&text)?;
    if probe.schema_version < CURRENT_SCHEMA {
        // 迁移前留备份:这是唯一会改写用户既有数据的地方。
        fs::copy(&sessions_path, sessions_path.with_extension("toml.bak"))?;
        let file = migrate_v1(&text)?;
        migrated = true;
        (file.group, file.session)
    } else {
        let file: SessionsFile = toml::from_str(&text)?;
        (file.group, file.session)
    }
} else {
    (Vec::new(), Vec::new())
};
```

`open()` 末尾构造 `Self` 后、返回前，插入迁移写回（`save` 的签名是 `&self`，无需 `mut`）：

```rust
let vault = Self { dir, groups, sessions, secrets, key };
if migrated {
    // 立即写回 v2,避免下次打开重复迁移并覆盖掉备份。
    vault.save()?;
}
Ok(vault)
```

`save()` 里构造 `SessionsFile` 改为：

```rust
let file = SessionsFile {
    schema_version: CURRENT_SCHEMA,
    group: self.groups.clone(),
    session: self.sessions.clone(),
};
```

`add()` 的 push 段改为：

```rust
self.sessions.push(SessionRecord {
    id,
    modified_at: now_rfc3339.to_string(),
    identity: draft.identity,
    connection: draft.connection,
    auth: draft.auth,
    terminal: draft.terminal,
    appearance: draft.appearance,
});
```

`update()` 的字段赋值段改为：

```rust
rec.identity = draft.identity;
rec.connection = draft.connection;
rec.auth = draft.auth;
rec.terminal = draft.terminal;
rec.appearance = draft.appearance;
rec.modified_at = now_rfc3339.to_string();
```

测试辅助函数 `draft_pw` 改为：

```rust
fn draft_pw(name: &str, pw: &str) -> SessionDraft {
    SessionDraft {
        identity: Identity {
            name: name.into(),
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
            kind: AuthKind::Password,
        },
        terminal: TerminalPrefs::default(),
        appearance: AppearancePrefs::default(),
        secret: Some(SecretEntry {
            password: Some(pw.into()),
            passphrase: None,
        }),
    }
}
```

已有测试中的字段路径按此规律改：`rec.name` → `rec.identity.name`，
`rec.host` → `rec.connection.host`，`d.host = ...` → `d.connection.host = ...`，
`d.auth = AuthKind::PublicKey{..}` → `d.auth.kind = AuthKind::PublicKey{..}`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -20`
Expected: PASS，含 `f70_no_plaintext` 集成测试

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/vault.rs
git commit -m "feat(store): Vault 接入 v1 迁移与 .bak 备份,SessionDraft 改分节形态"
```

---

## Task 8: 分组 CRUD 与删组置空

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

在 `vault.rs` 的 `mod tests` 里追加：

```rust
#[test]
fn add_group_allocates_incrementing_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let g1 = v.add_group("生产".into());
    let g2 = v.add_group("测试".into());
    assert_eq!(g1, GroupId(1));
    assert_eq!(g2, GroupId(2));
    assert_eq!(v.groups().len(), 2);
}

#[test]
fn delete_group_detaches_sessions_but_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let g = v.add_group("生产".into());
    let mut d = draft_pw("a", "p");
    d.identity.group_id = Some(g);
    let sid = v.add(d, "t");

    v.delete_group(g).unwrap();

    assert!(v.groups().is_empty());
    assert!(v.get(sid).is_some(), "删分组绝不能级联删会话");
    assert!(
        v.get(sid).unwrap().identity.group_id.is_none(),
        "归属该组的会话 group_id 应置 None"
    );
}

#[test]
fn delete_missing_group_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    assert!(v.delete_group(GroupId(99)).is_err());
}

#[test]
fn resolve_for_uses_group_layer_when_attached() {
    use crate::inherit::DEFAULT_SCROLLBACK;
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let g = v.add_group("生产".into());
    v.group_mut(g).unwrap().terminal.scrollback = Some(50_000);
    v.group_mut(g).unwrap().tags.push("prod".into());

    let mut d = draft_pw("a", "p");
    d.identity.group_id = Some(g);
    d.identity.tags.push("web01".into());
    let sid = v.add(d, "t");

    let cfg = v.resolve_for(sid).unwrap();
    assert_eq!(cfg.scrollback, 50_000, "应取分组值");
    assert_eq!(cfg.tags, vec!["prod".to_string(), "web01".to_string()]);

    // 未分组会话回落内置默认
    let sid2 = v.add(draft_pw("b", "p"), "t");
    assert_eq!(v.resolve_for(sid2).unwrap().scrollback, DEFAULT_SCROLLBACK);
}

#[test]
fn groups_survive_save_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let g;
    {
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        g = v.add_group("生产".into());
        v.group_mut(g).unwrap().terminal.scrollback = Some(1234);
        v.save().unwrap();
    }
    let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    assert_eq!(v.groups().len(), 1);
    assert_eq!(v.groups()[0].terminal.scrollback, Some(1234));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p mullion-store --lib vault 2>&1 | tail -20`
Expected: 编译失败，`no method named add_group`

- [ ] **Step 3: 实现**

在 `vault.rs` 的 `impl Vault` 里，`delete()` 之后追加：

```rust
pub fn groups(&self) -> &[GroupRecord] {
    &self.groups
}

pub fn group_mut(&mut self, id: GroupId) -> Option<&mut GroupRecord> {
    self.groups.iter_mut().find(|g| g.id == id)
}

/// 新增分组。id 取现有 max+1(空库从 1 起)。
pub fn add_group(&mut self, name: String) -> GroupId {
    let id = GroupId(self.groups.iter().map(|g| g.id.0).max().map_or(1, |m| m + 1));
    self.groups.push(GroupRecord {
        id,
        name,
        tags: Vec::new(),
        terminal: TerminalPrefs::default(),
        appearance: AppearancePrefs::default(),
    });
    id
}

/// 删除分组。归属该组的会话**不删除**,只把 `group_id` 置 `None`
/// ——分组是组织手段,不是会话的所有者。
pub fn delete_group(&mut self, id: GroupId) -> Result<(), StoreError> {
    let before = self.groups.len();
    self.groups.retain(|g| g.id != id);
    if self.groups.len() == before {
        return Err(StoreError::GroupNotFound(id));
    }
    for s in &mut self.sessions {
        if s.identity.group_id == Some(id) {
            s.identity.group_id = None;
        }
    }
    Ok(())
}

/// 沿 `[会话, 分组]` 层序解析出最终配置。
pub fn resolve_for(&self, id: SessionId) -> Result<crate::inherit::ResolvedConfig, StoreError> {
    let s = self.get(id).ok_or(StoreError::NotFound(id))?;
    let g = s
        .identity
        .group_id
        .and_then(|gid| self.groups.iter().find(|g| g.id == gid));
    Ok(match g {
        Some(g) => crate::inherit::resolve(&[s, g]),
        None => crate::inherit::resolve(&[s]),
    })
}
```

在 `error.rs` 的 `StoreError` 加一个变体：

```rust
    /// 目标分组不存在。
    GroupNotFound(GroupId),
```

`Display` 分支加：

```rust
            StoreError::GroupNotFound(id) => write!(f, "分组不存在:{id:?}"),
```

`error.rs` 顶部 import 改为 `use crate::model::{GroupId, SessionId};`

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -20`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-store/src/error.rs
git commit -m "feat(store): 分组 CRUD + 删组置空 + resolve_for 层序组装 (F60)"
```

---

## Task 9: mullion-app 机械适配

改 `SessionRecord` 必然破坏 app 侧编译。本任务**只做字段路径迁移，不加任何 UI**。

`app.rs` 里有事件循环（T1/T3/T7 三条领域陷阱都在那）。本任务碰它**仅限于**把
`rec.host` 一类的读取改成 `rec.connection.host`——**不许动 `ControlFlow`、
不许动 `PtyWrite` 分发、不许动帧率节流**。改完 Step 4 会点名跑那三条守护测试。

**Files:**
- Modify: `crates/mullion-app/src/shell/session_map.rs`
- Modify: `crates/mullion-app/src/shell/store.rs`
- Modify: `crates/mullion-app/src/ui/session_manager.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 看清破损范围**

Run: `cargo build -p mullion-app 2>&1 | grep -E "^error" | head -40`
Expected: 一批 `no field named name/host/port/protocol/user/note on type SessionRecord`

- [ ] **Step 2: 按映射表逐处替换**

| 旧路径 | 新路径 |
|---|---|
| `rec.name` | `rec.identity.name` |
| `rec.note` | `rec.identity.note` |
| `rec.host` | `rec.connection.host` |
| `rec.port` | `rec.connection.port` |
| `rec.protocol` | `rec.connection.protocol` |
| `rec.user` | `rec.auth.user` |
| `rec.auth`（作 `AuthKind` 用时） | `rec.auth.kind` |

`session_manager.rs` 里 `build_draft()` 构造 `SessionDraft` 的部分，改为分节形态：

```rust
SessionDraft {
    identity: Identity {
        name: buf.name.trim().to_string(),
        note: buf.note.trim().to_string(),
        group_id: None,
        tags: Vec::new(),
    },
    connection: Connection {
        host: buf.host.trim().to_string(),
        port,
        protocol: buf.protocol,
    },
    auth: Auth {
        user: buf.user.trim().to_string(),
        kind,
    },
    terminal: TerminalPrefs::default(),
    appearance: AppearancePrefs::default(),
    secret,
}
```

（`port`、`kind`、`secret` 三个局部变量的既有计算逻辑原样保留，不要改动。）

- [ ] **Step 3: 编译**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 4: 跑 app 侧既有测试**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED"`
Expected: 全部 PASS，特别是 `session_manager` 的 `build_draft` 单测与
`app::tests::reflow_emits_resize`（T4）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src
git commit -m "refactor(app): 适配 SessionRecord 分节结构,仅字段路径迁移"
```

---

## Task 10: 全绿收尾

**Files:** 无新增

- [ ] **Step 1: 跑全量测试**

```bash
cargo test --workspace > /tmp/p0a-test.log 2>&1
grep -nE "test result|FAILED|panicked" /tmp/p0a-test.log
```

Expected: 所有 `test result: ok`，无 FAILED

- [ ] **Step 2: 领域陷阱守护测试点名确认**

```bash
grep -nE "pty_write_is_collected|reflow_emits_resize|shift_blocks_mouse_report|shift_enter_without_kitty" /tmp/p0a-test.log
```

Expected: 四个测试名都出现且均为 ok。本切片不碰仿真与输入，
它们必须原样通过——一旦有红说明改动越界了。

- [ ] **Step 3: clippy 与 fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: 两条命令均无输出

- [ ] **Step 4: 人工确认迁移安全性**

在真实配置目录的副本上验证（**不要直接动原目录**）：

```bash
cp -r ~/.config/mullion /tmp/mullion-migrate-check
```

然后用一次 `cargo run -p mullion-app` 指向该副本目录启动（或直接跑
`cargo test -p mullion-store` 的迁移测试即可），确认：
- `/tmp/mullion-migrate-check/sessions.toml.bak` 生成且内容是原 v1
- 新 `sessions.toml` 含 `schema_version = 2`，会话条数不变

**这一条属于人工验收**——CLAUDE.md「你无法验证的东西」不含它，但真实用户数据的迁移
值得手动过一眼，无头测试只覆盖了构造出来的 fixture。

- [ ] **Step 5: 提交**

```bash
git commit --allow-empty -m "chore: 切片 P0-a 全绿 —— store 分节地基 + 分组 + 迁移 (F60)"
```

---

## 完成标准

- `cargo test --workspace` 全过，`clippy -D warnings` 无输出，`fmt --check` 无输出
- 旧 `sessions.toml` 能自动迁移，`.bak` 留存，会话与凭据零丢失（`f70_no_plaintext` 仍绿）
- `resolve()` 支持任意层数，F64 插入新继承层时**不需要改签名**
- 复合对象「整体覆盖」有单测钉死，P2 做 icon/color UI 时改不动这条语义
- app 侧只有字段路径变化，没有新 UI

## 不在本切片范围

- 代理与跳板（F4/F5）→ P0-b
- 分组的 UI（下拉框、折叠列表）→ P0-b 的最小 UI 部分
- 图标资源管线、色板取值、标签筛选 UI → P2-a
- `automation` / `safety` 分节 → 各自在 P1 / P2-b 引入。
  P0-a 不建空结构体（那是死代码）；新增顶层分节 key 不改已有类型，
  配合 `#[serde(default)]` 旧文件仍可读，不违反 D3。
