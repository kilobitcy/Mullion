# F148 多实例现场历史 —— 实现计划(第一片)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 每个 exe 实例各自记录自己的现场(标签 + 分屏树 + 窗口几何),互不覆盖;保留最近 10 条已关闭的记录;启动时展示列表,由用户选一条恢复。

**Architecture:** `layout.toml` 单文件换成 `layouts\<实例id>.toml` 一实例一文件 —— 写路径永不共享,并发问题从结构上消失。活性靠 `layouts\<实例id>.alive` 的 mtime 心跳(纯函数判活,误判会自愈)。启动时迁移老文件 → 裁剪 → 读列表 → 弹「恢复上次的现场」。**本片恢复仍走现有的标签级占位 `TabContent::Restored`,一行不改 `Workspace` 核心**(pane 级占位是第二片 F149)。

**Tech Stack:** Rust / `mullion-store`(零 UI、零 async、纯同步 IO)/ `mullion-app`(winit 0.30 + egui 0.30)/ `toml` / `time` 0.3。

**设计依据:** `docs/superpowers/specs/2026-08-21-multi-instance-layout-history-design.md`(决策 D1–D19 + 实现细节 X1–X8)。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-store/src/layout.rs` | 单份布局的磁盘格式 | 修改:`SavedLayout` 加 `updated_at` |
| `crates/mullion-store/src/history.rs` | **新** 多实例记录目录:路径、实例 id、心跳、读写、裁剪、迁移 | 创建 |
| `crates/mullion-store/src/lib.rs` | 导出 | 修改:`pub mod history;` + `pub use` |
| `crates/mullion-app/src/ui/history.rs` | **新** 恢复列表弹窗(纯 egui,零 IO) | 创建 |
| `crates/mullion-app/src/ui/mod.rs` | UI 调度 | 修改:`pub mod history;`、`UiState.history`、`UiActions.history`、`build_ui` 里接线 |
| `crates/mullion-app/src/ui/chrome.rs` | 菜单栏 | 修改:「会话」菜单加「恢复上次的现场…」 |
| `crates/mullion-app/src/app.rs` | 接线 | 修改:实例 id、心跳 tick、写新路径、启动流程、`Modal::History`、恢复处置 |
| `spec.md` | 需求登记 | 修改:F148 新增、F37 正文改写 |

**为什么 `history.rs` 是新文件而不是塞进 `layout.rs`**:`layout.rs` 的职责是「**一份**布局怎么编码成 TOML」,它的模块文档、`Loaded` 的「永不失败」契约、树的扁平编码规则全是围绕这一件事写的。「目录里有哪些记录、谁还活着、该删哪几个」是另一件事,混进去会让那份文档同时讲两个层次。

---

### Task 1: `SavedLayout` 带上「最后更新时刻」

一条记录要在列表里排序、要显示「3 小时前」,就得知道它是什么时候写的。文件 mtime 不够用 —— 迁移(Task 6)时要**指定**一个时刻,裁剪测试要造**假**时刻,而 mtime 在临时目录里没法可靠地伪造。

**Files:**
- Modify: `crates/mullion-store/src/layout.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-store/src/layout.rs` 的 `mod tests` 里,把 `sample()` 改成带 `updated_at`,并新增一条测试。先改 `sample()`:

```rust
    fn sample() -> SavedLayout {
        SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 1,
            updated_at: 1_755_000_000,
            window: Some(SavedWindow {
```

(其余字段不动。)然后在 `a_layout_survives_a_round_trip_through_toml` 后面加:

```rust
    /// F148:记录的时刻要能 round-trip。列表按它排序、「3 小时前」按它算 ——
    /// 丢了它,10 条记录的先后顺序就只能靠文件 mtime,而 mtime 会被备份软件、
    /// 同步盘、`touch` 改掉。
    #[test]
    fn the_updated_at_stamp_survives_a_round_trip() {
        let before = sample();
        let text = toml::to_string_pretty(&before).expect("序列化不该失败");
        let after: SavedLayout = toml::from_str(&text).expect("解析不该失败");
        assert_eq!(after.updated_at, 1_755_000_000, "时刻丢了:\n{text}");
    }

    /// 老文件(没有这个字段)读进来是 0,不失败 —— Task 6 的迁移要读的正是
    /// 这种文件。
    #[test]
    fn a_file_without_an_updated_at_reads_back_as_zero() {
        let text = "schema_version = 1\n";
        let got: SavedLayout = toml::from_str(text).expect("缺字段不该解析失败");
        assert_eq!(got.updated_at, 0);
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store layout:: 2>&1 | tail -20`
Expected: 编译失败 —— `struct SavedLayout has no field named updated_at`。

- [ ] **Step 3: 加字段**

在 `crates/mullion-store/src/layout.rs` 的 `SavedLayout` 里,`active_tab` 与 `window` **之间**插入:

```rust
    /// F148:这份记录最后一次写盘的时刻(Unix 秒,UTC)。`0` = 不知道
    /// (v1 老文件迁移过来的、或手改出来的)。
    ///
    /// **必须排在 `window` 之前、`tabs` 之前**:toml 的「值在表之前」规则 ——
    /// 标量字段排在 `[window]` 表和 `[[tab]]` 表数组后面会直接序列化失败。
    #[serde(default)]
    pub updated_at: i64,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED|error\["`
Expected: `test result: ok.`,零 FAILED。

> 注意:`app.rs` 里构造 `SavedLayout` 的地方(`snapshot_layout`)也要补这个字段才编译得过,那是 Task 7。本 Task 只跑 `-p mullion-store`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/layout.rs
git commit -m "feat(store): 布局记录带上最后更新时刻 (F148)"
```

---

### Task 2: 记录目录的路径与实例身份

**Files:**
- Create: `crates/mullion-store/src/history.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 建文件,只写常量 + 路径 + 实例 id + 判活,以及它们的测试**

创建 `crates/mullion-store/src/history.rs`:

```rust
//! F148:**多实例**现场历史的磁盘布局。零 UI、零 async、纯同步 IO。
//!
//! 一个 exe 实例 = `<config_dir>/layouts/<实例id>.toml` 一个文件 +
//! `<config_dir>/layouts/<实例id>.alive` 一个心跳文件。**每个进程只写自己那
//! 两个文件,从不改别人的**。
//!
//! 为什么不是「一个文件里存 10 条数组」(设计 D5):那样每个实例退出时都要
//! 读-改-写同一份文件,两个窗口同时关闭时,后写的那个会拿着旧的 9 条 + 自己,
//! 把先写的那条**整个抹掉** —— 静默且不可恢复。要修就得上文件锁,而那是
//! 新依赖 + 我们验证不了的 Windows 平台行为(设计 D4 已否)。
//!
//! 一实例一文件之后,并发问题从结构上消失:写路径永不共享,唯一的共享操作是
//! 启动时的删除,而**删除的误判后果有界** —— 多删 = 少一条历史,少删 = 目录里
//! 多几个文件,下次启动再删。
//!
//! 本模块**不认识 `mullion-core`**,理由同 `layout.rs`(架构不变量)。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// `StoreError` / `SavedLayout` 到 Task 3/4 才用得上,那时再加 use ——
// 提前写会触发 unused import,而本项目是 `-D warnings`。

/// 记录目录名(在 `config_dir` 下)。
pub const HISTORY_DIR: &str = "layouts";

/// 心跳文件的扩展名。
pub const ALIVE_EXT: &str = "alive";

/// 心跳的写入间隔(秒)。见 [`is_alive`] 的宽限说明。
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// 心跳的宽限期(秒)= 写入间隔的 3 倍。
pub const ALIVE_GRACE_SECS: i64 = 45;

/// 已关闭的记录最多留几条(设计 D5)。**活着的实例不计入这个名额** ——
/// 否则「同时开着 10 个窗口」会把全部历史删光,而用户说的「保留最后 10 条
/// 记录」指的是**已经关掉的现场**。
pub const MAX_RECORDS: usize = 10;

/// 这个实例的身份。**毫秒时间戳 + pid**,零新依赖。
///
/// 同一毫秒同一 pid 不可能撞(pid 在一毫秒内不会被回收再分配给新进程)。
/// 时间戳由调用方传进来而不是这里取 —— 取时刻是 IO 边界上的事,传进来才
/// 测得了「同毫秒不同 pid 不撞」。
///
/// 只含数字和 `-`,直接当文件名安全(不需要转义,也不可能撞上 `..`)。
pub fn new_instance_id(now_ms: u128, pid: u32) -> String {
    format!("{now_ms}-{pid}")
}

/// 记录目录:`<dir>/layouts`。
pub fn history_dir(dir: &Path) -> PathBuf {
    dir.join(HISTORY_DIR)
}

/// 某个实例的记录文件。
pub fn record_path(dir: &Path, id: &str) -> PathBuf {
    history_dir(dir).join(format!("{id}.toml"))
}

/// 某个实例的心跳文件。
pub fn alive_path(dir: &Path, id: &str) -> PathBuf {
    history_dir(dir).join(format!("{id}.{ALIVE_EXT}"))
}

/// 心跳停在 `heartbeat_secs` 的那个实例,在 `now_secs` 这一刻还算活着吗。
///
/// **纯函数**,这是刻意的(设计 D4):活性判定的另外两条路(文件锁 / PID +
/// 进程启动时间)都要平台特定代码,而 Windows 那一半在无头容器里验证不了。
/// 换成一段算术之后,两种误判的后果都有界且**会自愈**:
/// - 判活为死 → 列表里多一条(恢复它 = 克隆一份,无害);
/// - 判死为活 → 隐藏它,但心跳必然过期,**最多隐藏 45 秒**。
///
/// 宽限期取写入间隔的 3 倍:一次写盘失败、一次调度延迟都不该让一个活着的
/// 窗口被判死。
///
/// **未来的心跳算活着**:系统时钟往回跳(NTP 校时、用户改时间)会让
/// `heartbeat > now`,那种情况下判死等于把一个正开着的窗口摆进恢复列表。
pub fn is_alive(now_secs: i64, heartbeat_secs: i64) -> bool {
    heartbeat_secs > now_secs || now_secs - heartbeat_secs <= ALIVE_GRACE_SECS
}

/// 当前的 Unix 秒(UTC)。系统时钟早于 1970 时返回 0。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前的 Unix 毫秒(UTC)。给 [`new_instance_id`] 用。
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_started_in_the_same_millisecond_get_different_ids() {
        assert_ne!(new_instance_id(1_755_000_000_123, 4242), new_instance_id(1_755_000_000_123, 4243));
    }

    #[test]
    fn the_same_process_started_later_gets_a_different_id() {
        assert_ne!(new_instance_id(1_755_000_000_123, 4242), new_instance_id(1_755_000_000_124, 4242));
    }

    /// 实例 id 直接当文件名用,所以里面**不能**出现路径分隔符或 `..` ——
    /// 否则一个被手改过的 id 就能让 `record_path` 指到目录外面去。
    #[test]
    fn an_instance_id_is_safe_to_use_as_a_file_name() {
        let id = new_instance_id(1_755_000_000_123, 4242);
        assert!(
            id.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "实例 id 里出现了非数字非连字符:{id}"
        );
    }

    #[test]
    fn the_record_and_heartbeat_live_side_by_side_under_the_history_dir() {
        let dir = Path::new("/cfg");
        assert_eq!(record_path(dir, "7-1"), Path::new("/cfg/layouts/7-1.toml"));
        assert_eq!(alive_path(dir, "7-1"), Path::new("/cfg/layouts/7-1.alive"));
    }

    /// 刚跳过一次心跳的实例还活着 —— 宽限期就是给它的。
    #[test]
    fn an_instance_that_missed_one_heartbeat_is_still_alive() {
        assert!(is_alive(1000, 1000 - 20), "跳了一次心跳就被判死,宽限期形同虚设");
    }

    /// 过了宽限期就算死了。
    ///
    /// 自证会变红:把 `is_alive` 的 `<=` 改成 `<` 之外的任何放宽写法,
    /// 或把 `ALIVE_GRACE_SECS` 调大。
    #[test]
    fn an_instance_silent_past_the_grace_period_is_considered_gone() {
        assert!(is_alive(1000, 1000 - ALIVE_GRACE_SECS), "正好卡在宽限期上该算活着");
        assert!(!is_alive(1000, 1000 - ALIVE_GRACE_SECS - 1), "过了宽限期还判活 —— 那条记录会被永久隐藏");
    }

    /// 时钟往回跳(NTP 校时)时,未来的心跳**必须**算活着 —— 判死等于把一个
    /// 正开着的窗口摆进恢复列表,用户恢复它就克隆出一个重复的现场。
    ///
    /// 自证会变红:删掉 `is_alive` 里的 `heartbeat_secs > now_secs ||`。
    #[test]
    fn a_heartbeat_from_the_future_still_counts_as_alive() {
        assert!(is_alive(1000, 5000));
    }
}
```

- [ ] **Step 2: 挂进 lib.rs**

`crates/mullion-store/src/lib.rs`,在 `pub mod group;` 之后插入(保持字母序):

```rust
pub mod history;
```

并在 `pub use group::GroupRecord;` 之后插入:

```rust
pub use history::{
    alive_path, history_dir, is_alive, new_instance_id, record_path, ALIVE_GRACE_SECS,
    HEARTBEAT_INTERVAL_SECS, HISTORY_DIR, MAX_RECORDS,
};
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-store history:: 2>&1 | grep -E "test result|FAILED|error\["`
Expected: `test result: ok. 7 passed`。

- [ ] **Step 4: clippy**

Run: `cargo clippy -p mullion-store --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无输出(或只有 `Finished`)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/history.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 现场历史的路径、实例身份与心跳判活 (F148)"
```

---

### Task 3: 心跳文件的写与读

**Files:**
- Modify: `crates/mullion-store/src/history.rs`

- [ ] **Step 1: 写失败的测试**

在 `history.rs` 的 `mod tests` 末尾追加:

```rust
    /// 心跳写下去、读回来,时刻对得上(容 2 秒抖动 —— 我们记的是**写盘那一刻
    /// 的墙钟**,不是文件系统的 mtime 精度)。
    #[test]
    fn a_heartbeat_can_be_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        touch_alive(dir.path(), "7-1", 1_755_000_000).unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), Some(1_755_000_000));
    }

    /// 没写过心跳的实例读回 `None` —— 调用方据此把它当成死的
    /// (见 `load_all`:`None` 一律判死)。
    #[test]
    fn a_missing_heartbeat_reads_back_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_heartbeat(dir.path(), "nobody"), None);
    }

    /// 心跳文件被手改成垃圾 → `None`(判死),**不是报错**。心跳不是用户资产,
    /// 它坏了的正确表现是「那条记录出现在列表里」,不是「启动失败」。
    #[test]
    fn a_corrupt_heartbeat_reads_back_as_none_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(history_dir(dir.path())).unwrap();
        std::fs::write(alive_path(dir.path(), "7-1"), b"\x00 not a number").unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), None);
    }

    /// 重复 touch 就是覆盖,不是追加 —— 追加的话文件会随运行时长无限增长,
    /// 而它每 15 秒写一次。
    #[test]
    fn touching_twice_overwrites_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        touch_alive(dir.path(), "7-1", 1000).unwrap();
        touch_alive(dir.path(), "7-1", 2000).unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), Some(2000));
        let len = std::fs::metadata(alive_path(dir.path(), "7-1")).unwrap().len();
        assert!(len < 32, "心跳文件在增长({len} 字节)—— 它每 15 秒写一次");
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store history:: 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function touch_alive` / `read_heartbeat`。

- [ ] **Step 3: 实现**

在 `history.rs` 的 `now_ms()` 之后、`mod tests` 之前插入(并把 `use crate::error::StoreError;` 加回文件顶部的 use 区):

```rust
/// 写一次心跳:把 `now` 这个 Unix 秒写进 `<dir>/layouts/<id>.alive`。
///
/// **不是 `write_atomic`**:心跳只有一行数字,写一半的后果是下一次读回 `None`
/// (判死),而心跳每 15 秒就再写一次 —— 为它多付一次 rename 不值得。记录文件
/// 那边才需要原子写(那里写一半会丢掉整个现场)。
///
/// **必须无条件调用**,不能搭布局落盘的顺风车:`App::flush_layout_if_due` 是
/// 「布局没变就不写盘」,心跳跟着它走的话,一个开着不动的窗口会永远不写心跳、
/// 被别的实例判成死的(设计 D4 的实现约束)。
pub fn touch_alive(dir: &Path, id: &str, now: i64) -> Result<(), StoreError> {
    std::fs::create_dir_all(history_dir(dir))?;
    std::fs::write(alive_path(dir, id), now.to_string().as_bytes())?;
    Ok(())
}

/// 读某个实例最后一次心跳的时刻。`None` = 没写过 / 读不出来 / 内容不是数字,
/// 三者一律当**死了**处置(见模块文档:误判的后果有界且会自愈)。
pub fn read_heartbeat(dir: &Path, id: &str) -> Option<i64> {
    let text = std::fs::read_to_string(alive_path(dir, id)).ok()?;
    text.trim().parse::<i64>().ok()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store history:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 11 passed`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/history.rs
git commit -m "feat(store): 心跳文件的写与读 (F148)"
```

---

### Task 4: 记录的写与列举

**Files:**
- Modify: `crates/mullion-store/src/history.rs`

- [ ] **Step 1: 写失败的测试**

在 `history.rs` 的 `mod tests` 顶部(紧跟 `use super::*;`)加一个脚手架,并在末尾追加测试:

```rust
    use crate::layout::{SavedNodeEntry, SavedTab, SavedTabKind};
    use crate::model::SessionId;

    /// 一份最小的可用布局:一个终端标签、一个叶子。
    fn layout_at(updated_at: i64, title: &str) -> SavedLayout {
        SavedLayout {
            schema_version: crate::layout::CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(1),
                title: title.into(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf()],
            }],
        }
    }
```

```rust
    #[test]
    fn a_record_can_be_saved_and_listed_back() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "7-1", &layout_at(1000, "prod")).unwrap();
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "7-1");
        assert_eq!(got[0].layout.tabs[0].title, "prod");
    }

    /// 目录不存在 = 空列表,**不报错**。首次运行走的就是这条。
    #[test]
    fn a_missing_history_dir_lists_nothing_without_complaining() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 一个坏文件**不能**拖垮整份列表 —— 另外那几条现场是好的,用户要的是
    /// 它们照常出现。这是 `layout::load` 那条「布局不是用户资产」的同一姿态,
    /// 只是从「整份」下沉到了「每条」。
    ///
    /// 自证会变红:把 `list_records` 里解析失败那条 `continue` 改成 `return`。
    #[test]
    fn one_corrupt_record_does_not_take_the_whole_list_down() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "7-1", &layout_at(1000, "good")).unwrap();
        std::fs::write(record_path(dir.path(), "7-2"), b"\x00\xff not toml [[[").unwrap();
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 1, "坏记录把好记录也带走了");
        assert_eq!(got[0].id, "7-1");
    }

    /// 列表按 `updated_at` **倒序** —— 最近的现场在最上面。
    #[test]
    fn records_come_back_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "a", &layout_at(1000, "old")).unwrap();
        save_record(dir.path(), "b", &layout_at(3000, "new")).unwrap();
        save_record(dir.path(), "c", &layout_at(2000, "mid")).unwrap();
        let ids: Vec<_> = list_records(dir.path(), 9999).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    /// 心跳新鲜的记录标成 `alive` —— 调用方据此把它排除在恢复列表之外
    /// (设计 D3:正在用的现场没必要出现在别人的列表里)。
    #[test]
    fn a_record_with_a_fresh_heartbeat_is_marked_alive() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "live", &layout_at(1000, "x")).unwrap();
        save_record(dir.path(), "dead", &layout_at(1000, "y")).unwrap();
        touch_alive(dir.path(), "live", 10_000).unwrap();
        touch_alive(dir.path(), "dead", 10_000 - ALIVE_GRACE_SECS - 1).unwrap();
        let got = list_records(dir.path(), 10_000);
        let live = got.iter().find(|e| e.id == "live").unwrap();
        let dead = got.iter().find(|e| e.id == "dead").unwrap();
        assert!(live.alive, "心跳新鲜却被判死 —— 那个窗口的现场会被别人克隆走");
        assert!(!dead.alive, "心跳早停了还判活 —— 那条记录会被永久隐藏");
    }

    /// `.alive` / `.tmp` 这些非 `.toml` 文件**不是记录**,列举时跳过。
    /// (`.tmp` 是 `write_atomic` 崩在 rename 之前留下的残骸。)
    #[test]
    fn only_toml_files_count_as_records() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "7-1", &layout_at(1000, "real")).unwrap();
        touch_alive(dir.path(), "7-1", 1000).unwrap();
        std::fs::write(history_dir(dir.path()).join("7-9.tmp"), b"junk").unwrap();
        assert_eq!(list_records(dir.path(), 9999).len(), 1);
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store history:: 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function save_record` / `list_records` / `cannot find type HistoryEntry`。

- [ ] **Step 3: 实现**

在 `history.rs` 里(`read_heartbeat` 之后)插入,并把 `use crate::layout::SavedLayout;` 加回顶部 use 区:

```rust
/// 列表里的一条。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// 实例 id(= 文件名去掉 `.toml`)。恢复时用它认领槽位(设计 D12)。
    pub id: String,
    pub layout: SavedLayout,
    /// 它的主人此刻还活着吗(判据见 [`is_alive`])。**只是给调用方过滤用**,
    /// 判错不影响能不能恢复。
    pub alive: bool,
}

/// 原子写一条记录。
///
/// **这里要 `write_atomic`**(与心跳相反):写一半的记录会让下次启动少一条
/// 现场,而这个文件每 2 秒才写一次、承载的是用户拖出来的分屏比例。
pub fn save_record(dir: &Path, id: &str, layout: &SavedLayout) -> Result<(), StoreError> {
    let hdir = history_dir(dir);
    std::fs::create_dir_all(&hdir)?;
    let mut out = layout.clone();
    out.schema_version = crate::layout::CURRENT_LAYOUT_SCHEMA;
    let text = toml::to_string_pretty(&out)?;
    crate::vault::write_atomic(&record_path(dir, id), text.as_bytes())
}

/// 删一条记录(连心跳一起)。找不到文件**不算错** —— 调用方多半正在清理一个
/// 本来就不存在的槽位(设计 D12:恢复时删掉本实例还没写出来的那个文件)。
pub fn remove_record(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(record_path(dir, id));
    let _ = std::fs::remove_file(alive_path(dir, id));
}

/// 列举目录里全部记录,**按 `updated_at` 倒序**(最近的在前)。
///
/// **没有 `Result`,这是刻意的**(同 `layout::Loaded` 的理由):历史不是用户
/// 资产,读不出来的正确表现是「这条不在列表里」,不是「打不开客户端」。
/// 单条记录解析失败只跳过那一条 —— 另外几条现场是好的,用户要的是它们照常出现。
///
/// `now_secs` 由调用方传:活性判定要它,而取时刻是 IO 边界上的事,传进来才
/// 测得了「心跳早停的那条被判死」。
pub fn list_records(dir: &Path, now_secs: i64) -> Vec<HistoryEntry> {
    let hdir = history_dir(dir);
    let Ok(rd) = std::fs::read_dir(&hdir) else {
        // 目录不存在 = 首次运行,正常情况,不记日志。
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        // `.alive` 心跳、`write_atomic` 崩在 rename 之前留下的 `.tmp` 残骸,
        // 都不是记录。
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(layout) = toml::from_str::<SavedLayout>(&text) else {
            continue;
        };
        // 更新版本写出来的记录:**不猜**(同 `layout::load`)。字段可能改了
        // 含义,按现版本读出来的现场是错的,而错的现场比没有现场更难排查。
        if layout.schema_version > crate::layout::CURRENT_LAYOUT_SCHEMA {
            continue;
        }
        let alive = read_heartbeat(dir, id).is_some_and(|h| is_alive(now_secs, h));
        out.push(HistoryEntry {
            id: id.to_string(),
            layout,
            alive,
        });
    }
    // 倒序:最近的现场在最上面。`updated_at` 相同时按 id 倒序,让顺序**确定**
    // —— 不定的顺序会让列表在两次启动之间莫名其妙地换位置。
    out.sort_by(|a, b| {
        b.layout
            .updated_at
            .cmp(&a.layout.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    out
}
```

同时把 `HistoryEntry`、`save_record`、`remove_record`、`list_records`、`touch_alive`、`read_heartbeat`、`now_secs`、`now_ms` 加进 `lib.rs` 的 `pub use history::{...}`:

```rust
pub use history::{
    alive_path, history_dir, is_alive, list_records, new_instance_id, now_ms, now_secs,
    read_heartbeat, record_path, remove_record, save_record, touch_alive, HistoryEntry,
    ALIVE_GRACE_SECS, HEARTBEAT_INTERVAL_SECS, HISTORY_DIR, MAX_RECORDS,
};
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store history:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 17 passed`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/history.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 现场记录的原子写与列举 (F148)"
```

---

### Task 5: 裁剪 —— 只留最近 10 条已关闭的

**Files:**
- Modify: `crates/mullion-store/src/history.rs`

- [ ] **Step 1: 写失败的测试**

在 `mod tests` 末尾追加:

```rust
    /// 造 n 条条目,`alive` 按 `alive_ids` 决定,`updated_at` 递增(所以列表
    /// 倒序后 `id` 越大越靠前)。
    fn entries(n: usize, alive_ids: &[usize]) -> Vec<HistoryEntry> {
        let mut v: Vec<_> = (0..n)
            .map(|i| HistoryEntry {
                id: format!("{i}"),
                layout: layout_at(1000 + i as i64, "x"),
                alive: alive_ids.contains(&i),
            })
            .collect();
        v.sort_by(|a, b| b.layout.updated_at.cmp(&a.layout.updated_at));
        v
    }

    /// 不到上限就一个都不删。
    #[test]
    fn nothing_is_dropped_while_under_the_cap() {
        assert!(plan_prune(&entries(MAX_RECORDS, &[])).is_empty());
    }

    /// 超了就删**最老的那几条**,不是最新的。
    ///
    /// 自证会变红:把 `plan_prune` 里的 `.skip(MAX_RECORDS)` 改成 `.take(..)`。
    #[test]
    fn the_oldest_records_are_the_ones_dropped() {
        let doomed = plan_prune(&entries(MAX_RECORDS + 3, &[]));
        assert_eq!(doomed, vec!["2".to_string(), "1".to_string(), "0".to_string()]);
    }

    /// **活着的一条都不删**,而且它们**不占名额** —— 否则「同时开着 10 个
    /// 窗口」会把全部历史删光,而用户要的「最后 10 条记录」指的是已经关掉的现场。
    ///
    /// 自证会变红:把 `plan_prune` 里的 `.filter(|e| !e.alive)` 删掉。
    #[test]
    fn live_instances_are_never_pruned_and_do_not_eat_the_quota() {
        // 12 条,其中最新的 5 条活着 → 死的只有 7 条,一条都不该删。
        let all_alive_newest: Vec<usize> = (7..12).collect();
        let doomed = plan_prune(&entries(12, &all_alive_newest));
        assert!(doomed.is_empty(), "活着的占了名额,把好好的历史删了:{doomed:?}");
    }

    /// 活着的不占名额,但死的仍然按 10 条封顶。
    #[test]
    fn dead_records_are_still_capped_when_live_ones_are_around() {
        // 14 条,最新的 2 条活着 → 死的 12 条,该删掉最老的 2 条。
        let doomed = plan_prune(&entries(14, &[12, 13]));
        assert_eq!(doomed, vec!["1".to_string(), "0".to_string()]);
    }

    /// 端到端:裁剪真的把文件(连心跳一起)从盘上删掉。
    #[test]
    fn pruning_actually_removes_the_files_and_their_heartbeats() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_RECORDS + 2) {
            save_record(dir.path(), &format!("{i}"), &layout_at(1000 + i as i64, "x")).unwrap();
            touch_alive(dir.path(), &format!("{i}"), 0).unwrap(); // 全是老心跳 = 全死
        }
        let dropped = prune(dir.path(), 9999);
        assert_eq!(dropped, 2);
        assert!(!record_path(dir.path(), "0").exists(), "记录文件没删掉");
        assert!(!alive_path(dir.path(), "0").exists(), "心跳文件没跟着删 —— 目录会越攒越多");
        assert!(record_path(dir.path(), "11").exists(), "最新的那条被误删了");
        assert_eq!(list_records(dir.path(), 9999).len(), MAX_RECORDS);
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store history:: 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function plan_prune` / `prune`。

- [ ] **Step 3: 实现**

在 `history.rs` 的 `list_records` 之后插入:

```rust
/// 哪几条该删。输入必须是 [`list_records`] 的输出(已按 `updated_at` 倒序)。
///
/// **纯函数**:「该删哪几条」是这一片唯一一处会**永久丢用户数据**的判断,
/// 把它跟「怎么删文件」分开,才能对着一堆构造出来的条目把边界钉死。
///
/// 规则(设计 D5):活着的一概不删、且**不占名额**;死的里面留最新
/// [`MAX_RECORDS`] 条,其余按从旧到新的顺序删。
pub fn plan_prune(entries: &[HistoryEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !e.alive)
        .skip(MAX_RECORDS)
        .map(|e| e.id.clone())
        .collect()
}

/// 裁剪一次,返回删了几条。**只在启动时调**(设计 X6):关窗口时也裁的话,
/// 「读整个目录」就从一次性动作变成了每个实例退出都要做的共享操作。
pub fn prune(dir: &Path, now_secs: i64) -> usize {
    let doomed = plan_prune(&list_records(dir, now_secs));
    for id in &doomed {
        remove_record(dir, id);
    }
    doomed.len()
}
```

`lib.rs` 的 `pub use` 里加上 `plan_prune, prune,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store history:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 22 passed`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/history.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 现场历史裁剪,活着的不删也不占名额 (F148)"
```

---

### Task 6: 迁移老的 `layout.toml`

**Files:**
- Modify: `crates/mullion-store/src/history.rs`

- [ ] **Step 1: 写失败的测试**

在 `mod tests` 末尾追加:

```rust
    /// 老的 `layout.toml` 变成一条记录,原文件被删掉(设计 D14)。
    #[test]
    fn the_legacy_layout_file_becomes_a_record_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &layout_at(0, "从前的现场")).unwrap();
        let id = migrate_legacy(dir.path(), "legacy-1", 1_755_000_000);
        assert_eq!(id, Some("legacy-1".to_string()));
        assert!(
            !dir.path().join(crate::layout::LAYOUT_FILE).exists(),
            "老文件没删 —— 每次启动都会再迁移一遍,历史里会长出一堆重复"
        );
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].layout.tabs[0].title, "从前的现场");
    }

    /// 老文件没有 `updated_at`(v1 格式),迁移时补上调用方给的时刻 ——
    /// 留 0 的话它会永远排在列表最底下、并且第一个被裁掉。
    ///
    /// 自证会变红:把 `migrate_legacy` 里 `layout.updated_at = now;` 删掉。
    #[test]
    fn a_migrated_record_gets_a_timestamp_instead_of_staying_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &layout_at(0, "x")).unwrap();
        migrate_legacy(dir.path(), "legacy-1", 1_755_000_000);
        assert_eq!(list_records(dir.path(), 9999)[0].layout.updated_at, 1_755_000_000);
    }

    /// 没有老文件 = 什么都不做,**不报错**。升级之后的每一次启动走的都是这条。
    #[test]
    fn migrating_without_a_legacy_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 老文件是空布局(一个标签都没有)→ 不值得迁,但**照样删掉**它,
    /// 免得每次启动都来读一遍。
    #[test]
    fn an_empty_legacy_file_is_deleted_without_creating_a_record() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &SavedLayout::empty()).unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(!dir.path().join(crate::layout::LAYOUT_FILE).exists());
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 老文件坏了 → 删掉、不迁,**不阻断启动**(同 `layout::load` 的姿态)。
    #[test]
    fn a_corrupt_legacy_file_is_dropped_without_blocking_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join(crate::layout::LAYOUT_FILE), b"\x00 not toml [[[").unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(!dir.path().join(crate::layout::LAYOUT_FILE).exists());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store history:: 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function migrate_legacy`。

- [ ] **Step 3: 实现**

在 `history.rs` 的 `prune` 之后插入:

```rust
/// 把 v1 时代的单份 `layout.toml` 迁成一条记录,然后**删掉老文件**(设计 D14)。
///
/// 返回 `Some(id)` = 真的迁了一条;`None` = 没有老文件 / 老文件是空的 / 老文件
/// 坏了。**这三种情况都会把老文件删掉** —— 留着的话每次启动都要来读一遍,
/// 而它永远不会再被写。
///
/// 这是**单向**升级:exe 降回旧版本会一条标签都不恢复。可接受 —— 布局不是
/// 用户资产,丢了只是回到空标签栏(`layout.rs` 开篇就是这么写的)。
pub fn migrate_legacy(dir: &Path, id: &str, now: i64) -> Option<String> {
    let legacy = dir.join(crate::layout::LAYOUT_FILE);
    if !legacy.exists() {
        return None;
    }
    // `layout::load` 永不失败:坏文件读回来是空布局 + 一句 note。
    let mut layout = crate::layout::load(dir).layout;
    let _ = std::fs::remove_file(&legacy);
    if layout.tabs.is_empty() {
        return None;
    }
    // v1 没有这个字段,读回来是 0。留 0 的话这条记录会永远排在列表最底下,
    // 而且是第一个被裁掉的 —— 而它恰恰是用户升级前最后一次的现场。
    layout.updated_at = now;
    match save_record(dir, id, &layout) {
        Ok(()) => Some(id.to_string()),
        Err(_) => None,
    }
}
```

`lib.rs` 的 `pub use` 里加上 `migrate_legacy,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED"`
Expected: 全 ok,零 FAILED。

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy -p mullion-store --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-store/src/history.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 迁移 v1 的 layout.toml 成一条记录 (F148)"
```

---

### Task 7: app 接线 —— 实例身份、写新路径、心跳

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 加字段与常量**

在 `App` 结构体里,`last_saved_layout` 字段**之前**插入:

```rust
    /// F148:本实例在历史目录里的身份。**恢复一条记录时会被换成那条记录的
    /// id**(设计 D12 接管槽位)—— 所以它不是 `const`,也不能是启动时算完就
    /// 不再变的东西。
    instance_id: String,
    /// F148:上一次写心跳的时刻。
    ///
    /// **与 `layout_checked_at` 分开**:布局落盘是「不脏就不写」,心跳必须
    /// **无条件**写 —— 搭它的顺风车的话,一个开着不动的窗口永远不写心跳,
    /// 会被别的实例判成死的,于是它正用着的现场出现在别人的恢复列表里
    /// (设计 D4 的实现约束)。
    heartbeat_at: Instant,
```

在 `App::new` 的初始化里,`last_saved_layout: None,` **之前**插入:

```rust
            instance_id: mullion_store::new_instance_id(
                mullion_store::now_ms(),
                std::process::id(),
            ),
            // 减去一整个间隔:第一次 `about_to_wait` 就该写下心跳,而不是
            // 等 15 秒 —— 那 15 秒里别的实例会把本进程判成死的。
            heartbeat_at: Instant::now() - Duration::from_secs(mullion_store::HEARTBEAT_INTERVAL_SECS),
```

> 如果 `Duration` 还没 import,在文件顶部 use 区补 `use std::time::Duration;`(先 grep 确认:`grep -n "^use std::time" crates/mullion-app/src/app.rs`)。

- [ ] **Step 2: 写失败的接线守护测试**

在 `app.rs` 的 `mod tests` 里,紧挨着既有的 `about_to_wait_flushes_the_layout_periodically`(搜 `about_to_wait 不再定期落盘`)加:

```rust
    /// **接线守护 / F148**:心跳必须挂在 `about_to_wait` 上,而且**不能**
    /// 走 `flush_layout_if_due` 那条「不脏就不写」的路。
    ///
    /// 漏了的症状极其隐蔽:一个开着不动的窗口(布局没变 → 不落盘 → 不写心跳)
    /// 会被别的实例判成死的,于是它**正在用**的现场出现在别人的恢复列表里,
    /// 被恢复出来就是两个窗口抢同一个槽位(设计 D4)。
    ///
    /// **扎的是源码结构**:真正验它要一个完整的 `App` + `EventLoopProxy`,
    /// 容器里造不出来。验证边界:挡得住「整个调用被删/挪走」,挡不住
    /// 「函数体被掏空」。
    ///
    /// 自证会变红:删掉 `about_to_wait` 里那句 `self.tick_heartbeat();`。
    #[test]
    fn about_to_wait_writes_the_heartbeat() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn about_to_wait(")
            .nth(1)
            .expect("找不到 about_to_wait 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 about_to_wait 的函数结尾")];
        assert!(
            body.contains("self.tick_heartbeat();"),
            "about_to_wait 不写心跳 —— 开着不动的窗口会被别人判死,现场被克隆走"
        );
    }

    /// **接线守护 / F148**:心跳**不许**跟布局落盘共用节流窗口。
    ///
    /// 自证会变红:把 `tick_heartbeat` 的函数体改成
    /// `self.save_layout_if_changed()` 那种「先比对再写」的形状。
    #[test]
    fn the_heartbeat_is_written_unconditionally_not_only_when_the_layout_changed() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn tick_heartbeat(")
            .nth(1)
            .expect("找不到 tick_heartbeat");
        let body = &after[..after.find("\n    }\n").expect("找不到 tick_heartbeat 的结尾")];
        assert!(
            !body.contains("last_saved_layout"),
            "心跳搭上了布局落盘的「不脏就不写」—— 开着不动的窗口永远不写心跳"
        );
        assert!(
            body.contains("touch_alive("),
            "tick_heartbeat 没真的写心跳文件"
        );
    }
```

- [ ] **Step 3: 跑测试确认它失败**

Run: `cargo test -p mullion-app about_to_wait_writes_the_heartbeat 2>&1 | tail -10`
Expected: FAILED —— `about_to_wait 不写心跳`(或 `找不到 tick_heartbeat`)。

- [ ] **Step 4: 实现**

**4a.** `snapshot_layout` 补上时刻字段 —— 找到 `fn snapshot_layout`,把返回的结构体改成:

```rust
        mullion_store::SavedLayout {
            schema_version: mullion_store::CURRENT_LAYOUT_SCHEMA,
            active_tab,
            // F148:**这里恒填 0**。时刻由 `save_layout_if_changed` 在确定
            // 要写盘之后才盖上 —— 在这里盖的话,每次现算的快照都带着不同的
            // 时刻,`last_saved_layout` 的逐字段比对就永远不相等,于是空闲时
            // 每 2 秒也会写一次盘(E7 那条「不脏就一定不写」当场作废,
            // 笔记本硬盘永远睡不下去)。
            updated_at: 0,
            window: self.window_geometry(),
            tabs,
        }
```

**4b.** 改写 `save_layout_if_changed`:

```rust
    /// 现算一份快照,跟上次写盘的那份不同才写。
    ///
    /// 写盘失败**只记日志**,不弹错误卡片:布局是「上次的场景」,不是用户
    /// 资产(设计 E1),为它打断用户不成比例。
    ///
    /// F148:写的是**本实例的槽位** `layouts/<instance_id>.toml`,不再是共享的
    /// `layout.toml` —— 多开时两个进程每 2 秒轮流覆盖同一个文件,最后关的赢
    /// (那正是这一片要修的第一件事)。
    fn save_layout_if_changed(&mut self) {
        let now = self.snapshot_layout();
        if self.last_saved_layout.as_ref() == Some(&now) {
            return;
        }
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        // 时刻在**确定要写**之后才盖:盖在 `snapshot_layout` 里的话,上面那句
        // 比对永远不相等(见那里的注释)。
        let mut out = now.clone();
        out.updated_at = mullion_store::now_secs();
        match mullion_store::save_record(&dir, &self.instance_id, &out) {
            // 记 `now`(时刻为 0 的那份)而不是 `out` —— 下一次比对拿到的
            // 也是时刻为 0 的新快照,两者才可比。
            Ok(()) => self.last_saved_layout = Some(now),
            Err(e) => log::debug!(target: "mullion", "现场落盘失败: {e}"),
        }
    }

    /// F148:到点就写一次心跳。**无条件**,不看布局脏不脏 —— 见
    /// `heartbeat_at` 字段的说明。
    fn tick_heartbeat(&mut self) {
        if self.heartbeat_at.elapsed().as_secs() < mullion_store::HEARTBEAT_INTERVAL_SECS {
            return;
        }
        self.heartbeat_at = Instant::now();
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        if let Err(e) = mullion_store::touch_alive(&dir, &self.instance_id, mullion_store::now_secs())
        {
            log::debug!(target: "mullion", "心跳写入失败: {e}");
        }
    }
```

**4c.** `about_to_wait` 里,在 `self.flush_layout_if_due();` 之后插入:

```rust
        // F148:心跳。**与落盘分开的一次独立写**,理由见 `heartbeat_at`:
        // 布局没变时不落盘,心跳却必须照写,否则开着不动的窗口会被别的实例
        // 判成死的。
        self.tick_heartbeat();
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 > /tmp/t7.log; grep -nE "test result|FAILED|panicked" /tmp/t7.log`
Expected: `test result: ok.`,零 FAILED。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 现场写进本实例的槽位并独立打心跳 (F148)"
```

正文里写明:触到 T3(每帧路径不新增 IO —— 心跳走 `about_to_wait` 的空闲路径,15 秒一次),跑的是 `about_to_wait_writes_the_heartbeat` 与 `the_heartbeat_is_written_unconditionally_not_only_when_the_layout_changed`。

---

### Task 8: app 接线 —— 启动不再自动恢复,改为读列表

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `app.rs` 的 `mod tests` 里,紧挨着 `the_restored_window_geometry_is_clamped_to_the_real_monitors` 加:

```rust
    /// **接线守护 / F148 D1**:启动**不再**自动摆回上次的标签。
    ///
    /// 多开成为常态之后,「最近一条」是哪个窗口关出来的完全不可预测 ——
    /// 自动摆回一个随机窗口的布局比不摆更困惑。摆什么由用户在恢复列表里选。
    ///
    /// 自证会变红:把 `finish_store_open` 里那句 `self.restore_tabs(..)` 加回来。
    #[test]
    fn startup_no_longer_restores_tabs_behind_the_users_back() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn finish_store_open(")
            .nth(1)
            .expect("找不到 finish_store_open");
        let body = &after[..after.find("\n    }\n").expect("找不到 finish_store_open 的结尾")];
        assert!(
            !body.contains("self.restore_tabs("),
            "启动仍在自动摆回标签 —— 多开时摆的是哪个窗口的现场完全不可预测(D1)"
        );
    }

    /// **接线守护 / F148 D14**:启动时必须迁移老的 `layout.toml`。
    ///
    /// 漏了的话,升级那一次用户正开着的现场直接消失,而且老文件会永远躺在
    /// 那儿不被任何人读。
    ///
    /// 自证会变红:删掉 `resumed` 里那句 `mullion_store::migrate_legacy(`。
    #[test]
    fn startup_migrates_the_legacy_layout_file() {
        let src = include_str!("app.rs");
        let after = src.split("fn resumed(").nth(1).expect("找不到 resumed");
        assert!(
            after.contains("mullion_store::migrate_legacy("),
            "启动不迁移老的 layout.toml —— 升级那次的现场会直接消失(D14)"
        );
    }

    /// **接线守护 / F148 D5/X6**:裁剪只在启动时做一次。
    ///
    /// 自证会变红:删掉 `resumed` 里那句 `mullion_store::prune(`。
    #[test]
    fn startup_prunes_the_history_once() {
        let src = include_str!("app.rs");
        let after = src.split("fn resumed(").nth(1).expect("找不到 resumed");
        assert!(
            after.contains("mullion_store::prune("),
            "启动不裁剪历史 —— layouts 目录会无限增长"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app startup_ 2>&1 | grep -E "test result|FAILED|assert"`
Expected: 三条全 FAILED。

- [ ] **Step 3: 改 `resumed`**

把 `resumed` 开头那段读布局的代码(从 `let saved_layout = crate::shell::store::config_dir()` 到 `.unwrap_or_else(mullion_store::SavedLayout::empty);`)整段替换成:

```rust
        // F148:先把 v1 的 `layout.toml` 迁成一条记录并删掉它(D14),再裁剪
        // 到 10 条(D5/X6:**只在启动时裁一次**,关窗口时也裁的话「读整个
        // 目录」就从一次性动作变成了每个实例退出都要做的共享操作)。
        //
        // 两件事都在建窗口之前做:下面要拿最新一条的几何去填
        // `WindowAttributes`,建完再 `set_outer_position` 会让用户看见窗口
        // 先在默认位置闪一下再跳过去。
        let history = crate::shell::store::config_dir()
            .map(|d| {
                let now = mullion_store::now_secs();
                if let Some(id) = mullion_store::migrate_legacy(&d, &self.instance_id_for_legacy(), now) {
                    crate::logx::line(&format!("F148:老的 layout.toml 已迁成记录 {id}"));
                }
                let dropped = mullion_store::prune(&d, now);
                if dropped > 0 {
                    crate::logx::line(&format!("F148:裁掉了 {dropped} 条旧记录"));
                }
                mullion_store::list_records(&d, now)
            })
            .unwrap_or_default();
        // X8:启动**不摆标签**(D1),但窗口总得有个大小和位置 —— 取最新
        // 一条记录的几何(死活不论)。恢复某条记录时**不再改窗口几何**:
        // 窗口已经建好了,再跳一次位置只会让人眼花(D13)。
        let saved_window = history.first().and_then(|e| e.layout.window);
        let mut attrs = Window::default_attributes().with_title("mullion");
        if let Some(w) = saved_window {
```

紧接着原来 `if let Some(w) = saved_layout.window {` 之后的那整块(算 monitors、`clamp_to_monitors`、填 attrs)**原样保留**。

然后把 `resumed` 后半段里三处用到 `saved_layout` 的地方改掉:

```rust
                Ok(true) => {
                    crate::logx::line("secrets.enc 由主密码加密,等待解锁");
                    self.pending_history = Some(history);
                    self.ui.unlock = Some(crate::ui::unlock::UnlockDraft::default());
                }
                Ok(false) => {
                    self.open_store_with(
                        crate::shell::store::SessionStore::open(
                            d,
                            &mullion_store::KeyringSource::new(),
                        ),
                        history,
                    );
                }
                Err(e) => {
                    crate::logx::line(&format!("secrets.enc 探测失败: {e}"));
                    self.ui.set_error(format!("会话库打开失败:{e}"));
                    self.finish_store_open(history);
                }
```

`dir` 为 `None` 的那一臂(`match dir` 的 `None =>`)里同样把 `saved_layout` 换成 `history`(它此时是空 `Vec`)。

- [ ] **Step 4: 换掉 `pending_layout` 字段**

`App` 结构体里,把:

```rust
    pending_layout: Option<mullion_store::SavedLayout>,
```

替换成:

```rust
    /// F71 + F148:解锁框开着时,那份还没能用上的历史列表。
    ///
    /// 「这条会话还在不在库里」是丢弃规则之一(D16),而解锁框开着的时候库
    /// 还没打开 —— 列表只能先在这儿等着。解锁成功 / 放弃解锁时由
    /// `finish_store_open` 取走。
    pending_history: Option<Vec<mullion_store::HistoryEntry>>,
```

`App::new` 里 `pending_layout: None,` 改成 `pending_history: None,`。

`apply_unlock_action` 里三处 `self.pending_layout.take().unwrap_or_else(mullion_store::SavedLayout::empty)` 全部改成:

```rust
                    let history = self.pending_history.take().unwrap_or_default();
```

(对应的 `finish_store_open(layout)` / `open_store_with(.., layout)` 改成传 `history`。)

- [ ] **Step 5: 改 `open_store_with` / `finish_store_open` 的签名与函数体**

```rust
    fn open_store_with(
        &mut self,
        opened: Result<crate::shell::store::SessionStore, mullion_store::StoreError>,
        history: Vec<mullion_store::HistoryEntry>,
    ) {
```

(函数体里 `self.finish_store_open(saved_layout);` 改成 `self.finish_store_open(history);`。)

```rust
    /// 会话库尘埃落定之后的那串收尾:算外观缓存、决定第一屏。
    ///
    /// 抽出来的唯一理由是 F71:解锁框开着的时候这些都还不能做(库还没打开,
    /// 「这条会话还在不在库里」答不上来),得等解锁成功再跑。
    ///
    /// **F148 起不再自动摆回标签**(D1):启动摆什么由用户在恢复列表里选。
    fn finish_store_open(&mut self, history: Vec<mullion_store::HistoryEntry>) {
        // 启动时先算一次,否则第一次打开会话管理器全是无色。
        self.refresh_appearance();

        // CLI 直连(路径①)→ 立刻发起连接,进终端态。
        if let Some(cfg) = self.initial.take() {
            // CLI 直连恒是终端态——这条路径没有会话记录可查协议字段。
            self.spawn_connect(cfg, false);
            return;
        }
        // F148 D9:无参启动 → 有历史就先给恢复列表,没有就照旧弹会话管理器。
        // **必须在这里而不是 `resumed` 里**:「这条会话还在不在库里」要查
        // 会话库,而库到这一刻才刚打开。
        let rows = self.history_rows(&history);
        if rows.is_empty() {
            // 首次运行 / 全被清空 —— 弹一个空列表等于让用户点一下才能开始干活。
            self.ui.session_manager_open = true;
        } else {
            self.ui.history = Some(crate::ui::history::HistoryDraft::new(rows));
        }
    }
```

> `history_rows` 与 `crate::ui::history` 在 Task 9/10 才存在。**本 Task 先用一个临时桩**让它编译得过:在 `finish_store_open` 里暂时只写 `self.ui.session_manager_open = true;`(即把上面那段 `let rows = ...` 到 `}` 换成这一行),并在 Task 10 换成正式版本。这样本 Task 的三条测试能独立跑绿。

- [ ] **Step 6: 加一个只在迁移时用的实例 id**

在 `impl App` 里,`snapshot_layout` 附近插入:

```rust
    /// F148 D14:迁移老 `layout.toml` 时给那条记录用的 id。
    ///
    /// **不能直接用 `self.instance_id`**:那是本实例正在写的槽位,迁移过去
    /// 会被本进程 2 秒后的第一次落盘(此刻标签栏是空的)当场覆盖成空现场 ——
    /// 用户升级前那一屏就这么没了。
    fn instance_id_for_legacy(&self) -> String {
        format!("{}-legacy", self.instance_id)
    }
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 > /tmp/t8.log; grep -nE "test result|FAILED|panicked|error\[" /tmp/t8.log`
Expected: `test result: ok.`,零 FAILED。

> 既有测试 `the_restored_window_geometry_is_clamped_to_the_real_monitors` 扎的是 `resumed` 里有 `clamp_to_monitors(` —— 上面保留了那整块,它应该照常绿。若变红,说明那块被误删了。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 启动改为迁移+裁剪+读历史,不再自动摆回标签 (F148)"
```

---

### Task 9: 恢复列表弹窗

**Files:**
- Create: `crates/mullion-app/src/ui/history.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(只加 `pub mod history;`)

- [ ] **Step 1: 写文件(含全部纯函数与测试)**

创建 `crates/mullion-app/src/ui/history.rs`:

```rust
//! F148:「恢复上次的现场」弹窗(设计 D9/D10)。
//!
//! **零 IO**:这里只画一份已经准备好的行、回报用户选了哪条,真的读盘/摆标签
//! 在 `app.rs`。
//!
//! 时间显示用**相对时间**(设计 X3):`time` 0.3 只开了 `formatting` feature,
//! 拿不到本地时区偏移(`now_local` 要 `local-offset`,而且在多线程进程里按
//! soundness 规则通常返回 `Err`)。绝对时间在中国时区会差 8 小时 —— 相对时间
//! 既规避了这个坑,对「认出哪条记录」也更好用。

use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::metrics::{SP_L, SP_M, SP_S, SP_XS};

/// 摘要里最多列几个会话名,超出的折成 `+N`。
const SUMMARY_MAX: usize = 3;

/// 列表里的一行。**已经算好的字符串**,画的时候不做任何计算 ——
/// 这些文本的判据(几天前、摘要怎么折)全是纯函数,单独测。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRow {
    /// 实例 id,回报给 `app.rs` 用来认领槽位(D12)。
    pub id: String,
    /// 第一行:`3 小时前 · 4 个标签 · 7 块分屏`。
    pub head: String,
    /// 第二行:`prod-web-01 · nas · db-01 · +1`。
    pub summary: String,
}

/// 弹窗自己的那点状态。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryDraft {
    pub rows: Vec<HistoryRow>,
    /// 选中第几行。恒有值 —— 列表非空才会建这个草稿(见 `new`)。
    pub selected: usize,
}

impl HistoryDraft {
    /// **只在 `rows` 非空时调**:空列表的弹窗等于让用户点一下才能开始干活
    /// (D9:没有任何记录时不弹)。
    pub fn new(rows: Vec<HistoryRow>) -> Self {
        Self { rows, selected: 0 }
    }
}

/// 这一帧用户干了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryOut {
    /// 恢复这个实例 id 的现场。
    Restore(String),
    /// 「不恢复」/ 关掉弹窗。菜单里还有常驻入口能再打开(D9)。
    Dismiss,
}

/// 相对时间(设计 X3)。`now` 与 `updated_at` 都是 Unix 秒(UTC)。
///
/// 超过 7 天退回 `MM-DD`(UTC 日期,最多差一天)—— 到那个尺度上,「11 天前」
/// 不如一个日期好定位。
pub fn when_text(now: i64, updated_at: i64) -> String {
    // 未来的时刻(时钟往回跳过)按「刚刚」算 —— 显示「-3 小时前」更莫名其妙。
    let d = now.saturating_sub(updated_at);
    if d < 60 {
        return "刚刚".into();
    }
    if d < 3600 {
        return format!("{} 分钟前", d / 60);
    }
    if d < 86_400 {
        return format!("{} 小时前", d / 3600);
    }
    if d < 7 * 86_400 {
        return format!("{} 天前", d / 86_400);
    }
    match time::OffsetDateTime::from_unix_timestamp(updated_at) {
        Ok(dt) => format!("{:02}-{:02}", dt.month() as u8, dt.day()),
        // 时刻本身是垃圾(手改过的文件)。用破折号而不是编一个日期出来。
        Err(_) => "—".into(),
    }
}

/// 会话名摘要。超过 `SUMMARY_MAX` 个折成 `+N`。
///
/// 空列表给一句话而不是空字符串:第二行空着的话,那一行的高度还在,看着像
/// 渲染出了 bug。
pub fn summary_text(titles: &[String]) -> String {
    if titles.is_empty() {
        return "(没有可恢复的标签)".into();
    }
    if titles.len() <= SUMMARY_MAX {
        return titles.join(" · ");
    }
    format!(
        "{} · +{}",
        titles[..SUMMARY_MAX].join(" · "),
        titles.len() - SUMMARY_MAX
    )
}

/// 第一行。`panes` 是所有标签的分屏数之和。
///
/// 单标签单分屏时不啰嗦「1 个标签 · 1 块分屏」—— 那两句话没有信息量,
/// 只是把真正有用的时间挤到一边。
pub fn head_text(when: &str, tabs: usize, panes: usize) -> String {
    if tabs <= 1 && panes <= 1 {
        return when.to_string();
    }
    if panes <= tabs {
        return format!("{when} · {tabs} 个标签");
    }
    format!("{when} · {tabs} 个标签 · {panes} 块分屏")
}

/// 画弹窗。返回 `Some` = 这一帧有结论(由 `app.rs` 负责把 `draft` 置 `None`)。
///
/// `draft` 为 `None` = 弹窗关着,什么都不画。
pub fn show(ctx: &egui::Context, t: &Theme, draft: &mut Option<HistoryDraft>) -> Option<HistoryOut> {
    let d = draft.as_mut()?;
    let mut out = None;
    egui::Window::new("恢复上次的现场")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "恢复现场弹窗", ui.max_rect());
            ui.label(theme::hint_text(
                t,
                "选一条摆回标签栏。摆回来的标签**不会**自动连接,点「重连」才拨号。",
            ));
            ui.add_space(SP_M);
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for i in 0..d.rows.len() {
                        let selected = i == d.selected;
                        let row = &d.rows[i];
                        // 整行可点:只让文字可点的话,行末的空白点不中,而用户
                        // 会去点行的任何地方(J 片「标签宿主一行都点不中」的
                        // 同一个教训)。
                        let resp = ui.allocate_response(
                            egui::vec2(ui.available_width(), 44.0),
                            egui::Sense::click(),
                        );
                        // 选中 / 悬停的底色与会话列表同源:那边是
                        // `session_manager::list::row_bg(selected, hovered, None, t)`
                        // 的两条常量臂。**照抄取值而不是调那个函数** ——
                        // 它是 `pub(crate)` 且第三个参数是节点色(本列表没有
                        // 节点概念),为两个常量拉一条跨模块依赖不划算。
                        // 色板已冻结,**不许**往 `Theme` 里加新字段。
                        if selected {
                            ui.painter().rect_filled(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                theme::c32(t.sunken_bg),
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                theme::c32(t.panel_head),
                            );
                        }
                        let mut p = resp.rect.min + egui::vec2(SP_S, SP_XS);
                        ui.painter().text(
                            p,
                            egui::Align2::LEFT_TOP,
                            &row.head,
                            egui::FontId::proportional(14.0),
                            theme::c32(t.fg_strong),
                        );
                        p.y += 20.0;
                        ui.painter().text(
                            p,
                            egui::Align2::LEFT_TOP,
                            &row.summary,
                            egui::FontId::proportional(12.0),
                            theme::c32(t.fg_dim),
                        );
                        if resp.clicked() {
                            d.selected = i;
                        }
                        // 双击 = 选中并恢复,与会话管理器双击连接一致。
                        if resp.double_clicked() {
                            out = Some(HistoryOut::Restore(row.id.clone()));
                        }
                    }
                });
            ui.add_space(SP_L);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("恢复").min_size([96.0, 28.0].into()))
                    .clicked()
                {
                    out = Some(HistoryOut::Restore(d.rows[d.selected].id.clone()));
                }
                ui.add_space(SP_S);
                if ui.button("不恢复").clicked() {
                    out = Some(HistoryOut::Dismiss);
                }
            });
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_record_says_just_now() {
        assert_eq!(when_text(1000, 1000), "刚刚");
        assert_eq!(when_text(1000, 941), "刚刚");
    }

    #[test]
    fn relative_time_walks_up_the_units() {
        assert_eq!(when_text(1_000_000, 1_000_000 - 120), "2 分钟前");
        assert_eq!(when_text(1_000_000, 1_000_000 - 3 * 3600), "3 小时前");
        assert_eq!(when_text(1_000_000, 1_000_000 - 2 * 86_400), "2 天前");
    }

    /// 超过一周退回日期 —— 「11 天前」在那个尺度上不如一个日期好定位。
    #[test]
    fn anything_older_than_a_week_falls_back_to_a_date() {
        // 2025-08-14T00:00:00Z
        assert_eq!(when_text(1_755_000_000, 1_755_000_000 - 30 * 86_400), "07-14");
    }

    /// 时钟往回跳过(记录的时刻在未来)时不显示负数 —— 「-3 小时前」比
    /// 「刚刚」更让人以为程序坏了。
    ///
    /// 自证会变红:把 `when_text` 里的 `saturating_sub` 换成 `-`。
    #[test]
    fn a_record_stamped_in_the_future_reads_as_just_now() {
        assert_eq!(when_text(1000, 9999), "刚刚");
    }

    #[test]
    fn a_short_summary_lists_every_session() {
        let t = vec!["a".to_string(), "b".to_string()];
        assert_eq!(summary_text(&t), "a · b");
    }

    /// 长摘要折成 `+N` —— 不折的话第二行会把弹窗撑得比屏幕还宽。
    #[test]
    fn a_long_summary_is_folded() {
        let t: Vec<String> = (1..=6).map(|i| format!("s{i}")).collect();
        assert_eq!(summary_text(&t), "s1 · s2 · s3 · +3");
    }

    /// 空摘要给一句话,不是空字符串:那一行的高度还在,空着看着像渲染坏了。
    #[test]
    fn an_empty_summary_says_so_instead_of_going_blank() {
        assert_eq!(summary_text(&[]), "(没有可恢复的标签)");
    }

    /// 单标签单分屏不啰嗦 —— 「1 个标签 · 1 块分屏」没有信息量。
    #[test]
    fn a_single_pane_record_does_not_brag_about_its_counts() {
        assert_eq!(head_text("刚刚", 1, 1), "刚刚");
    }

    #[test]
    fn a_record_with_splits_reports_both_counts() {
        assert_eq!(head_text("3 小时前", 4, 7), "3 小时前 · 4 个标签 · 7 块分屏");
    }

    /// 标签数等于分屏数(每个标签都只有一块)时不重复报同一个数。
    #[test]
    fn a_record_without_splits_only_reports_the_tab_count() {
        assert_eq!(head_text("2 天前", 3, 3), "2 天前 · 3 个标签");
    }

    fn rows() -> Vec<HistoryRow> {
        vec![
            HistoryRow {
                id: "a".into(),
                head: "刚刚 · 2 个标签".into(),
                summary: "prod · nas".into(),
            },
            HistoryRow {
                id: "b".into(),
                head: "3 小时前".into(),
                summary: "db-01".into(),
            },
        ]
    }

    /// 跑两帧,收本帧画出来的所有文字。**两帧**:`egui::Window` 首帧
    /// `fade_in` 只记 `Shape::Noop`(同 `ui/restored.rs` 的说明)。
    fn texts(draft: &mut Option<HistoryDraft>) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, draft);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 点一下写着 `label` 的那颗按钮,返回这一帧 `show` 的结论。
    fn click(draft: &mut Option<HistoryDraft>, label: &str) -> Option<HistoryOut> {
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, draft);
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("弹窗里没有写着「{label}」的按钮"));
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, draft);
        });
        out
    }

    /// `None` 的草稿什么都不画 —— 弹窗关着就是关着。
    #[test]
    fn a_closed_dialog_draws_nothing() {
        let mut draft = None;
        assert!(texts(&mut draft).is_empty());
    }

    /// 每一条记录的两行都要画出来:只画第一行的话,多开场景下两条记录的
    /// 时间可能很接近,用户分不出哪条是哪个窗口(D10)。
    #[test]
    fn every_row_shows_both_its_lines() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let joined = texts(&mut draft).join(" ");
        assert!(joined.contains("刚刚 · 2 个标签"), "第一行没画:{joined}");
        assert!(joined.contains("prod · nas"), "第二行没画:{joined}");
        assert!(joined.contains("3 小时前"), "第二条记录没画:{joined}");
    }

    /// 「恢复」回报的是**当前选中那一条的 id**,不是恒第一条。
    ///
    /// 自证会变红:把 `d.rows[d.selected].id` 改成 `d.rows[0].id`。
    #[test]
    fn restoring_reports_the_selected_record_not_the_first_one() {
        let mut draft = Some(HistoryDraft::new(rows()));
        draft.as_mut().unwrap().selected = 1;
        assert_eq!(click(&mut draft, "恢复"), Some(HistoryOut::Restore("b".into())));
    }

    #[test]
    fn dismissing_reports_dismiss() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(click(&mut draft, "不恢复"), Some(HistoryOut::Dismiss));
    }

    /// 光把弹窗画出来不等于选了什么 —— 否则它一出现就自己恢复了,
    /// 「可以选择恢复」这条需求当场作废。
    #[test]
    fn merely_showing_the_dialog_restores_nothing() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(texts(&mut draft).is_empty(), false);
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(ctx, &t, &mut draft);
            });
        }
        assert_eq!(out, None);
    }
}
```

- [ ] **Step 2: 挂进 `ui/mod.rs`**

在 `pub mod restored;` 之前(保持字母序)插入:

```rust
pub mod history;
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-app ui::history 2>&1 | grep -E "test result|FAILED|error\["`
Expected: `test result: ok. 16 passed`。

用到的 `Theme` 字段(`sunken_bg` / `panel_head` / `fg_strong` / `fg_dim`)与
`metrics` 常量(`SP_XS` / `SP_S` / `SP_M` / `SP_L`)**都已核实存在**。色板已冻结
(见记忆「UI 视觉规格已冻结」)—— 缺什么颜色就从现有字段里挑,**不许**往
`Theme` 里加字段。

- [ ] **Step 4: 字形白名单检查**

本文件里用到的非 ASCII 符号只有 `·`(U+00B7)和 `—`(U+2014),两个都已在
`ui::glyphs::VERIFIED` 里登记过(T9)。跑一遍确认:

Run: `cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/history.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 恢复现场弹窗与它的相对时间/摘要文案 (F148)"
```

正文写明:触到 T9(字形白名单),跑的是 `tests/glyph_whitelist.rs`。

---

### Task 10: app 接线弹窗 —— 模态门控、菜单入口、恢复处置

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/ui/chrome.rs`

- [ ] **Step 1: 写失败的测试**

**1a.** 在 `app.rs` 的 `every_modal_variant_is_listed_in_all` 那个 `check` 的 `match` 里加一条分支(搜 `Modal::FilesPathEdit =>` 在测试里的那处),并在它附近加:

```rust
    /// **接线守护 / F148**:恢复列表弹窗必须算模态(T8)。
    ///
    /// 不算的话,它开着的时候 `Ctrl+W` 仍能关掉背后的标签、方向键仍被判给
    /// 终端 —— 而这个弹窗是启动后用户看到的第一样东西。
    #[test]
    fn the_history_dialog_counts_as_a_modal_so_it_does_not_share_the_keyboard() {
        assert!(
            Modal::ALL.contains(&Modal::History),
            "History 没登记进 Modal::ALL(T8)"
        );
        let src = include_str!("app.rs");
        let after = src.split("fn modal_open(").nth(1).expect("找不到 modal_open");
        let body = &after[..after.find("\n    }\n").expect("找不到 modal_open 的结尾")];
        assert!(
            body.contains("Modal::History =>"),
            "modal_open 里没有 History 这一臂(T8)"
        );
    }
```

**1b.** 在 `has_real_action` 附近加:

```rust
    /// **接线守护 / F148**:弹窗的结论必须进 `has_real_action`。
    ///
    /// 漏了的话,「恢复」按下去会在 egui 的 discard 趟被静默吃掉 —— 而这个
    /// 弹窗是启动后唯一能操作的东西,用户只能去杀进程。
    ///
    /// 自证会变红:把 `has_real_action` 里的 `|| a.history.is_some()` 删掉。
    #[test]
    fn the_history_dialog_action_is_not_swallowed_by_the_discard_pass() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn has_real_action(")
            .nth(1)
            .expect("找不到 has_real_action");
        let body = &after[..after.find("\n}\n").expect("找不到 has_real_action 的结尾")];
        assert!(
            body.contains("a.history.is_some()"),
            "恢复列表的结论会在 discard 趟被静默吃掉"
        );
    }
```

**1c.** 在 `chrome.rs` 的 `mod tests` 里(若没有就新建)加:

```rust
    /// **接线守护 / F148 D9**:菜单里必须留一个常驻入口。
    ///
    /// 只有启动弹窗的话,用户手滑点了「不恢复」就再也回不去,而那 10 条记录
    /// 还在磁盘上躺着。
    ///
    /// **扎的是源码结构**(菜单项要展开 `menu_button` 才画得出来,跑帧测不到)。
    /// 判据串带上行首缩进,避免匹配到这条测试自己(第五类恒绿模式)。
    #[test]
    fn the_session_menu_has_a_permanent_entry_to_the_history_dialog() {
        let src = include_str!("chrome.rs");
        assert!(
            src.contains("\n                    if ui.button(\"恢复上次的现场…\").clicked() {"),
            "「会话」菜单里没有恢复现场的常驻入口 —— 点过一次「不恢复」就再也回不去了"
        );
    }
```

- [ ] **Step 2: 跑测试确认它们失败**

Run: `cargo test -p mullion-app history 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 编译失败(`no variant named History`)或三条 FAILED。

- [ ] **Step 3: 加 `Modal::History`**

`app.rs` 的 `enum Modal` 里,`FilesPathEdit` **之后**加:

```rust
    /// F148:「恢复上次的现场」弹窗。里面没有输入框,但有一颗一按就摆回
    /// 整个标签栏的「恢复」按钮,而空格/回车在 egui 里是按钮的激活键 ——
    /// 同 `Modal::Import` 的理由(T8)。
    History,
```

`Modal::ALL` 里加 `Modal::History,`。

`modal_open` 的 `match` 里加:

```rust
            // F148:见 `Modal::History` 的说明(T8)。
            Modal::History => self.ui.history.is_some(),
```

测试 `every_modal_variant_is_listed_in_all` 的 `check` 里加一条对应的 `assert!`(照抄现有那些的写法)。

- [ ] **Step 4: UI 侧接线**

**4a.** `ui/mod.rs` 的 `UiState` 里加:

```rust
    /// F148:恢复列表弹窗。`Some` = 弹窗开着。
    ///
    /// **必须计进 `app.rs::modal_open`**:里面有一颗一按就摆回整个标签栏的
    /// 按钮,而空格/回车是 egui 的按钮激活键(T8)。
    pub history: Option<history::HistoryDraft>,
    /// F148:菜单里点了「恢复上次的现场…」→ `app.rs` 事后读盘、建草稿。
    ///
    /// **不在这里直接建草稿**:建草稿要读 `layouts` 目录、还要查会话库把
    /// 已删会话的标签滤掉(D16),而 `ui/` 这一层零 IO。
    pub history_request: bool,
```

**4b.** `ui/mod.rs` 的 `UiActions` 里加:

```rust
    /// F148:恢复列表这一帧的结论(恢复某条 / 不恢复)。`None` = 没动过。
    ///
    /// 加字段时记得同步 `app.rs::has_real_action` —— 漏了的话「恢复」按下去
    /// 毫无反应,而这个弹窗是启动后唯一能操作的东西。
    pub history: Option<history::HistoryOut>,
```

**4c.** `ui/mod.rs` 的 `build_ui` 里,在解锁框那一段**之后**、`if ui_state.session_manager_open {` 之前插入:

```rust
    // F148:恢复列表。画在解锁框**之后** —— 解锁框开着时会话库还没打开,
    // 这个列表里「哪条会话还在」根本答不上来(它由 `app.rs` 在库打开之后
    // 才建草稿,所以此刻它必然是 `None`,这里只是把顺序写明白)。
    // 排在会话管理器**之前**:启动那一刻它是用户看到的第一样东西。
    actions.history = history::show(ctx, t, &mut ui_state.history);
```

**4d.** `chrome.rs` 的「会话」菜单里,在「全部重连」**之后**、「退出」之前插入:

```rust
                    // F148 D9:常驻入口。只有启动弹窗的话,用户手滑点了
                    // 「不恢复」就再也回不去,而那 10 条记录还在磁盘上躺着。
                    if ui.button("恢复上次的现场…").clicked() {
                        ui_state.history_request = true;
                        ui.close_menu();
                    }
```

**4e.** `app.rs` 的 `has_real_action` 里加一行:

```rust
        || a.history.is_some()
```

- [ ] **Step 5: 实现恢复处置**

在 `app.rs` 的 `restore_tabs` 附近加:

```rust
    /// F148:把一批记录做成弹窗要画的行(D10/D16)。
    ///
    /// **会话已删的标签在这里就被滤掉**(沿用 `layout_snapshot::usable` 的
    /// 规则):摘要里列一个已经不存在的会话名,用户点了恢复只会得到一个点了
    /// 必然失败的「重连」。**整条记录一个可用标签都不剩时,这条记录不进列表**
    /// —— 它恢复出来是个空窗口。
    ///
    /// **活着的实例的记录不进列表**(D3):那个现场正被别人用着。
    fn history_rows(
        &self,
        entries: &[mullion_store::HistoryEntry],
    ) -> Vec<crate::ui::history::HistoryRow> {
        let known: Vec<SessionId> = self
            .store
            .as_ref()
            .map_or(Vec::new(), |s| s.list().iter().map(|r| r.id).collect());
        let now = mullion_store::now_secs();
        let mut out = Vec::new();
        for e in entries {
            if e.alive {
                continue;
            }
            let usable =
                crate::shell::layout_snapshot::usable(e.layout.clone(), &|id| known.contains(&id));
            if usable.tabs.is_empty() {
                continue;
            }
            let titles: Vec<String> = usable.tabs.iter().map(|t| t.title.clone()).collect();
            let panes: usize = usable
                .tabs
                .iter()
                .map(|t| crate::shell::layout_snapshot::leaf_count(&t.tree).unwrap_or(1))
                .sum();
            let when = crate::ui::history::when_text(now, e.layout.updated_at);
            out.push(crate::ui::history::HistoryRow {
                id: e.id.clone(),
                head: crate::ui::history::head_text(&when, usable.tabs.len(), panes),
                summary: crate::ui::history::summary_text(&titles),
            });
        }
        out
    }

    /// F148:菜单里点了「恢复上次的现场…」—— 现读一次盘、建草稿。
    ///
    /// **现读而不是用启动时那份**:这中间可能又有别的窗口关掉了,拿旧列表
    /// 会让用户看不到刚关的那个现场。
    fn open_history_dialog(&mut self) {
        let entries = crate::shell::store::config_dir()
            .map(|d| mullion_store::list_records(&d, mullion_store::now_secs()))
            .unwrap_or_default();
        let rows = self.history_rows(&entries);
        if rows.is_empty() {
            self.ui.set_toast("没有可恢复的现场");
            return;
        }
        self.ui.history = Some(crate::ui::history::HistoryDraft::new(rows));
        self.ui_dirty = true;
    }

    /// F148:恢复一条记录(D12 接管槽位 / D13 追加进当前窗口)。
    ///
    /// 三步,顺序不能换:
    /// 1. 读出那条记录并摆回标签(**追加**在现有标签后面,不清空 —— 清空会
    ///    断掉正在跑的连接);
    /// 2. 删掉本实例原来的槽位文件(启动时它通常还不存在,删除是 no-op);
    /// 3. 把本实例的身份换成那条记录的 id —— 此后就往那个文件写。
    ///
    /// 第 3 步是「接管」的全部内容(D12):不接管的话,本实例仍在写自己的新
    /// 槽位,而老记录原样躺着 —— 下次启动列表里就会出现两条内容几乎一样的
    /// 记录,而且越滚越多。
    ///
    /// **窗口几何不套用**(X8/D13):窗口已经建好了,再跳一次位置只会让人
    /// 眼花。
    fn restore_history(&mut self, id: &str) {
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        let now = mullion_store::now_secs();
        let Some(entry) = mullion_store::list_records(&dir, now)
            .into_iter()
            .find(|e| e.id == id)
        else {
            // 两次启动之间被别的实例裁掉了(D5)。不是错误,说一声就行。
            // `set_toast` 收 `impl Into<String>`,直接传 `&str` ——
            // 写成 `"…".into()` 推断不出目标类型,编译不过。
            self.ui.set_toast("那条现场已经不在了");
            return;
        };
        self.restore_tabs(entry.layout);
        // 2 → 3:先删旧槽位再改身份,顺序反了会把**刚接管的那个文件**删掉。
        mullion_store::remove_record(&dir, &self.instance_id);
        self.instance_id = id.to_string();
        // 接管之后立刻打一次心跳:别的实例这一刻起就该把这个槽位看成「有人
        // 在用」,否则第二个新实例会把它也列出来,两个进程往同一个文件写(D12
        // 的残余竞态)。
        let _ = mullion_store::touch_alive(&dir, &self.instance_id, now);
        // 本实例的记录内容变了(标签栏多了一批),下次比对必须重来一遍 ——
        // 不清的话 `save_layout_if_changed` 会拿旧快照比出「没变」,新摆回来
        // 的标签永远不落盘。
        self.last_saved_layout = None;
        self.ui_dirty = true;
    }
```

在处理 `UiActions` 的那一段(搜 `if let Some(out) = actions.unlock`,照它的位置)加:

```rust
        if let Some(out) = actions.history.take() {
            // 无论恢复还是不恢复,弹窗都收掉 —— 「恢复」之后还留着的话,
            // 用户会以为可以再选一条(而那时本实例已经接管了槽位)。
            self.ui.history = None;
            if let crate::ui::history::HistoryOut::Restore(id) = out {
                self.restore_history(&id);
            }
            self.ui_dirty = true;
        }
        if std::mem::take(&mut self.ui.history_request) {
            self.open_history_dialog();
        }
```

最后把 Task 8 Step 5 里那个临时桩换成正式版本 —— `finish_store_open` 末尾:

```rust
        let rows = self.history_rows(&history);
        if rows.is_empty() {
            // 首次运行 / 全被清空 —— 弹一个空列表等于让用户点一下才能开始干活。
            self.ui.session_manager_open = true;
        } else {
            self.ui.history = Some(crate::ui::history::HistoryDraft::new(rows));
        }
```

- [ ] **Step 6: `restore_tabs` 改成追加**

现有的 `restore_tabs` 本来就是 `self.tabs.open(..)` 逐个开(不清空),**但末尾那句 `switch_to_index(usable.active_tab)` 在追加场景下会跳到错的下标**(F37 那时只在启动时调,标签栏必然是空的,`base` 恒 0;F148 起菜单里随时能恢复,前面就有别的标签了)。

三处改动。**其一**,`if usable.tabs.is_empty()` 之后、`let count` 那行改成:

```rust
        let count = usable.tabs.len();
        // D13:**追加**在现有标签后面,不清空 —— 清空会断掉正在跑的连接。
        // 所以得先记住追加起点:存进记录里的 `active_tab` 是**那条记录内部**
        // 的下标,不是本窗口标签栏里的下标。
        let base = self.tabs.len();
        let active = usable.active_tab;
```

(`active` 必须在这里取 —— 下面那个 `for t in usable.tabs` 会把 `usable` 部分移走。)

**其二**,末尾那两行:

```rust
        // `open` 每开一个都会把它设成活动标签,所以最后再切回存的那一个。
        // D13:加上追加起点 —— 运行中恢复时前面还有别的标签,用记录内部的
        // 裸下标会跳到一个不相干的标签上。
        self.tabs.switch_to_index(base + active);
        crate::logx::line(&format!("F148:恢复了 {count} 个占位标签"));
```

**其三**:既有的源码级守护测试判据是 `body.contains("layout_snapshot::usable(")`,上面三处都没动那行,它照常绿。

再加一条测试(放在 `app.rs` 的 `mod tests`):

```rust
    /// **D13**:运行中恢复要**追加**,不能把现有标签顶掉 —— 顶掉会断连接。
    ///
    /// 自证会变红:把 `restore_tabs` 里的 `base + active` 改回 `active`。
    #[test]
    fn restoring_into_a_non_empty_window_switches_to_the_newly_added_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn restore_tabs(")
            .nth(1)
            .expect("找不到 restore_tabs");
        let body = &after[..after.find("\n    }\n").expect("找不到 restore_tabs 的结尾")];
        assert!(
            body.contains("base + active"),
            "恢复时活动标签用的是记录内部的裸下标 —— 运行中恢复会跳到不相干的标签上"
        );
    }
```

- [ ] **Step 7: 跑全量测试**

Run: `cargo test --workspace > /tmp/t10.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t10.log`
Expected: 全 ok,零 FAILED。

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无 warning。

Run: `cargo fmt --check`
Expected: 无输出。

> ⚠️ `cargo fmt` 可能把上面 Step 1c 那条源码级判据里的长字符串拆行,导致它变红
> (记忆里「fmt 拆行打断源码锚点」的那类恒绿/假红)。若发生,把判据改短:
> 只匹配 `"恢复上次的现场…"` 这个字面量 + 同一行有 `.clicked()`。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/chrome.rs
git commit -m "feat(app): 恢复列表接线——模态门控、菜单入口、接管槽位 (F148)"
```

正文写明:触到 T8(模态门控),跑的是 `the_history_dialog_counts_as_a_modal_so_it_does_not_share_the_keyboard` 与 `every_modal_variant_is_listed_in_all`。

---

### Task 11: 登记 spec 与文档

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 改写 F37 的正文**

找到 `| F37 | 布局持久化,重启恢复 | P1 | 序列化/反序列化 round-trip 单测 |`,替换成:

```markdown
| F37 | 布局持久化(标签 + 分屏树 + 焦点叶子 + 窗口几何)。**启动即自动摆回**那部分已被 F148 取代** —— 现在存的是「本实例的槽位」,摆什么由用户在恢复列表里选 | P1 | 序列化/反序列化 round-trip 单测(`layout::tests`) |
```

- [ ] **Step 2: 新增 F148**

在 F147 那一行之后插入:

```markdown
| F148 | **多实例现场历史**:每个 exe 实例各写自己的 `layouts\<实例id>.toml`(实例 id = 毫秒时间戳 + pid),**从不改别人的文件**;活性靠 `<实例id>.alive` 的心跳(15 秒一写,45 秒宽限),运行中的实例不进别人的恢复列表;已关闭的记录保留最近 10 条(**活着的不删也不占名额**);启动时迁移 v1 的 `layout.toml`(迁完即删,单向升级)→ 裁剪 → 弹「恢复上次的现场」列表(双行:相对时间 + 标签/分屏数 + 会话名摘要),**启动不再自动摆回标签**;恢复 = 摆回标签 + **接管那条记录的槽位**(此后往它写,并删掉本实例原来的);菜单「会话」下留常驻入口 | P1 | `is_alive` 纯函数的宽限边界与「未来心跳算活着」;`plan_prune` 的「活着的不占名额」;一条坏记录不拖垮整份列表;迁移后老文件必须消失;**心跳不许搭布局落盘的「不脏就不写」**(源码级:`tick_heartbeat` 体内不含 `last_saved_layout`);`Modal::History` 进 `ALL` 且 `modal_open` 有对应臂(T8);`has_real_action` 含 `a.history.is_some()`;弹窗的相对时间/摘要/首行三个纯函数;「恢复」回报选中项而非第一项 |
```

- [ ] **Step 3: 更新路线图**

找到 `| **v0.5** | F4, F5, ~~F36~~（已提前到 D0）, F37, F71 | 代理、跳板机、持久化 |`,在 `F37` 后加 `, F148`。

- [ ] **Step 4: 提交**

```bash
git add spec.md
git commit -m "docs: 登记 F148,F37 正文改写为「已被 F148 取代」"
```

---

### Task 12: 发版

- [ ] **Step 1: 走发版一条龙**

用 `release-windows` skill(说「发版」即自动加载)。它会:升 patch 版本号(单独 `chore:` 提交)→ 跑绿 → 交叉编译 → objdump 依赖验收(出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格)→ **签名**(在算 sha256 之前)→ 先 push 再 `gh release create`(标题只能是纯版本号 `v0.1.N`)。

- [ ] **Step 2: 人工验收清单(写进 Release notes)**

```markdown
## F148 人工验收清单

**多实例互不覆盖(核心,必须真开两个 exe)**
1. 开 exe A,建 2 个标签、其中一个分成 3 屏;开 exe B,建 1 个标签。
2. 先关 A,再关 B。
3. 开 exe C → 恢复列表里应有**两条**记录(A 那条写着「2 个标签 · 4 块分屏」,B 那条写着 1 个标签)。
   - 只有一条 = 互相覆盖了,这一片的核心没做到。

**运行中的实例不进列表**
4. 开 exe A(别关),再开 exe B → B 的列表里**不该**有 A 那条。
5. 关掉 A,等 1 分钟,再开 exe C → C 的列表里**应该**有 A 那条(心跳过期了)。

**接管槽位(不长重复项)**
6. 从列表恢复一条 → 关窗口 → 再开 → 列表里那条记录**还是一条**,不是两条。

**迁移**
7. 升级前若 `%APPDATA%\mullion\layout.toml` 存在:升级后第一次启动,列表里应有它那一条,且该文件**已消失**。

**常驻入口**
8. 点「不恢复」关掉弹窗 → 菜单「会话」→「恢复上次的现场…」应能再打开。
9. 窗口里已有标签时从菜单恢复一条 → 新标签**追加**在后面,原有标签和连接不受影响。

**裁剪**
10. 反复开关 exe 十几次 → `%APPDATA%\mullion\layouts\` 里的 `.toml` 不超过 10 个(活着的实例除外)。

**观感(只有人眼能判)**
11. 弹窗两行文字的排版、截断位置、选中行的底色。
12. 「3 小时前」这类相对时间读起来对不对。
```

---

## 自查

**规格覆盖**:D1(Task 8)、D2(Task 7)、D3(Task 4 `alive` + Task 10 `history_rows` 过滤)、D4(Task 2/3)、D5(Task 5)、D9(Task 9/10)、D10(Task 9)、D11(不做,无任务)、D12(Task 10 `restore_history`)、D13(Task 10 Step 6)、D14(Task 6 + Task 8)、D15(不做状态字段,`CloseRequested` 里既有的 `save_layout_if_changed` 已满足)、D16(Task 10 `history_rows`)、D19(本计划 = 第一片)、X1(Task 1)、X2(路径隔开,无任务)、X3(Task 9 `when_text`)、X4(Task 10)、X5(Task 9 Step 4)、X6(Task 8)、X8(Task 8 Step 3)。

**D6/D7/D8/D17/D18 不在本片** —— per-pane 节点、空壳 pane、回车触发连接、全部重连队列全部归第二片 F149,本计划一个任务都不给它们。

**留给 F149 的接口债**:`restore_tabs` 目前把整棵树塞进 `TabContent::Restored`,第二片要把终端标签那一支换成真 `Workspace` + 空壳 pane;`SavedTab` 的叶子届时要加 `session_id: Option<SessionId>`(D6)。两处都不影响本片的磁盘格式 —— 加 `Option` 字段走 `#[serde(default)]`,老记录读进来是 `None`。
