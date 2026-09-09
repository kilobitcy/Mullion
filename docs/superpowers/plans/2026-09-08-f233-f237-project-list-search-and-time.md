# F233~F237 项目列表：搜索 / 相对时间 / 记账时机 / 「+ 添加项目」/ 多行说明

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让三处项目列表（项目管理器左栏 / 启动页「继续上次的活」/ pane 标题条切换弹窗）都能搜索、都显示「最后打开」的相对时间、都共用同一份行渲染；并把「+ 添加项目」变成一键新建、把项目「说明」改成多行。

**Architecture:** 新增一个手绘的共享行组件 `ui::project_row`，三处列表全部改用它；搜索判据与相对时间文案各抽成一个零 UI 的纯函数（分别落在 `crate::project` 与 `crate::localtime`），命中高亮复用会话侧既有的 `highlight::segments` + `paint_highlighted`（提升可见性，不搬家）。项目管理器左栏重排为「顶部搜索框 / 中间列表 / 底部整宽『+ 添加项目』」三段式，右栏套滚动区并把按钮行钉在底部。

**Tech Stack:** Rust / egui 0.30 / `time` 0.3（本计划新开 `parsing` feature）

---

## 背景：为什么是这几条

这五条来自一次实机走查后的 grilling，共识摘要：

| 编号 | 内容 |
|---|---|
| F233 | 三处项目列表统一支持搜索，匹配**名称 + 目录 + 节点会话名 + 节点主机**；命中片段染 accent；搜不到给「没有匹配的项目」+「清空搜索」出口 |
| F234 | 三处列表显示**相对时间**（刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 超 30 天走本地日期） |
| F235 | `last_accessed_at` 的记账时机：**点开项目那一刻就写**，现有的上报跃迁写入保留为补充 |
| F236 | 左栏输入框改为搜索框；「添加」变成撞满左栏宽的描边按钮「+ 添加项目」移到左栏底部，点它直接建「新项目」 |
| F237 | 右栏「说明」改多行（3 行），标签顶对齐 |

支撑改动（不单独编号，分摊在上面几条里）：抽共享 `project_row`；左栏定宽 300、窗口 720→840；右栏内容套滚动区、按钮行固定底部。

**不在本计划范围内**（下一批，schema v11）：项目图标与颜色（F238）。因此 `project_row` **本批不预留图标槽位**——那会在界面上留一列无法解释的 32px 空白。

---

## 已认下的代价（每条都要落进代码注释）

1. **相对时间不自动刷新**。帧闸只在有事件时重绘（F157~F183 一连八个切片就是在抠空闲帧），文字会停在打开那一刻，用户动一下鼠标才跳。不为一行时间文字去请求定时重绘。
2. **时区取进程启动时那一次**（`localtime` 模块既有约定，F186）。进程跑着时改系统时区或跨夏令时要重启 exe 才更新。
3. **本批不预留图标槽**，下一批加图标时行内坐标整体右移，本批守护测试里写死的落点坐标要跟着改。
4. **`highlight` 与 `paint_highlighted` 只提升可见性、不搬家**。搬到中立模块更干净，但会打到一大批「读源码断言」的守护测试，且违反 Scope Discipline。代价是 `project_row` 要 `use crate::ui::session_manager::...`，依赖方向上看着别扭——注释里写清。

---

## 领域陷阱对照（动手前必读）

| 陷阱 | 本计划哪里会撞上 |
|---|---|
| **T8**（键盘先判后喂） | 启动页新增搜索框。launcher 态一块 pane 都没有、没有终端跟它抢键盘，**不需要**新增 `Modal` 项；项目管理器（`Modal::ProjectManager`）与切换弹窗（已登记）本来就在表里 |
| **T9**（字形白名单） | 新文案「+ 添加项目」「搜索项目名 / 目录 / 节点」「刚刚」「N 分钟前」「昨天」「没有匹配的项目」「清空搜索」全是 GBK 内汉字 + ASCII；`·` 已在 `VERIFIED` 里。**不要**引入 `…`／`—`／`▸` 之类 |
| **F141**（一行都点不中） | `project_row` 必须 `allocate_exact_size` 整行矩形再 `interact`，判定矩形罩住整行 |
| **F119 表单规范** | `form_guidelines.rs` 的 `EXTRA` 要登记新文件，且不得出现 `add_space(<数字>)` / `desired_width(<数字>)` |

---

## 文件清单

**新建**
- `crates/mullion-app/src/ui/project_row.rs` —— 三处共用的项目行（手绘）。唯一职责：画一行 + 返回 `Response`

**修改**
- `Cargo.toml`（workspace 根）—— `time` 加 `parsing` feature
- `crates/mullion-app/src/localtime.rs` —— 加 `relative()` 纯函数
- `crates/mullion-app/src/project.rs` —— 加 `matches()` / `fresh_project_name()` 纯函数
- `crates/mullion-app/src/ui/mod.rs` —— 挂 `pub mod project_row`；`UiState` 字段增删
- `crates/mullion-app/src/ui/session_manager/mod.rs` —— `mod highlight` → `pub(crate) mod highlight`
- `crates/mullion-app/src/ui/session_manager/highlight.rs` —— `pub(super) fn segments` → `pub(crate) fn segments`
- `crates/mullion-app/src/ui/session_manager/list.rs` —— `fn paint_highlighted` → `pub(crate) fn paint_highlighted`
- `crates/mullion-app/src/ui/project_manager.rs` —— 左栏三段式重排 + 右栏滚动/多行说明/窗口宽度
- `crates/mullion-app/src/ui/launcher.rs` —— 搜索框 + 改用 `project_row`
- `crates/mullion-app/src/ui/project_pick.rs` —— 改用 `project_row` + 高度预算重算 + hint 文案
- `crates/mullion-app/src/app.rs` —— `ProjectIntent::Add` 改用 `fresh_project_name`；`dial_project` 记一笔访问时间
- `crates/mullion-app/tests/form_guidelines.rs` —— `EXTRA` 登记三个文件
- `spec.md` —— 补 F233~F237

---

## Task 1: `time` 开 `parsing` + `localtime::relative()`

**Files:**
- Modify: `Cargo.toml:50`
- Modify: `crates/mullion-app/src/localtime.rs`（在 `format_unix` 之后追加）

- [ ] **Step 1: 先确认 `parsing` 现在确实没开（否则后面那步是空操作）**

Run:
```bash
grep -n '^time = ' Cargo.toml
```
Expected: `time = { version = "0.3", features = ["formatting", "local-offset"] }` —— 没有 `parsing`。

- [ ] **Step 2: 写失败的测试**

在 `crates/mullion-app/src/localtime.rs` 的 `mod tests` 里追加（文件末尾已有 `#[cfg(test)] mod tests`，追加到它内部）：

```rust
    /// 固定一个「现在」：`2026-09-08T12:00:00Z`。测试不许用真实时钟 ——
    /// 那样断言会在跨过整点/午夜时随机变红。
    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-09-08T12:00:00Z", &time::format_description::well_known::Rfc3339)
            .expect("测试基准时间写错了")
    }

    /// UTC+8（本项目主场景）。
    fn cn() -> UtcOffset {
        UtcOffset::from_hms(8, 0, 0).expect("UTC+8 是合法偏移")
    }

    /// 一分钟以内不报「0 分钟前」——那句话读起来像出错了。
    ///
    /// 自证会变红：把 `secs < 60` 那一支删掉。
    #[test]
    fn anything_within_a_minute_reads_as_just_now() {
        assert_eq!(relative("2026-09-08T11:59:30Z", now(), cn()), "刚刚");
    }

    #[test]
    fn minutes_and_hours_are_counted_off_the_raw_difference() {
        assert_eq!(relative("2026-09-08T11:30:00Z", now(), cn()), "30 分钟前");
        assert_eq!(relative("2026-09-08T09:00:00Z", now(), cn()), "3 小时前");
    }

    /// **本计划最重要的一条。** 「昨天」按**本地**日界判，不按 UTC 日期。
    ///
    /// `2026-09-08T00:30Z` 在 UTC 下是 9 月 8 日（与 `now` 同日），
    /// 但在 UTC+8 下是 9 月 8 日 08:30 —— 同样同日。反过来
    /// `2026-09-07T17:00Z` 在 UTC 下是 9 月 7 日（昨天），在 UTC+8 下
    /// 是 9 月 8 日 01:00（今天，19 小时前）。拿 UTC 日期算的话，
    /// 用户每天早上 8 点前都会看到错误的「昨天」，而且完全静默 ——
    /// 编译、测试、日志一律正常，只有人眼能发现。
    ///
    /// 自证会变红：把 `then.to_offset(offset).date()` 里的 `to_offset(offset)`
    /// 去掉（两处都去），第二条断言会变成「昨天」。
    #[test]
    fn yesterday_is_decided_by_the_local_day_boundary_not_the_utc_one() {
        // UTC+8 下这是 9 月 7 日 23:00 —— 真的是昨天。
        assert_eq!(relative("2026-09-07T15:00:00Z", now(), cn()), "昨天");
        // UTC+8 下这是 9 月 8 日 01:00 —— 今天凌晨，19 小时前。
        assert_eq!(relative("2026-09-07T17:00:00Z", now(), cn()), "19 小时前");
    }

    #[test]
    fn a_few_days_back_counts_days_and_a_month_back_falls_back_to_a_date() {
        assert_eq!(relative("2026-09-05T12:00:00Z", now(), cn()), "3 天前");
        // 31 天前 → 落回本地日期。UTC+8 下 2026-08-08T00:00Z 是 08-08 08:00。
        assert_eq!(relative("2026-08-08T00:00:00Z", now(), cn()), "2026-08-08");
    }

    /// 时钟回拨（或配置来自一台快钟的机器）时不能显示「-3 分钟前」。
    ///
    /// 自证会变红：把 `secs < 60` 改成 `(0..60).contains(&secs)`。
    #[test]
    fn a_timestamp_from_the_future_reads_as_just_now_not_a_negative_count() {
        assert_eq!(relative("2026-09-08T12:05:00Z", now(), cn()), "刚刚");
    }

    /// 解析不出来时**返回原文**，不编一句「时间未知」——那句话既没有
    /// 可操作性，又把「配置里到底写了什么」这唯一的线索藏起来。
    ///
    /// 自证会变红：把早退分支改成返回 `"未知".to_string()`。
    #[test]
    fn an_unparseable_stamp_is_shown_verbatim_instead_of_a_made_up_placeholder() {
        assert_eq!(relative("昨天下午", now(), cn()), "昨天下午");
    }
```

同时把 `mod tests` 顶部的 `use` 补全（原文件里只有 `use super::*;` 就够，`OffsetDateTime`/`UtcOffset` 已在模块顶部 `use`）。

- [ ] **Step 3: 跑测试确认它失败**

Run:
```bash
cargo test -p mullion-app localtime:: 2>&1 | tail -20
```
Expected: 编译失败，`cannot find function relative in this scope`。

- [ ] **Step 4: 开 `parsing` feature**

`Cargo.toml:50` 改为：
```toml
time = { version = "0.3", features = ["formatting", "parsing", "local-offset"] }
```

- [ ] **Step 5: 实现 `relative()`**

在 `crates/mullion-app/src/localtime.rs` 的 `format_unix` 之后、`#[cfg(test)]` 之前追加：

```rust
/// F234:「最后打开」的相对时间文案。
///
/// `then_rfc3339` 是我们自己写出去的时间戳(`OffsetDateTime::now_utc()` +
/// `Rfc3339`,一律 UTC)。`now` 与 `offset` **都是参数不是全局读取** ——
/// 理由同 `format_unix`:进程级 `OnceLock` 一旦被别的测试设过就再也改不动,
/// 拿它当输入的测试会互相打架;而「现在几点」写死才能让断言不随真实时钟
/// 在跨整点/跨午夜时随机变红。
///
/// 档位:刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 超 30 天落回本地日期。
///
/// **「昨天」与「N 天前」按本地日界判,不按 UTC 日期。**
/// `2026-09-07T17:00Z` 在 UTC 下是 9 月 7 日(昨天),在 UTC+8 下却是
/// 9 月 8 日凌晨 1 点(今天)。拿 UTC 日期算的话,用户每天早上 8 点前都会
///看到错误的「昨天」——而且完全静默:编译、测试、日志一律正常,只有人眼
/// 能发现。
///
/// **返回值不会自己更新**:帧闸只在有事件时重绘,这行字会停在画出来那一刻
/// 直到用户动一下。为一行时间文字去请求定时重绘,和 F157~F183 一连八个
/// 切片抠空闲帧的方向直接相反 —— 这个代价是认下的。
///
/// 解析不出来时**返回原文**:那只可能来自手改配置文件,而编一句「时间未知」
/// 既没有可操作性,又把「配置里写了什么」这唯一线索藏起来。
pub fn relative(then_rfc3339: &str, now: OffsetDateTime, offset: UtcOffset) -> String {
    let Ok(then) = OffsetDateTime::parse(
        then_rfc3339,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return then_rfc3339.to_string();
    };
    let secs = (now - then).whole_seconds();
    // `< 60` 而不是 `(0..60)`:时钟回拨(或配置来自一台快钟的机器)时
    // 负数差要落进「刚刚」,不能算出「-3 分钟前」。
    if secs < 60 {
        return "刚刚".to_string();
    }
    if secs < 3600 {
        return format!("{} 分钟前", secs / 60);
    }
    let then_local = then.to_offset(offset).date();
    let now_local = now.to_offset(offset).date();
    let days = (now_local - then_local).whole_days();
    if days == 0 {
        return format!("{} 小时前", secs / 3600);
    }
    if days == 1 {
        return "昨天".to_string();
    }
    if days <= 30 {
        return format!("{days} 天前");
    }
    format!(
        "{:04}-{:02}-{:02}",
        then_local.year(),
        then_local.month() as u8,
        then_local.day()
    )
}
```

- [ ] **Step 6: 跑测试确认通过**

Run:
```bash
cargo test -p mullion-app localtime:: 2>&1 | grep -E "test result|FAILED|panicked"
```
Expected: `test result: ok.`，6 条新测试全过。

- [ ] **Step 7: 逐条做变异自证**

对上面每条标了「自证会变红」的测试，按注释里写的那一处改动改掉 → 跑 → 确认**只有那一条**红 → 改回。特别是 `yesterday_is_decided_by_the_local_day_boundary_not_the_utc_one`：去掉两处 `to_offset(offset)` 之后必须变红，否则这条测试是恒绿的。

**先 `git commit` 再做变异**（记忆里两次被 `git checkout` 吞掉未提交编辑）：变异改完用 `git checkout -- <file>` 还原。

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/mullion-app/src/localtime.rs
git commit -m "feat(app): 相对时间文案,「昨天」按本地日界判 (F234)

time 加 parsing feature。判据放在纯函数里:now 与 offset 都是入参,
否则断言会随真实时钟在跨整点时随机变红。
跑了 localtime::tests 六条,含本地日界那条的变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 2: `project::matches()` 搜索判据

**Files:**
- Modify: `crates/mullion-app/src/project.rs`（在 `node_for` 之后追加）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/project.rs` 的 `#[cfg(test)] mod tests` 里追加。若该文件的 tests 模块里还没有构造 `SessionRecord` 的辅助函数，一并加上：

```rust
    fn sess(id: u64, name: &str, host: &str) -> mullion_store::SessionRecord {
        mullion_store::SessionRecord {
            id: mullion_store::SessionId(id),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: host.into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn pr(name: &str, dir: &str, nodes: &[u64]) -> mullion_store::ProjectRecord {
        mullion_store::ProjectRecord {
            id: mullion_store::ProjectId(1),
            name: name.into(),
            note: String::new(),
            nodes: nodes.iter().map(|n| mullion_store::SessionId(*n)).collect(),
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
        }
    }

    /// 空查询放行全部 —— 调用方不用特判「还没输字」。
    #[test]
    fn an_empty_query_lets_every_project_through() {
        assert!(matches(&pr("接口", "/srv/api", &[]), "", &[]));
        assert!(matches(&pr("接口", "/srv/api", &[]), "   ", &[]));
    }

    #[test]
    fn name_and_directory_both_match_case_insensitively() {
        let p = pr("API 网关", "/srv/Api", &[]);
        assert!(matches(&p, "api", &[]));
        assert!(matches(&p, "/SRV", &[]));
        assert!(!matches(&p, "数据库", &[]));
    }

    /// 用户记得住的常是机器名或 IP 尾数,不是当初给活起的名字 ——
    /// 与 `session_manager::list::matches` 收 host/tags 是同一条理由。
    ///
    /// 自证会变红:把 `p.nodes.iter().any(..)` 那一整段删掉。
    #[test]
    fn a_project_is_found_by_the_name_or_host_of_any_node_it_can_dial() {
        let ss = vec![sess(7, "web01", "10.0.0.9"), sess(8, "web02", "10.0.0.10")];
        let p = pr("接口", "/srv/api", &[7, 8]);
        assert!(matches(&p, "web02", &ss), "按节点会话名没搜到");
        assert!(matches(&p, "0.0.10", &ss), "按节点主机没搜到");
    }

    /// 只收**这个项目自己的**节点。收全表的话,任意一条会话名都能把
    /// 所有项目一起捞出来,搜索等于失效。
    ///
    /// 自证会变红:把 `p.nodes.iter().any(|id| sessions.iter().any(|s| s.id == *id && ..))`
    /// 换成 `sessions.iter().any(|s| ..)`(丢掉 id 比对)。
    #[test]
    fn a_session_that_is_not_a_node_of_this_project_never_makes_it_match() {
        let ss = vec![sess(7, "web01", "10.0.0.9"), sess(9, "db01", "10.0.0.20")];
        let p = pr("接口", "/srv/api", &[7]);
        assert!(!matches(&p, "db01", &ss), "不是这个项目的节点也命中了");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run:
```bash
cargo test -p mullion-app project::tests 2>&1 | tail -20
```
Expected: 编译失败，`cannot find function matches`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/project.rs` 的 `node_for` 之后追加：

```rust
/// F233:一个项目是否命中搜索词。空查询(trim 后为空)放行全部。
///
/// 匹配**项目名 / 目录 / 每一条节点会话的名字与主机**,大小写不敏感。
/// 收节点是因为用户记得住的常是机器名或 IP 尾数,不是当初给活起的名字
/// —— 与 `session_manager::list::matches` 收 host/tags 是同一条理由。
///
/// 收**全部** `nodes` 而不只是首选:多节点正是「同一台机器的等价路线」,
/// 用户搜哪条路线的名字都该找到这个活。
///
/// 只看这个项目自己的节点(`s.id == *id`)。丢掉 id 比对的话,任意一条会话
/// 名都能把全部项目一起捞出来 —— 搜索仍然「有反应」,但等于失效。
///
/// **不收 `note`**:F237 把说明改成了多行,长文本参与匹配会让搜索命中一堆
/// 用户在列表上看不见的东西。
pub fn matches(
    p: &mullion_store::ProjectRecord,
    query: &str,
    sessions: &[mullion_store::SessionRecord],
) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if p.name.to_lowercase().contains(&q) || p.dir.to_lowercase().contains(&q) {
        return true;
    }
    p.nodes.iter().any(|id| {
        sessions.iter().any(|s| {
            s.id == *id
                && (s.identity.name.to_lowercase().contains(&q)
                    || s.connection.host.to_lowercase().contains(&q))
        })
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

Run:
```bash
cargo test -p mullion-app project::tests 2>&1 | grep -E "test result|FAILED"
```
Expected: `test result: ok.`

- [ ] **Step 5: 变异自证**

按两条注释里写的改法各改一次，确认对应测试变红、其余不变，然后 `git checkout -- crates/mullion-app/src/project.rs` 还原。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/project.rs
git commit -m "feat(app): 项目搜索判据,连节点会话名与主机一起匹配 (F233)

零 UI 纯函数,三处列表共用一份。不收 note:F237 把说明改多行之后,
长文本参与匹配会命中一堆用户在列表上看不见的东西。
跑了 project::tests 四条,含「不是本项目的节点不该命中」的变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: `project::fresh_project_name()`

**Files:**
- Modify: `crates/mullion-app/src/project.rs`（紧接 `matches` 之后）

- [ ] **Step 1: 写失败的测试**

追加到同一个 `mod tests`：

```rust
    /// 一个项目都没有时就是「新项目」,不带后缀。
    #[test]
    fn the_first_new_project_has_no_suffix() {
        assert_eq!(fresh_project_name(&[]), "新项目");
    }

    /// `validate` 要求项目名全局唯一。不去重就会在盘上建出一条**必然存不
    /// 进去**的记录:列表里两行同名、右栏「保存」灰着,而用户看不出为什么。
    ///
    /// 自证会变红:把整个函数改成恒返回 `"新项目".to_string()`。
    #[test]
    fn a_clashing_name_gets_the_next_free_number() {
        let ps = vec![pr("新项目", "/a", &[])];
        assert_eq!(fresh_project_name(&ps), "新项目 2");
    }

    /// 找**第一个空号**,不是 max+1:删掉「新项目」再点添加,给出的应该是
    /// 「新项目」,而不是跳过一堆空号变成「新项目 7」。
    ///
    /// 自证会变红:把实现改成先数出最大后缀再 +1。
    #[test]
    fn the_lowest_free_number_is_reused_after_a_deletion() {
        let mut a = pr("新项目 2", "/a", &[]);
        a.id = mullion_store::ProjectId(2);
        let mut b = pr("新项目 3", "/b", &[]);
        b.id = mullion_store::ProjectId(3);
        // 「新项目」被删了 → 应该把它让出来的号补回去。
        assert_eq!(fresh_project_name(&[a, b]), "新项目");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app project::tests 2>&1 | tail -10`
Expected: 编译失败，`cannot find function fresh_project_name`。

- [ ] **Step 3: 实现**

```rust
/// F236:「+ 添加项目」用的默认名。
///
/// 「新项目」,撞名就往后找**第一个空号**(「新项目 2」「新项目 3」…)。
/// 不是 max+1:删掉「新项目」再点添加,给出的应该是「新项目」,而不是跳过
/// 一堆空号变成「新项目 7」。
///
/// 为什么必须去重:`mullion_store::validate_project` 要求项目名全局唯一,
/// 而 `ProjectIntent::Add` 是**立刻落盘**的。不去重就会在盘上建出一条必然
/// 存不进去的记录 —— 列表里两行同名、右栏「保存」灰着,用户看不出为什么。
pub fn fresh_project_name(existing: &[mullion_store::ProjectRecord]) -> String {
    const BASE: &str = "新项目";
    let taken = |cand: &str| existing.iter().any(|p| p.name == cand);
    if !taken(BASE) {
        return BASE.to_string();
    }
    (2..)
        .map(|n| format!("{BASE} {n}"))
        .find(|cand| !taken(cand))
        .expect("2.. 是无穷序列,find 必然返回")
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app project::tests 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 变异自证 + Commit**

```bash
git add crates/mullion-app/src/project.rs
git commit -m "feat(app): 新建项目的默认名,撞名补第一个空号 (F236)

Add 是立刻落盘的,不去重会在盘上建出一条必然存不进去的记录。
跑了 project::tests 三条,含「补空号而不是 max+1」的变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: 提升命中高亮的可见性（纯机械）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:17`
- Modify: `crates/mullion-app/src/ui/session_manager/highlight.rs:19`
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（`fn paint_highlighted` 那一行）

- [ ] **Step 1: 三处可见性各改一个词**

`session_manager/mod.rs:17`：
```rust
mod highlight;
```
改为：
```rust
/// F233:项目列表也要标命中片段,所以这个模块从 `mod` 提到 `pub(crate) mod`。
///
/// **只提可见性,不搬家**:搬到一个中立模块更干净,但这个 crate 里有一大批
/// 「读源码断言」式的守护测试,搬运会把它们成批打红,而本切片的范围不是
/// 重构会话管理器(Scope Discipline)。代价是 `ui::project_row` 要
/// `use crate::ui::session_manager::highlight` —— 依赖方向上看着别扭,
/// 但那是真实的复用关系,不是错误。
pub(crate) mod highlight;
```

`session_manager/highlight.rs:19`：
```rust
pub(super) fn segments(text: &str, query: &str) -> Vec<(String, bool)> {
```
改为：
```rust
pub(crate) fn segments(text: &str, query: &str) -> Vec<(String, bool)> {
```

`session_manager/list.rs` 的 `fn paint_highlighted`：把
```rust
fn paint_highlighted(
```
改为
```rust
pub(crate) fn paint_highlighted(
```
并在它的文档注释末尾追加一句：
```rust
/// F233:项目列表(`ui::project_row`)也用这一份。两处各画一遍分段着色的话,
/// 命中色、截断策略、CJK 混排的对齐会各错一次。
```

- [ ] **Step 2: 编译确认没有别的调用点被打破**

Run:
```bash
cargo check -p mullion-app 2>&1 | grep -E "^error|^warning: unused" | head
```
Expected: 无输出（可见性放宽不会破坏任何调用点；`pub(crate)` 的未使用项在本 Task 里还没有外部调用者，但它们在 crate 内部已有调用者，不会触发 dead_code）。

- [ ] **Step 3: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "refactor(app): 命中高亮的切分与绘制提到 crate 可见 (F233)

只提可见性不搬家:这个 crate 有一大批读源码断言的守护测试,
搬运会成批打红,而本切片范围不是重构会话管理器。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: 新建共享 `ui::project_row`

**Files:**
- Create: `crates/mullion-app/src/ui/project_row.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（模块声明处，`pub mod project_pick;` 那一行附近）

- [ ] **Step 1: 挂上模块**

在 `crates/mullion-app/src/ui/mod.rs` 的 `pub mod project_pick;`（第 24 行附近）之后加一行：
```rust
pub mod project_row;
```

- [ ] **Step 2: 写文件（含测试）**

新建 `crates/mullion-app/src/ui/project_row.rs`：

```rust
//! F233/F234:三处项目列表共用的**一行**。
//!
//! 为什么抽出来:改之前三处各写一份 —— 项目管理器左栏是
//! `selectable_label`,启动页是 egui 部件嵌套 + `interact`,pane 切换弹窗是
//! 纯 painter 手绘。本切片要往行里同时加搜索命中高亮和相对时间,三份实现
//! 各改一遍必然漂移。而「顺序」这件小得多的事,项目里已经为它写了三条注释
//! 反复强调复用 `by_recent_access`(理由:两处顺序不一样用户第一眼就看得
//! 出来)。行的**内容**比顺序更显眼。
//!
//! **手绘而不是 egui 部件拼装**:两行文字 + 右对齐的时间列 + 命中分段着色,
//! `selectable_label` 一样都做不到;而 F141 那条「侧栏本地栏一行都点不中」
//! 的教训要求判定矩形罩住整行 —— 手绘天然是「先 `allocate_exact_size` 整行
//! 矩形,再 `interact`」。
//!
//! **本批不留图标槽位**(F238 才加图标)。留着的话界面上会出现一列 32px 的
//! 空白,而那时还没有任何图标功能 —— 正是项目一贯反对的「对用户没有可操作
//! 性的占位」。代价:F238 会让行内坐标整体右移,本文件的落点测试要跟着改。

use mullion_store::{ProjectRecord, SessionRecord};

use crate::theme::{self, Theme};

/// 行高。两行文字加上下内边距。
///
/// 与会话侧 `session_manager::list::row_h(Density::Full)` 的 48.0 **刻意一致**
/// —— 两个列表在同一个程序里,行高不一样会显得像两个软件。
pub const ROW_H: f32 = 48.0;

/// 灯的槽位中心距行左边缘(逻辑点)。
const LAMP_X: f32 = 14.0;
/// 文字左边界 = 灯槽右沿 + 一点呼吸。**恒定**:有灯没灯的行文字左边界必须
/// 对齐,而灯是三态恒画的,这里只是把常量写死免得两行各算一次。
const TEXT_X: f32 = 26.0;
/// 文字区距行右边缘的留白。
const TEXT_RIGHT_PAD: f32 = 8.0;
/// 名称行顶距行顶。
const NAME_TOP: f32 = 6.0;
/// 副标题行顶距行顶。
const SUB_TOP: f32 = 27.0;
/// 名称字号。
const NAME_SIZE: f32 = 14.0;
/// 副标题与时间字号。
const SUB_SIZE: f32 = 11.0;

/// 画一行要的全部输入。
///
/// `now` 由调用方**一帧取一次**传进来 —— 每行各取一次 `now_utc()` 等于每帧
/// 几十次系统调用,而这个项目为了空闲期的 CPU 花了整整八个切片。
pub struct Row<'a> {
    pub project: &'a ProjectRecord,
    pub lamp: crate::project::Lamp,
    /// 用来把节点 id 解析成机器名。三处调用方手上都有全表。
    pub sessions: &'a [SessionRecord],
    /// 当前搜索词。空串 = 没在搜索,`segments` 会返回整段不高亮。
    pub query: &'a str,
    /// 这一行是不是右栏正在编辑的那个。只有项目管理器左栏会传 `true`。
    pub selected: bool,
    pub now: time::OffsetDateTime,
    /// 这是哪个列表(`"manager"` / `"launcher"` / `"pick"`)。
    ///
    /// **必须区分**:项目管理器是弹窗、启动页是 `CentralPanel`,两者可以
    /// 同时在屏幕上,同一个项目在两处的行会算出同一个 egui id,交互互相打架。
    pub list: &'static str,
}

/// 一行的副标题:`目录 · 节点名`。
///
/// 节点名解析不出来(会话被删了、或项目还没勾节点)时**只显示目录**,
/// 不显示「(未知)」一类占位:那句话对用户没有任何可操作性,而目录本身
/// 已经足以认出这是哪个活。
///
/// 选节点走 [`crate::project::node_for`] —— 和 `plan_open` 真拨号时用的是
/// **同一个函数**。各写一份的话,列表上写着 A、点下去连的是 B。
pub fn subtitle(p: &ProjectRecord, sessions: &[SessionRecord]) -> String {
    let name = crate::project::node_for(p)
        .and_then(|id| sessions.iter().find(|s| s.id == id))
        .map(|s| s.identity.name.as_str());
    match name {
        Some(n) => format!("{} · {}", p.dir, n),
        None => p.dir.clone(),
    }
}

/// 行尾那一列时间。从没打开过的说「从未打开」,不留空 —— 空白会被读成
/// 「这一列坏了」,而「从未打开」本身就是用户要的信息。
pub fn time_text(p: &ProjectRecord, now: time::OffsetDateTime) -> String {
    match p.last_accessed_at.as_deref() {
        Some(s) => crate::localtime::relative(s, now, crate::localtime::offset()),
        None => "从未打开".to_string(),
    }
}

/// 画一行,返回它的 `Response`。调用方自己判 `clicked()`。
pub fn show(ui: &mut egui::Ui, t: &Theme, row: &Row) -> egui::Response {
    let w = ui.available_width();
    // 先占整行矩形再 `interact`:靠里面某个 label 的 `sense` 的话,只有字上
    // 那几十个像素点得中 —— F141 那条「侧栏本地栏一行都点不中」就是这么来的,
    // 而它的症状是**完全静默**:界面画得好好的,点了没反应。
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::hover());
    let id = egui::Id::new(("project_row", row.list, row.project.id.0));
    let resp = ui.interact(rect, id, egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    let p = ui.painter();
    let rounding = egui::Rounding::same(4.0);
    if row.selected {
        p.rect_filled(rect, rounding, theme::c32(t.sunken_bg));
    } else if resp.hovered() {
        p.rect_filled(rect, rounding, theme::c32(t.panel_head));
    }

    // 灯。走 `ui::icon` 自绘、**不写字符**:●/○/◐ 都在 GBK 外,egui 的两级
    // 字体链画不出来就是豆腐块,而那在 Linux 开发机上多半是正常的(T9)。
    // 颜色不承担区分职责,形状才是(同 `project_manager::lamp_dot`)。
    let side = ROW_H * 0.28;
    let dot = egui::Rect::from_center_size(
        egui::pos2(rect.left() + LAMP_X, rect.center().y),
        egui::vec2(side, side),
    );
    let (glyph, color) = match row.lamp {
        crate::project::Lamp::Lit => (crate::ui::icon::Glyph::LampLit, t.ok),
        crate::project::Lamp::Dark => (crate::ui::icon::Glyph::LampDark, t.fg_muted),
        crate::project::Lamp::Unknown => (crate::ui::icon::Glyph::LampUnknown, t.warn),
    };
    p.extend(crate::ui::icon::shapes(
        dot,
        glyph,
        egui::Stroke::new(1.4, theme::c32(color)),
    ));

    let text_left = rect.left() + TEXT_X;
    let text_avail = (rect.right() - TEXT_RIGHT_PAD - text_left).max(0.0);

    // 时间先量宽度:名称的可用宽度要**先**扣掉它,不扣的话长项目名会一路
    // 截断到右边缘,把时间整列挤出行外(整条看不见)。
    let time = time_text(row.project, row.now);
    let time_galley = p.layout_no_wrap(
        time,
        egui::FontId::proportional(SUB_SIZE),
        theme::c32(t.fg_muted),
    );
    let time_w = time_galley.size().x;
    p.galley(
        egui::pos2(
            rect.right() - TEXT_RIGHT_PAD - time_w,
            rect.top() + NAME_TOP + (NAME_SIZE - SUB_SIZE) / 2.0,
        ),
        time_galley,
        theme::c32(t.fg_muted),
    );

    // 名称 + 副标题都过命中着色。副标题里就是目录和节点名 —— 搜「web01」
    // 命中的正是那里,不标出来的话用户完全不知道这一行为什么会出现。
    crate::ui::session_manager::list::paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + NAME_TOP),
        &row.project.name,
        row.query,
        egui::FontId::proportional(NAME_SIZE),
        theme::c32(t.fg_strong),
        t,
        (text_avail - time_w - 8.0).max(0.0),
    );
    crate::ui::session_manager::list::paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + SUB_TOP),
        &subtitle(row.project, row.sessions),
        row.query,
        egui::FontId::proportional(SUB_SIZE),
        theme::c32(t.fg_muted),
        t,
        text_avail,
    );

    // 自绘的行在 accesskit 树里是个没名字的空节点 —— 补一个名字,给屏幕
    // 阅读器和 F100 的自动候选用(同 `icon_button` / `lamp_dot` 的理由)。
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &row.project.name)
    });
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{ProjectId, Protocol, SessionId};

    fn proj(id: u64, name: &str, dir: &str, accessed: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: vec![SessionId(7)],
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
        }
    }

    fn sess(id: u64, name: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::parse(
            "2026-09-08T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .expect("测试基准时间写错了")
    }

    /// 一行必须同时说清**在哪台机器上的哪个目录** —— 只有项目名的话,
    /// 「api」和「api(测试机)」这种命名在列表里根本分不出来。
    #[test]
    fn a_row_names_both_the_directory_and_the_node_it_will_dial() {
        let s = vec![sess(7, "web01")];
        assert_eq!(
            subtitle(&proj(1, "接口", "/srv/api", None), &s),
            "/srv/api · web01"
        );
    }

    /// 节点解析不出来(会话被别的实例删了)只显示目录,**不显示占位文字**。
    #[test]
    fn a_row_whose_node_is_gone_still_says_which_directory_it_is() {
        assert_eq!(subtitle(&proj(1, "接口", "/srv/api", None), &[]), "/srv/api");
    }

    /// 从没打开过的行要说「从未打开」,不能留空 —— 空白会被读成
    /// 「这一列坏了」,而「从未打开」本身就是用户要的信息。
    ///
    /// 自证会变红:把 `None` 那一臂改成 `String::new()`。
    #[test]
    fn a_project_that_was_never_opened_says_so_instead_of_leaving_a_blank() {
        assert_eq!(time_text(&proj(1, "接口", "/srv/api", None), now()), "从未打开");
    }

    #[test]
    fn a_project_opened_this_morning_shows_a_relative_time() {
        let p = proj(1, "接口", "/srv/api", Some("2026-09-08T09:00:00Z"));
        assert_eq!(time_text(&p, now()), "3 小时前");
    }

    const SCREEN_W: f32 = 600.0;

    /// **整行**可点,不是只有那几个字可点。
    ///
    /// 列表里一行有一大片空白(名字右边到时间列之间),用户瞄准的是「那一
    /// 条」,落点几乎不可能正好在字上。F141 那条「侧栏本地栏一行都点不中」
    /// 就是判定矩形只罩住了内容 —— 症状**完全静默**:界面画得好好的,
    /// 点了没反应。
    ///
    /// 自证会变红:把 `show` 里 `ui.interact(rect, ..)` 的矩形换成
    /// `egui::Rect::from_min_size(rect.min, egui::vec2(60.0, ROW_H))`。
    #[test]
    fn the_whole_row_is_clickable_not_just_the_name() {
        assert!(clicked_at(0.85), "点在行的右半边没反应 —— 判定矩形没罩住整行");
        assert!(clicked_at(0.1), "点在行的左端没反应");
    }

    /// 跑两帧,在行矩形宽度的 `frac` 处点一下,返回是否点中。
    ///
    /// **两帧**:`CentralPanel` 首帧 `fade_in` 只记 `Shape::Noop`
    /// (同 `restored` / `files_panel` / `launcher` 那边)。
    fn clicked_at(frac: f32) -> bool {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let base = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let row = |query: &'static str| Row {
            project: &p,
            lamp: crate::project::Lamp::Unknown,
            sessions: &ss,
            query,
            selected: false,
            now: now(),
            list: "test",
        };
        let mut rect = egui::Rect::NOTHING;
        for _ in 0..2 {
            ctx.run(base(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    rect = show(ui, &t, &row("")).rect;
                });
            });
        }
        let pos = egui::pos2(rect.left() + rect.width() * frac, rect.center().y);
        let mut input = base();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut hit = false;
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                hit = show(ui, &t, &row("")).clicked();
            });
        });
        hit
    }

    /// 搜索命中的片段要染成 accent。**过滤了却标不出命中在哪,用户会以为
    /// 搜索坏了**(走查 22 的原话)——尤其在项目列表里,搜「web01」命中的
    /// 是副标题里的节点名,不标出来的话这一行为什么会出现完全没线索。
    ///
    /// 判据是「画出来的文字里有 accent 色的那一段」,不是「调用了
    /// `paint_highlighted`」—— 后者是读源码,换个函数名就恒绿。
    ///
    /// 自证会变红:把两处 `paint_highlighted` 的 `query` 实参换成 `""`。
    #[test]
    fn the_matching_piece_is_tinted_so_the_user_can_see_why_this_row_showed_up() {
        let t = crate::theme::MULLION_DARK;
        let accent = crate::theme::c32(t.accent);
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                &Row {
                                    project: &p,
                                    lamp: crate::project::Lamp::Unknown,
                                    sessions: &ss,
                                    query: "web01",
                                    selected: false,
                                    now: now(),
                                    list: "test",
                                },
                            );
                        });
                    },
                )
                .shapes;
        }
        assert!(
            shapes.iter().any(|cs| has_accent_text(&cs.shape, accent)),
            "命中「web01」的那一段没有染成 accent —— 用户看不出这一行为什么会出现"
        );
    }

    /// 这堆 shape 里有没有一段用 accent 色画的文字。
    ///
    /// `Shape::Text` 的每个 section 各带自己的 `format.color`,所以要下钻到
    /// `galley.job.sections` —— 只看 `TextShape::fallback_color` 的话,分段
    /// 着色的命中色根本不在那里。
    fn has_accent_text(shape: &egui::Shape, accent: egui::Color32) -> bool {
        match shape {
            egui::Shape::Vec(v) => v.iter().any(|s| has_accent_text(s, accent)),
            egui::Shape::Text(ts) => ts
                .galley
                .job
                .sections
                .iter()
                .any(|s| s.format.color == accent),
            _ => false,
        }
    }
}
```

- [ ] **Step 3: 编译并跑测试**

Run:
```bash
cargo test -p mullion-app project_row:: 2>&1 | grep -E "test result|FAILED|^error"
```
Expected: `test result: ok.`，6 条测试全过。

若 `egui::Shape::Text(ts)` 里 `ts.galley.job.sections` 的路径编译不过，先读实际签名再改：
```bash
grep -n "pub struct Galley" -A12 /home/ubuntu/.cargo/registry/src/*/epaint-0.30.0/src/text/text_layout_types.rs
```

- [ ] **Step 4: 变异自证**

三条标了「自证会变红」的各改一次，确认只有对应那条红。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/project_row.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 三处项目列表共用一份手绘行 (F233/F234)

灯 + 名称 + 右对齐相对时间 + 副标题,名称与副标题都过命中着色。
整行 allocate 后再 interact —— F141 那条「一行都点不中」的教训。
本批不留图标槽位:界面上一列无法解释的空白比一次坐标改动更糟。
跑了 project_row::tests 六条,含整行可点与命中着色两条变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: 项目管理器左栏三段式重排

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 字段）
- Modify: `crates/mullion-app/src/ui/project_manager.rs:132-191`（`list_column`）+ `show` 的签名

- [ ] **Step 1: 改 `UiState` 字段**

`crates/mullion-app/src/ui/mod.rs:277` 附近，把
```rust
    /// 「新建项目」输入框的跨帧缓冲。
    pub project_name_buf: String,
```
换成
```rust
    /// F233:项目管理器左栏搜索框的跨帧缓冲。
    ///
    /// 这个字段原来是「新建项目」的名字输入框(`project_name_buf`)。F236 把
    /// 新建改成了一键「+ 添加项目」——名字由 `project::fresh_project_name`
    /// 现算,那个输入框整个让位给搜索。
    pub project_search: String,
    /// F236:下一帧要把焦点打到右栏「名称」框上(刚新建完)。
    ///
    /// 用一位标志而不是在 app 侧直接 `request_focus`:那个 `Response` 只在
    /// 渲染闭包里存在,而新建是在闭包外施加的。
    pub project_focus_name: bool,
```

- [ ] **Step 2: 写失败的测试**

在 `crates/mullion-app/src/ui/project_manager.rs` 的 `mod tests` 里追加：

```rust
    /// 跑两帧,把左栏画出来的全部文字收上来。
    fn left_column_texts(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions: Vec<SessionRecord> = Vec::new();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, &mut ui_state, projects, &lamps, &sessions, None);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 搜索词把不匹配的行滤掉。
    ///
    /// 自证会变红:把 `list_column` 里的 `.filter(|p| crate::project::matches(..))`
    /// 删掉。
    #[test]
    fn the_left_column_hides_projects_that_do_not_match_the_query() {
        let ps = vec![proj(1, "接口", None), proj(2, "数据库", None)];
        let texts = left_column_texts(&ps, "接口");
        assert!(texts.iter().any(|s| s == "接口"), "命中的行不见了:{texts:?}");
        assert!(
            !texts.iter().any(|s| s == "数据库"),
            "没命中的行还在:{texts:?}"
        );
    }

    /// 搜不到任何东西时列表是一整片空白 —— 用户分不清「没有匹配」和
    /// 「项目都没了」。给一句话 + 一个回到全部列表的出口(走查 22)。
    ///
    /// 自证会变红:把那个空态分支删掉。
    #[test]
    fn a_query_that_matches_nothing_says_so_and_offers_a_way_back() {
        let ps = vec![proj(1, "接口", None)];
        let joined = left_column_texts(&ps, "根本没有这个").join(" ");
        assert!(joined.contains("没有匹配的项目"), "没给空态说明:{joined}");
        assert!(joined.contains("清空搜索"), "没给回到全部列表的出口:{joined}");
    }

    /// 新建按钮的文案与形态:整宽、写「+ 添加项目」。
    ///
    /// 判据读的是**渲染出来的文字**,不是源码里的字面量 —— 后者换个拼法
    /// 就恒绿。
    ///
    /// 自证会变红:把按钮文案改回「添加」。
    #[test]
    fn the_add_button_spells_out_that_it_makes_a_project() {
        let joined = left_column_texts(&[], "").join(" ");
        assert!(joined.contains("+ 添加项目"), "按钮文案不对:{joined}");
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-app project_manager:: 2>&1 | tail -20`
Expected: 编译失败（`project_search` 字段不存在 / `matches` 未接线），或三条新测试红。

- [ ] **Step 4: 重写 `list_column`**

把 `crates/mullion-app/src/ui/project_manager.rs` 的 `list_column` 整个函数替换为：

```rust
/// 左栏:搜索框 / 列表 / 底部「+ 添加项目」三段式。
///
/// 与会话管理器左栏同构(那边是搜索框 / 分组树 / 底部「+ 新建」)。底部按钮
/// 用 `TopBottomPanel::bottom(..).show_inside(ui)` 先占位:egui 的面板布局
/// 保证面板先分配自己的高度、再把外层 `ui` 的可用区底边收缩到面板上沿 ——
/// 直接按顺序画的话,项目一多列表就会把按钮顶出可视区,而那是唯一的新建入口。
///
/// 宽度从原来的 `FIELD_W_S * 2`(192)提到 `LIST_W`(300):行里现在有副标题
/// 和右对齐的时间列,192 装不下,长项目名会被截成一两个字。
fn list_column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    lamps: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
    sessions: &[SessionRecord],
    now: time::OffsetDateTime,
) {
    use crate::ui::metrics::{field_w, FIELD_W_L, SP_S};
    ui.vertical(|ui| {
        ui.set_width(crate::ui::session_manager::LIST_W);
        // 搜索框独占一整行:原来它旁边挂着「添加」按钮,只剩 96px,
        // 一个路径片段都打不下。
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut ui_state.project_search)
                .hint_text(crate::theme::hint_text(t, "搜索项目名 / 目录 / 节点"))
                .desired_width(w),
        );
        crate::ui::annotate::mark(ui.ctx(), "项目管理器/左栏/搜索框", search.rect);
        ui.add_space(SP_S);

        egui::TopBottomPanel::bottom("project_list_bottom")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.add_space(SP_S);
                // 撞满整宽:视觉重点靠**位置和尺寸**,不靠颜色。
                // 全场唯一一个 accent 实心按钮是会话编辑器的「保存并连接」
                // (`editor.rs`),再加一颗会把那个层级搅浑。
                let b = ui.add_sized(
                    egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                    egui::Button::new("+ 添加项目"),
                );
                crate::ui::annotate::mark(ui.ctx(), "项目管理器/左栏/添加项目", b.rect);
                if b.clicked() {
                    ui_state.project_intent = Some(ProjectIntent::Add(
                        crate::project::fresh_project_name(projects),
                    ));
                }
            });

        if projects.is_empty() {
            ui.label(
                egui::RichText::new(
                    "还没有项目。项目 = 一台机器上的一个开发目录 + 一个专属 tmux 会话,\
                     打开它就回到那个活。",
                )
                .color(crate::theme::c32(t.fg_muted)),
            );
            return;
        }
        // 顺序**复用** `by_recent_access`,过滤复用 `project::matches` ——
        // 三处列表各写一份的话,同一个搜索词在两个界面给出不同结果,而用户
        // 几分钟内就会都看到一遍。
        let rows: Vec<&ProjectRecord> = by_recent_access(projects)
            .into_iter()
            .filter(|p| crate::project::matches(p, &ui_state.project_search, sessions))
            .collect();
        if rows.is_empty() {
            ui.label(
                egui::RichText::new("没有匹配的项目").color(crate::theme::c32(t.fg_muted)),
            );
            ui.add_space(SP_S);
            if ui.button("清空搜索").clicked() {
                ui_state.project_search.clear();
            }
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("project_list")
            .show(ui, |ui| {
                for p in rows {
                    let lamp = lamps
                        .get(&p.id)
                        .copied()
                        .unwrap_or(crate::project::Lamp::Unknown);
                    let r = crate::ui::project_row::show(
                        ui,
                        t,
                        &crate::ui::project_row::Row {
                            project: p,
                            lamp,
                            sessions,
                            query: &ui_state.project_search,
                            selected: ui_state.project_selected == Some(p.id),
                            now,
                            list: "manager",
                        },
                    );
                    if r.clicked() {
                        ui_state.project_selected = Some(p.id);
                        ui_state.project_draft = Some(p.clone());
                    }
                }
            });
    });
}
```

- [ ] **Step 5: `show` 里取一次 `now` 并传下去**

把 `crates/mullion-app/src/ui/project_manager.rs` 的 `show` 函数体里的
```rust
    egui::Window::new("项目管理")
        .open(&mut open)
        .default_width(720.0)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                list_column(ui, t, ui_state, projects, lamps);
```
改为
```rust
    // 「现在几点」一帧取一次,不是每行取一次 —— 每行各调一次 `now_utc()`
    // 等于每帧几十次系统调用,而这个项目为了空闲期的 CPU 花了整整八个切片。
    let now = time::OffsetDateTime::now_utc();
    egui::Window::new("项目管理")
        .open(&mut open)
        .default_width(840.0)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                list_column(ui, t, ui_state, projects, lamps, sessions, now);
```

（`default_width` 从 720 提到 840 是因为左栏从 192 加宽到 300：不提的话右栏会从 442 缩到 334，「说明」多行框跟着变窄。）

- [ ] **Step 6: 确认 `LIST_W` 可见**

Run:
```bash
grep -n "LIST_W" crates/mullion-app/src/ui/session_manager/mod.rs | head -3
```
若 `LIST_W` 不是 `pub(crate)`，把它改成 `pub(crate) const LIST_W: f32 = 300.0;`，并在它的文档注释里追加：
```rust
/// F233:项目管理器左栏也用这一档 —— 两个左栏在同一个程序里,宽度不一样
/// 会显得像两个软件。
```

- [ ] **Step 7: 跑测试**

Run:
```bash
cargo test -p mullion-app project_manager:: 2>&1 | grep -E "test result|FAILED|^error"
```
Expected: `test result: ok.`

- [ ] **Step 8: 变异自证 + Commit**

```bash
git add crates/mullion-app/src/ui/project_manager.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 项目管理器左栏改三段式,搜索框独占一行 (F233/F236)

搜索框 / 列表 / 底部整宽「+ 添加项目」。按钮走 TopBottomPanel::bottom
先占位:直接按顺序画的话项目一多就把唯一的新建入口顶出可视区。
左栏 192→LIST_W(300),窗口 720→840。
跑了 project_manager::tests 三条新的,含空态出口与按钮文案的变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: 「+ 添加项目」接线 + 新建后焦点落到名称框

**Files:**
- Modify: `crates/mullion-app/src/app.rs:12033-12044`（`ProjectIntent::Add` 分支）
- Modify: `crates/mullion-app/src/ui/project_manager.rs`（`form_column` 的「名称」输入框）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加（与既有的 `project_edits_are_counted_as_touching_the_store` 同一个模块，紧挨着它放）：

```rust
    /// F236:新建项目的名字由 `fresh_project_name` 现算,不是从一个输入框里
    /// 读来的。
    ///
    /// 读输入框的旧写法在 F236 之后会永远拿到空串(那个框已经改成搜索框),
    /// 建出来的项目**名字是空的** —— 而 `validate_project` 会拒掉它,右栏
    /// 「保存」永远灰着,用户看不出为什么。
    ///
    /// 自证会变红:把 `fresh_project_name(store.projects())` 换回
    /// `self.ui.project_search.clone()`。
    #[test]
    fn a_new_project_gets_a_generated_name_not_whatever_is_in_the_search_box() {
        let src = include_str!("app.rs");
        let body = src
            .split_once("ProjectIntent::Add(name) => {")
            .expect("找不到 Add 分支")
            .1;
        let body = &body[..body.find("\n                            }\n").expect("找不到 Add 分支的结尾")];
        assert!(
            !body.contains("project_search"),
            "新建项目还在读搜索框的内容 —— 那个框已经不是名字框了"
        );
    }
```

同时在 `crates/mullion-app/src/ui/project_manager.rs` 的 `mod tests` 里追加：

```rust
    /// 新建完之后焦点要落到右栏「名称」框上 —— 否则用户得先用鼠标点进去
    /// 才能给这个活起名字,而「起名字」正是新建之后唯一要做的事。
    ///
    /// 自证会变红:把 `form_column` 里那段 `request_focus()` 删掉。
    #[test]
    fn right_after_adding_a_project_the_name_field_takes_focus() {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(1, "新项目", None);
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            project_focus_name: true,
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        for _ in 0..2 {
            ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, &mut ui_state, &ps, &lamps, &sessions, None);
            });
        }
        assert!(
            ctx.memory(|m| m.focused()).is_some(),
            "新建之后没有任何部件拿到焦点 —— 用户得先用鼠标点进名称框才能起名"
        );
        assert!(
            !ui_state.project_focus_name,
            "焦点标志没被消费 —— 会每帧抢一次焦点,用户点别的框都会被弹回来"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app -- project 2>&1 | grep -E "test result|FAILED"`
Expected: 两条新测试红。

- [ ] **Step 3: 改 `app.rs` 的 `Add` 分支**

把 `crates/mullion-app/src/app.rs:12033` 那一段改为：

```rust
                            crate::ui::project_manager::ProjectIntent::Add(name) => {
                                // 目录留空:新建那一刻还不知道要落在哪个目录,
                                // 逼用户在一个单行输入框里先想好路径是本末倒置。
                                // 右栏的「保存」按钮会拦住空目录。
                                //
                                // F236:名字由 `project::fresh_project_name` 现算
                                // (UI 侧传进来的就是它)。原来那个「新建项目」
                                // 输入框已经改成搜索框 —— 继续从那里读的话建出
                                // 来的项目名字是空的,而 `validate_project` 会拒
                                // 掉它,右栏「保存」永远灰着且没有任何解释。
                                let now = time::OffsetDateTime::now_utc()
                                    .format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_default();
                                let id = store.add_project(name, String::new(), &now);
                                self.ui.project_selected = Some(id);
                                self.ui.project_draft =
                                    store.projects().iter().find(|p| p.id == id).cloned();
                                // 新建之后唯一要做的事就是起名字 —— 把焦点直接
                                // 送过去,省掉一次「用鼠标点进那个框」。
                                self.ui.project_focus_name = true;
                            }
```

（名字已经由 `list_column` 用 `fresh_project_name` 算好塞进 `ProjectIntent::Add`，这里只消费，不再读任何 UI 缓冲。）

- [ ] **Step 4: 改 `form_column` 的名称框**

把 `crates/mullion-app/src/ui/project_manager.rs` 的
```rust
        ui.label("名称");
        let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(w));
        ui.end_row();
```
改为
```rust
        ui.label("名称");
        let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
        let name_resp = ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(w));
        // F236:刚新建完 —— 把焦点送进来并全选。
        //
        // **标志要 `take` 掉**:留着的话每帧都抢一次焦点,用户点右栏任何
        // 别的输入框都会被当场弹回名称框,而且这个状态没有自愈路径。
        if std::mem::take(&mut focus_name) {
            name_resp.request_focus();
            // 全选:默认名「新项目」是占位,用户第一个动作必然是把它删掉重打。
            // `TextEditState` 的游标区间是 egui 唯一能表达「全选」的地方
            // (`TextEdit` 自己没有 select-all 的构造项)。
            if let Some(mut st) =
                egui::widgets::text_edit::TextEditState::load(ui.ctx(), name_resp.id)
            {
                let n = draft.name.chars().count();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(n),
                )));
                st.store(ui.ctx(), name_resp.id);
            }
        }
        ui.end_row();
```

`form_column` 的签名要多收一个 `focus_name: &mut bool` 之类的入口。**做法**：在 `form_column` 顶部（`let Some(draft) = ui_state.project_draft.as_mut() else {...}` 之前）先把标志取出来：
```rust
    // 先取成局部量:下面 `draft` 会把 `ui_state` 可变借出去,那之后就再也
    // 摸不到 `ui_state` 的别的字段了。
    let mut focus_name = ui_state.project_focus_name;
```
并在函数末尾（`draft` 的借用结束之后，即 `form_column` 返回前）写回：
```rust
    ui_state.project_focus_name = focus_name;
```
若借用检查仍不通过（`draft` 借用贯穿整个函数体），改成在 `form_column` 的调用方 `show` 里处理：`show` 先 `let focus = std::mem::take(&mut ui_state.project_focus_name);`，把 `focus` 按值传给 `form_column`，`form_column` 内部只读。**优先用这个版本**——它没有借用问题，且「标志被消费」这件事在 `show` 里一眼可见。

即：
- `show` 里：`let focus_name = std::mem::take(&mut ui_state.project_focus_name);`，然后 `form_column(ui, t, ui_state, projects, sessions, table, focus_name)`
- `form_column` 签名末尾加 `focus_name: bool`，函数体里用 `if focus_name { ... }`

- [ ] **Step 5: 跑测试**

Run: `cargo test -p mullion-app -- project 2>&1 | grep -E "test result|FAILED|^error"`
Expected: `test result: ok.`

若 `egui::widgets::text_edit::TextEditState` 路径编译不过，核实：
```bash
grep -rn "TextEditState" /home/ubuntu/.cargo/registry/src/*/egui-0.30.0/src/widgets/text_edit/mod.rs
```

- [ ] **Step 6: 变异自证 + Commit**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/ui/project_manager.rs
git commit -m "feat(app): 一键新建项目,焦点直接落到名称框并全选 (F236)

名字由 fresh_project_name 现算 —— 原来那个输入框已经改成搜索框,
继续从那里读会建出名字为空、必然存不进去的记录。
焦点标志用完即 take:留着会每帧抢焦点,用户点别的框都被弹回来。
跑了 app::tests 与 project_manager::tests 各一条新的。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 8: 右栏 —— 说明多行 / 内容滚动 / 按钮固定底部

**Files:**
- Modify: `crates/mullion-app/src/ui/project_manager.rs`（`form_column`）

- [ ] **Step 1: 写失败的测试**

在 `project_manager.rs` 的 `mod tests` 里追加：

```rust
    /// F237:「说明」是多行框。
    ///
    /// 判据是**画出来的输入框高度**,不是源码里出现了 `multiline` ——
    /// 后者换个写法就恒绿。单行框的高度约等于一行文字 + 内边距(20 上下),
    /// 三行框必然显著高于它,取 40 当阈值有充足余量。
    ///
    /// 自证会变红:把 `multiline` 换回 `singleline`。
    #[test]
    fn the_note_field_is_tall_enough_to_hold_more_than_one_line() {
        let h = note_field_height();
        assert!(h > 40.0, "「说明」框只有 {h} 高 —— 还是单行的");
    }

    /// 画两帧,量「说明」输入框的高度。
    ///
    /// 靠 hint 文字锚点找不到它(有值时 hint 不画),所以给 draft 的 note
    /// 塞一个独特的值,再从 shape 里反查那段文字所在的 galley 高度。
    fn note_field_height() -> f32 {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut p = proj(1, "接口", None);
        p.note = "唯一说明串".into();
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        let mut out = 0.0f32;
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, &mut ui_state, &ps, &lamps, &sessions, None);
            });
            out = full
                .shapes
                .iter()
                .filter_map(|cs| galley_box(&cs.shape, "唯一说明串"))
                .fold(0.0, f32::max);
        }
        out
    }

    /// 找到画着 `label` 的那个 galley 所在的**裁剪矩形**高度 —— `TextEdit`
    /// 把内容画在自己的 clip rect 里,所以这个高度就是输入框的高度。
    fn galley_box(shape: &egui::Shape, label: &str) -> Option<f32> {
        match shape {
            egui::Shape::Vec(v) => v.iter().find_map(|s| galley_box(s, label)),
            egui::Shape::Text(ts) if ts.galley.text() == label => Some(ts.galley.rect.height()),
            _ => None,
        }
    }
```

> **注意**：`ts.galley.rect.height()` 量的是**文字本身**的高度，不是输入框外框。多行 `TextEdit` 的 galley 会按 `desired_rows` 撑到三行高（内容不足时用空行补），所以这条判据成立。跑之前先验证一次：若量出来的仍是单行高，改用 `Shape::Rect` 里最接近该文字位置的那个矩形。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app project_manager::tests::the_note_field 2>&1 | tail -10`
Expected: 红，高度约 14~20。

- [ ] **Step 3: 说明改多行**

把
```rust
        ui.label("说明");
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        ui.add(egui::TextEdit::singleline(&mut draft.note).desired_width(w));
        ui.end_row();
```
改为
```rust
        // 说明改多行(F237)。标签**顶对齐**:`Grid` 每行默认 `Align::Center`,
        // 3 行高的 multiline 旁边的短标签会被垂直居中,跟上面几行的标签对不齐
        // (会话侧「备注」为同一个原因写过这段,走查 P2-17)。
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.label("说明");
        });
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        ui.add(
            egui::TextEdit::multiline(&mut draft.note)
                .desired_rows(3)
                .desired_width(w),
        );
        ui.end_row();
```

- [ ] **Step 4: 右栏套滚动区 + 按钮钉底**

在 `form_column` 里，把「按钮行」那一段（从 `ui.add_space(SP_M);` 开始，到 `if dirty { ... }` 结束）搬进一个 `TopBottomPanel::bottom`，其余内容包进 `ScrollArea::vertical`。具体：

在 `form_column` 的 `let id = draft.id;` 之后插入一层结构。整个函数体（`let Some(draft) = ... else {...}` 之后）改成下面的形状：

```rust
    let id = draft.id;
    // 校验与「能不能开」在**画之前**先算好:下面按钮行在
    // `TopBottomPanel::bottom` 里,而那一段的闭包跑在滚动区之前,
    // 拿不到滚动区里那份 `draft` 的借用。
    let issue = mullion_store::validate_project(draft, projects, sessions).err();
    let blank = draft.name.trim().is_empty() || draft.dir.trim().is_empty();
    // F223:「打开」拿的是**库里那份**,不是右栏这份草稿 —— 草稿改了没保存
    // 就打开的话,连过去的是草稿里的目录/tmux 名,而配置库里根本没这回事,
    // 下次再打开又变回去。所以草稿脏了就先不让开。
    let stored = projects.iter().find(|p| p.id == id);
    let dirty = stored != Some(&*draft);
    let openable = stored.is_some_and(|p| !p.nodes.is_empty()) && !dirty;
    let save_draft = draft.clone();

    // 按钮行钉在底部,**不跟着内容滚**。右栏加了三行说明之后内容约 640px,
    // 在 1080p + 150% 缩放(逻辑高 720)下,按顺序画的话「保存 / 打开 /
    // 删除项目」会被顶出屏幕 —— 而那是这个界面唯一的出口。
    egui::TopBottomPanel::bottom("project_form_bottom")
        .frame(egui::Frame::none())
        .show_inside(ui, |ui| {
            ui.add_space(SP_M);
            if let Some(ref e) = issue {
                ui.label(
                    egui::RichText::new(issue_text(e, projects, sessions))
                        .color(crate::theme::c32(t.danger_text)),
                );
                ui.add_space(SP_XS);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(issue.is_none() && !blank, egui::Button::new("保存"))
                    .clicked()
                {
                    ui_state.project_intent =
                        Some(ProjectIntent::Save(id, Box::new(save_draft.clone())));
                }
                ui.add_space(SP_S);
                if ui
                    .add_enabled(openable, egui::Button::new("打开"))
                    .clicked()
                {
                    ui_state.project_open_request = Some((id, None));
                }
                ui.add_space(SP_S);
                if ui.button("删除项目").clicked() {
                    ui_state.project_intent = Some(ProjectIntent::Delete(id));
                }
            });
            if dirty {
                ui.add_space(SP_XS);
                ui.label(
                    egui::RichText::new("有未保存的改动,先保存再打开。")
                        .color(crate::theme::c32(t.fg_muted)),
                );
            }
        });

    egui::ScrollArea::vertical()
        .id_salt("project_form")
        .show(ui, |ui| {
            // …… 原来的「基本」/「节点」/「记录」三段原样搬进来 ……
        });
```

> **借用提示**：`draft` 是从 `ui_state.project_draft.as_mut()` 借来的，而底部面板闭包里要写 `ui_state.project_intent`。上面的写法通过「先把要用的值 clone/copy 成局部量（`save_draft` / `issue` / `blank` / `openable` / `dirty`），再在闭包里只用局部量」绕开。滚动区那一段仍持有 `draft` 的可变借用，**必须排在底部面板之后**——否则两处借用重叠。

- [ ] **Step 5: 「记录」那一行改成本地时间**

把
```rust
    ui.label(
        egui::RichText::new(format!(
            "创建于 {} · 最后打开 {}",
            draft.created_at,
            draft.last_accessed_at.as_deref().unwrap_or("从未")
        ))
        .color(crate::theme::c32(t.fg_muted)),
    );
```
改为
```rust
    // 时间走本地时区,不把 RFC3339 的 UTC 原文糊在界面上:列表那边说
    // 「3 小时前」、这里说「…T05:00:00Z」的话,同一个字段在同一个弹窗里
    // 分裂成两种读法。偏移取 `localtime::offset()`(进程启动时取的那一次,
    // Windows 上来自 `GetTimeZoneInformation`,就是系统设置里的时区)。
    let now = time::OffsetDateTime::now_utc();
    ui.label(
        egui::RichText::new(format!(
            "创建于 {} · 最后打开 {}",
            crate::localtime::relative(&draft.created_at, now, crate::localtime::offset()),
            draft
                .last_accessed_at
                .as_deref()
                .map_or_else(
                    || "从未".to_string(),
                    |s| crate::localtime::relative(s, now, crate::localtime::offset())
                )
        ))
        .color(crate::theme::c32(t.fg_muted)),
    );
```

- [ ] **Step 6: 跑测试 + clippy**

Run:
```bash
cargo test -p mullion-app project_manager:: 2>&1 | grep -E "test result|FAILED|^error"
cargo clippy -p mullion-app --all-targets 2>&1 | grep -E "^error|^warning" | head
```
Expected: 测试全过；clippy 无输出。

- [ ] **Step 7: Commit**

```bash
git add crates/mullion-app/src/ui/project_manager.rs
git commit -m "feat(app): 项目说明改多行,右栏内容滚动、按钮钉底 (F237)

标签顶对齐 —— Grid 每行默认垂直居中,3 行框旁边的短标签会跟上面对不齐
(走查 P2-17)。右栏加完约 640px,1080p+150% 下按顺序画会把唯一的出口
顶出屏幕。「记录」行顺带改成本地时间,免得同一字段在同一弹窗里两种读法。
跑了 project_manager::tests::the_note_field_is_tall_enough_to_hold_more_than_one_line。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 9: 启动页 —— 搜索 + 改用 `project_row`

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 加 `launcher_search`）
- Modify: `crates/mullion-app/src/ui/launcher.rs`

- [ ] **Step 1: `UiState` 加字段**

在 `project_focus_name` 之后追加：
```rust
    /// F233:启动页项目列表的搜索词。
    ///
    /// 与项目管理器左栏那份**分开**:两个界面可以同时开着,共用一份的话在
    /// 一边打字会静默改掉另一边的过滤结果。
    pub launcher_search: String,
```

- [ ] **Step 2: 写失败的测试**

把 `launcher.rs` 的 `mod tests` 里现有的 `texts` 辅助函数改成接受查询词，并追加两条测试：

```rust
    /// 启动页也能搜。这个页面的标题就叫「继续上次的活」,项目一多就得靠滚。
    ///
    /// 自证会变红:把 `show` 里的 `.filter(|p| crate::project::matches(..))` 删掉。
    #[test]
    fn the_launcher_filters_by_the_search_box_too() {
        let ps = vec![
            proj(1, "接口", "/srv/api", None),
            proj(2, "数据库", "/srv/db", None),
        ];
        let texts = texts_with(&ps, "数据");
        assert!(texts.iter().any(|s| s == "数据库"), "命中的行不见了:{texts:?}");
        assert!(!texts.iter().any(|s| s == "接口"), "没命中的行还在:{texts:?}");
    }

    /// 搜不到时给一句话 + 一个回到全部列表的出口(走查 22)。
    ///
    /// 自证会变红:把空态分支删掉。
    #[test]
    fn a_launcher_query_that_matches_nothing_says_so_and_offers_a_way_back() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let joined = texts_with(&ps, "根本没有").join(" ");
        assert!(joined.contains("没有匹配的项目"), "没给空态说明:{joined}");
        assert!(joined.contains("清空搜索"), "没给回到全部列表的出口:{joined}");
    }

    /// 启动页每一行要显示最后打开时间 —— 这个页面标题是「继续上次的活」,
    /// 却不告诉你上次是什么时候。
    ///
    /// 自证会变红:把 `project_row::show` 换回原来那个只画名字和副标题的
    /// `row` 实现。
    #[test]
    fn each_launcher_row_says_when_it_was_last_opened() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let joined = texts_with(&ps, "").join(" ");
        assert!(joined.contains("从未打开"), "没显示最后打开时间:{joined}");
    }
```

并把原有的 `fn texts(projects: &[ProjectRecord]) -> Vec<String>` 改名/扩展为：
```rust
    fn texts(projects: &[ProjectRecord]) -> Vec<String> {
        texts_with(projects, "")
    }

    fn texts_with(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            launcher_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, &mut ui_state, projects, &lamps, &sessions);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }
```

同时把既有的 `request_after_click` 里的 `find(&cs.shape, "接口")` 保留 —— `project_row` 仍然把项目名画成一个独立的 galley（`paint_highlighted` 在空查询下只 append 一段），锚点仍然找得到。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-app launcher:: 2>&1 | tail -20`

- [ ] **Step 4: 改 `launcher::show`**

把 `crates/mullion-app/src/ui/launcher.rs` 的 `show` 函数体（`egui::CentralPanel` 那一段）改为：

```rust
    use crate::ui::metrics::{field_w, FIELD_W_M, SP_L, SP_M, SP_S};
    // 「现在几点」一帧取一次,不是每行取一次(同 `project_manager::show`)。
    let now = time::OffsetDateTime::now_utc();
    let panel = egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(crate::theme::c32(t.window_bg)))
        .show(ctx, |ui| {
            ui.add_space(SP_L);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("继续上次的活")
                        .size(16.0)
                        .color(crate::theme::c32(t.fg_strong)),
                );
            });
            ui.add_space(SP_M);
            if projects.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(crate::theme::hint_text(
                        t,
                        "还没有项目。一个项目 = 一台机器上的一个目录 + 一个专属 tmux 会话;\
                         从菜单「会话 → 项目管理器」建一个,以后开机点一下就回到现场。",
                    ));
                });
                return;
            }
            // 搜索框居中,取 M 档宽:整宽的搜索框在宽屏上拉成一条几百像素的
            // 缝,而搜索词通常只有几个字。
            //
            // T8:这个框在 `CentralPanel` 里,**不需要**新增 `Modal` 项 ——
            // launcher 态一块 pane 都没有(`show` 的 `pane` 恒传 `None`),
            // 没有终端跟它抢键盘。
            ui.vertical_centered(|ui| {
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut ui_state.launcher_search)
                        .hint_text(crate::theme::hint_text(t, "搜索项目名 / 目录 / 节点"))
                        .desired_width(w),
                );
                crate::ui::annotate::mark(ui.ctx(), "启动页/搜索框", r.rect);
            });
            ui.add_space(SP_M);
            // 顺序**复用** `by_recent_access`、过滤**复用** `project::matches`
            // —— 三处列表各写一份的话,同一个搜索词在两个界面给出不同结果,
            // 而用户几分钟内就会都看到一遍。
            let rows: Vec<&ProjectRecord> =
                crate::ui::project_manager::by_recent_access(projects)
                    .into_iter()
                    .filter(|p| crate::project::matches(p, &ui_state.launcher_search, sessions))
                    .collect();
            if rows.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(crate::theme::hint_text(t, "没有匹配的项目"));
                    ui.add_space(SP_S);
                    if ui.button("清空搜索").clicked() {
                        ui_state.launcher_search.clear();
                    }
                });
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("launcher_projects")
                .show(ui, |ui| {
                    for p in rows {
                        let lamp = lamps
                            .get(&p.id)
                            .copied()
                            .unwrap_or(crate::project::Lamp::Unknown);
                        let r = crate::ui::project_row::show(
                            ui,
                            t,
                            &crate::ui::project_row::Row {
                                project: p,
                                lamp,
                                sessions,
                                query: &ui_state.launcher_search,
                                // 启动页没有「正在编辑哪一个」的概念。
                                selected: false,
                                now,
                                list: "launcher",
                            },
                        );
                        if r.clicked() {
                            ui_state.project_open_request = Some((p.id, None));
                        }
                        ui.add_space(SP_S);
                    }
                });
        });
    crate::ui::annotate::mark(ctx, "项目列表(启动页)", panel.response.rect);
```

删掉本文件里原有的 `fn row(...)`（已被 `project_row::show` 取代），并把 `pub(super) fn row_subtitle` 改成转发到共享实现，**保留这个名字**——`project_pick.rs` 还在用它：

```rust
/// 一行的副标题:`目录 · 节点名`。
///
/// F233 起真正的实现在 `ui::project_row::subtitle`(三处列表共用)。这里留一层
/// 转发是因为 `project_pick` 还按老名字调它,而改调用点属于另一件事。
pub(super) fn row_subtitle(p: &ProjectRecord, sessions: &[SessionRecord]) -> String {
    crate::ui::project_row::subtitle(p, sessions)
}
```

> Task 10 会把 `project_pick` 也改成直接用 `project_row`，届时这层转发和它的两条测试一并删掉。**本 Task 先留着**，保证每一步都能独立编译通过。

- [ ] **Step 5: 跑测试 + 变异自证 + Commit**

Run: `cargo test -p mullion-app launcher:: 2>&1 | grep -E "test result|FAILED|^error"`

```bash
git add crates/mullion-app/src/ui/launcher.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 启动页项目列表可搜索,每行带最后打开时间 (F233/F234)

改用共享的 project_row。搜索框与项目管理器那份分开:两个界面可以同时
开着,共用一份会在一边打字静默改掉另一边的过滤结果。
launcher 态一块 pane 都没有,搜索框不需要新增 Modal 项(T8)。
跑了 launcher::tests 三条新的,含空态出口的变异自证。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 10: pane 切换弹窗 —— 改用 `project_row` + 高度预算重算

**Files:**
- Modify: `crates/mullion-app/src/ui/project_pick.rs`
- Modify: `crates/mullion-app/src/ui/launcher.rs`（删掉 Task 9 留的转发层）

- [ ] **Step 1: 写失败的测试**

在 `project_pick.rs` 的 `mod tests` 里追加：

```rust
    /// 切换弹窗的行也要带最后打开时间 —— 这个弹窗回答的问题和启动页
    /// 完全一样(「切到哪个活」),两处一个有时间一个没有,用户会以为
    /// 其中一处坏了。
    ///
    /// 自证会变红:把 `row` 换回原来那个把名字和副标题拼成一行的实现。
    #[test]
    fn each_pick_row_says_when_it_was_last_opened() {
        let joined = pick_texts(&[proj(1, "接口", "/srv/api", None)], "").join(" ");
        assert!(joined.contains("从未打开"), "没显示最后打开时间:{joined}");
    }

    /// 搜索匹配判据与另外两处**同一个函数** —— 各写一份的话,同一个搜索词
    /// 在两个界面给出不同结果。这里钉的是「按节点名也搜得到」,那正是
    /// `project_pick` 原来那份私有实现做不到的。
    ///
    /// 自证会变红:把 `crate::project::matches` 换回原来的私有 `matches`
    /// (只查 name/dir)。
    #[test]
    fn the_pick_dialog_finds_a_project_by_its_node_name_like_the_other_two_lists() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let joined = pick_texts(&ps, "web01").join(" ");
        assert!(joined.contains("接口"), "按节点名搜不到:{joined}");
    }
```

> `pick_texts` 需要在这个 tests 模块里实现（跑两帧、收 shape 文字），姿态与 `launcher::tests::texts_with` 一致；`proj` / `sess` 辅助函数该模块已有（`project_pick.rs:269` 附近）。

- [ ] **Step 2: 删掉私有 `matches`，改调共享判据**

删掉 `project_pick.rs:75-81` 的私有 `fn matches`，把 `show` 里的
```rust
                        .filter(|p| matches(p, &d.filter))
```
改为
```rust
                        // 判据**复用** `crate::project::matches` —— 另外两处
                        // 列表用的是同一个函数。各写一份的话,同一个搜索词在
                        // 两个界面给出不同结果。原来这里那份私有实现只查
                        // name/dir,搜机器名一条都搜不到。
                        .filter(|p| crate::project::matches(p, &d.filter, sessions))
```

hint 文案同步改：
```rust
                            .hint_text("搜索项目名 / 目录 / 节点")
```

- [ ] **Step 3: 行改用 `project_row`**

删掉 `project_pick.rs` 的私有 `fn row(...)` 和 `fn row_id(...)`，把 `show` 里的
```rust
                                if row(ui, t, p, lamp, sessions) {
```
改为
```rust
                                let now = pick_now;
                                let r = crate::ui::project_row::show(
                                    ui,
                                    t,
                                    &crate::ui::project_row::Row {
                                        project: p,
                                        lamp,
                                        sessions,
                                        query: &d.filter,
                                        selected: false,
                                        now,
                                        list: "pick",
                                    },
                                );
                                if r.clicked() {
```
并在 `show` 函数体最开头（`let d = draft.as_mut()?;` 之后）加：
```rust
    // 一帧取一次(同另外两处列表)。
    let pick_now = time::OffsetDateTime::now_utc();
```

- [ ] **Step 4: 重算高度预算**

行从「一行拼接文本」变成「两行 + 时间列」，行高从约 `galley.height()+8`（≈22）变成 `project_row::ROW_H`（48）。把 `CHROME_H` 的文档注释与取值改为：

```rust
/// 固定部分(说明行 + 搜索框 + 分隔线 + 取消按钮 + 边框内边距)的高度预算。
/// 宁可估大:估小了列表会把取消按钮顶出 pane 外面,而那是唯一的退出口。
///
/// F233:行从「一行拼接文本」换成 `project_row`(两行 + 时间列,`ROW_H` = 48)
/// 之后,同一块 pane 里能放下的行数少了一半多。这个常量本身只管**固定部分**,
/// 不随行高变 —— 但下面 `list_h` 的计算必须按新行高走,否则最后一行会被切掉
/// 一半、看起来像渲染坏了。
const CHROME_H: f32 = 120.0;
```

找到 `show` 里算 `list_h` 的那一行，确认它是 `(host.height() - CHROME_H).max(..)` 形式；把下界改成至少能完整放下一行：
```rust
    // 下界取一整行:放不下一整行的话最后那行会被切一半,看起来像渲染坏了。
    let list_h = (host.height() - CHROME_H - 2.0 * INSET).max(crate::ui::project_row::ROW_H);
```

- [ ] **Step 5: 删掉 `launcher::row_subtitle` 转发层**

`project_pick` 不再调它了。删掉 `launcher.rs` 里的 `pub(super) fn row_subtitle`，并把 `launcher.rs` 的 `mod tests` 里那两条只测它的测试（`a_row_names_both_the_directory_and_the_node_it_will_dial` 与 `a_row_whose_node_is_gone_still_says_which_directory_it_is`）删掉 —— **它们已经在 `project_row::tests` 里逐字保留了**，不是丢掉判据。

Run 确认没有别的调用点：
```bash
grep -rn "row_subtitle" crates/mullion-app/src/
```
Expected: 无输出。

- [ ] **Step 6: 跑测试 + clippy**

Run:
```bash
cargo test -p mullion-app project_pick:: 2>&1 | grep -E "test result|FAILED|^error"
cargo clippy -p mullion-app --all-targets 2>&1 | grep -E "^error|^warning" | head
```

- [ ] **Step 7: 变异自证 + Commit**

```bash
git add crates/mullion-app/src/ui/project_pick.rs crates/mullion-app/src/ui/launcher.rs
git commit -m "feat(app): 切换弹窗改用共享行,搜索判据与另两处统一 (F233/F234)

原来那份私有 matches 只查 name/dir,搜机器名一条都搜不到。
行高 22→48,list_h 的下界改成至少一整行,否则最后一行被切一半。
跑了 project_pick::tests 两条新的。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 11: F235 —— 点开项目那一刻就记账

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`fn dial_project`，8408 行附近）

- [ ] **Step 1: 写失败的测试**

在 `app.rs` 的 `mod tests` 里，紧挨着既有的 `dial_project` 源码切片测试追加：

```rust
    /// F235:点开项目那一刻就记一笔访问时间。
    ///
    /// 改之前唯一的写入点是 `drive_project_visits` 的上报跃迁 —— 链路是
    /// 「pane 上报远端 tmux 名 → `project::hits` 匹配 → 跃迁时写盘」,
    /// 中间任何一环没成(上报自举没跑、连接失败、远端会话名对不上),
    /// `last_accessed_at` 就永远是 `None`:右栏显示「从未」、列表按 id 排,
    /// 看起来就是「排序坏了」。
    ///
    /// 上报那条**保留**:它还负责「用户不走项目入口、直接连会话 attach 进
    /// 那个 tmux」的情形(F224①)。两条写入点共存,后写的赢,而两条写的都是
    /// 「刚刚」,谁赢都对。
    ///
    /// 判据放在 `dial_project` 上而不是它下游的两条分支上:那里是「用户
    /// 确认要开这个项目」的唯一收口点,两条分支(换节点 / 开新标签)都从它
    /// 出发。挂在某一条分支上的话另一条静默漏记。
    ///
    /// 自证会变红:把 `dial_project` 里那句 `touch_project_accessed` 删掉。
    #[test]
    fn opening_a_project_records_the_visit_right_away_not_only_when_the_report_lands() {
        let src = include_str!("app.rs");
        let body = src
            .split_once("fn dial_project(")
            .expect("找不到 dial_project")
            .1;
        let body = &body[..body.find("\n    }\n").expect("找不到 dial_project 的结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains("touch_project_accessed("),
            "dial_project 没记访问时间 —— 上报链路断掉时列表会永远显示「从未」"
        );
        // 必须在两条分支**之前**:`open_project_in_new_tab` 那一支是 early
        // return,记在它后面的话 launcher 上点开的项目一条都不记 —— 而
        // launcher 正是这个功能的主入口。
        let touch = code.find("touch_project_accessed(").expect("上一条断言已保证有");
        let branch = code
            .find("open_project_in_new_tab(")
            .expect("找不到开新标签那一支");
        assert!(
            touch < branch,
            "记账排在 open_project_in_new_tab 之后 —— 从 launcher 点开的项目一条都不会记"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app opening_a_project_records 2>&1 | grep -E "test result|FAILED"`
Expected: 红。

- [ ] **Step 3: 实现**

把 `fn dial_project` 开头改为（在拿到 `p` 之后、任何分支之前插入记账）：

```rust
    fn dial_project(&mut self, ask: &crate::ui::project_manager::OpenAsk) {
        let Some(p) = self
            .store
            .as_ref()
            .and_then(|s| s.projects().iter().find(|p| p.id == ask.project).cloned())
        else {
            self.ui.set_error("这个项目已经不在了".to_string());
            return;
        };
        // F235:记一笔访问时间。**在这里、在任何分支之前** —— 下面
        // `open_project_in_new_tab` 那一支是 early return,记在它后面的话
        // 从 launcher 点开的项目一条都不会记,而 launcher 正是主入口。
        //
        // 为什么不只靠 F224 的上报跃迁:那条链路是「pane 上报远端 tmux 名 →
        // `project::hits` 匹配 → 跃迁时写盘」,中间任何一环没成(上报自举没
        // 跑、连接失败、远端会话名对不上)时间戳就永远是 `None`,列表退化成
        // 按 id 排 —— 用户看到的就是「排序坏了」,且零报错。
        //
        // 上报那条**保留**:它还负责「用户不走项目入口、直接连会话 attach 进
        // 那个 tmux」的情形(F224①)。两条写的都是「刚刚」,谁后写谁赢都对。
        //
        // 与 F224② 的「跃迁触发」不冲突:那条约束防的是「每几秒写一次盘」,
        // 而这里一次点击只走一遍。
        //
        // 写不进去**只记日志**:访问时间不是用户资产,为它弹一张错误卡片
        // 不成比例(同 `drive_project_visits`)。
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        if let Some(store) = self.store.as_mut() {
            store.touch_project_accessed(ask.project, &now);
            if let Err(e) = store.save() {
                log::debug!(target: "mullion", "项目访问时间落盘失败: {e}");
            }
        }
        let Some((g, focus)) = self.active_ws().map(|ws| (ws.generation(), ws.focus())) else {
```
（其余部分不动。）

- [ ] **Step 4: 跑测试 + 变异自证**

Run: `cargo test -p mullion-app opening_a_project_records 2>&1 | grep -E "test result|FAILED"`
Expected: `ok`。

变异：把 `touch_project_accessed` 那一段整体挪到 `let Some((g, focus)) = ...` 的 `else` 分支之后 → 第二条断言必须红。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 点开项目那一刻就记访问时间 (F235)

改之前唯一写入点是上报跃迁,链路上任何一环没成时间戳就永远是 None,
列表退化成按 id 排 —— 看起来就是「排序坏了」且零报错。
记在两条分支之前:open_project_in_new_tab 是 early return,记在它后面
的话从 launcher 点开的项目一条都不会记,而 launcher 正是主入口。
跑了 app::tests::opening_a_project_records_the_visit_right_away_not_only_when_the_report_lands。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 12: 规范登记 + spec.md

**Files:**
- Modify: `crates/mullion-app/tests/form_guidelines.rs`（`EXTRA`）
- Modify: `spec.md`

- [ ] **Step 1: 把三个文件登记进表单规范的扫描范围**

在 `crates/mullion-app/tests/form_guidelines.rs` 的 `EXTRA` 数组末尾追加：

```rust
    // F233:三处项目列表共用的行 + 启动页 + pane 切换弹窗。它们同样吃
    // `metrics` 的间距/宽度刻度。**漏登记的话规范对它们一条都管不住** ——
    // 而这三个文件是本切片新写/大改的,正是最容易漏进裸数字的地方。
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/project_row.rs"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/launcher.rs"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/project_pick.rs"),
```

- [ ] **Step 2: 跑规范测试**

Run:
```bash
cargo test -p mullion-app --test form_guidelines 2>&1 | grep -E "test result|FAILED|:.*add_space|:.*desired_width"
```
Expected: `test result: ok.`。**若报出违规行**：把那一处的数字换成 `SP_*` / `field_w(...)`；**不要**往 `ALLOW` 里加条目——那个数组只登记既有欠债，本切片新写的代码没有资格进去。

- [ ] **Step 3: spec.md 补 F233~F237**

在 `spec.md` 第 314 行（`| F225 | ...`）之后追加五行。表格三列是 `编号 | 内容 | 优先级 | 理由/陷阱`：

```markdown
| F233 | **项目列表搜索**：项目管理器左栏 / 启动页「继续上次的活」/ pane 标题条切换弹窗**三处**共用一份 `project::matches`，匹配**项目名 + 目录 + 每一条节点会话的名字与主机**，大小写不敏感；命中片段染 accent（复用会话侧 `highlight::segments` + `paint_highlighted`）；搜不到时给「没有匹配的项目」+「清空搜索」出口 | P2 | **① 判据必须三处同一个函数** —— 各写一份的话同一个搜索词在两个界面给出不同结果，而用户几分钟内就会都看到一遍（同 `by_recent_access` 那条）。改之前 `project_pick` 那份私有实现只查 name/dir，**搜机器名一条都搜不到**。**② 收节点名/主机而不是只收项目名**：用户记得住的常是机器名或 IP 尾数，不是当初给活起的名字。**③ 只收这个项目自己的节点**（`s.id == *id`）——丢掉 id 比对的话任意一条会话名都能把全部项目一起捞出来，搜索仍然「有反应」但等于失效。**④ 不收 `note`**：F237 把说明改多行之后，长文本参与匹配会命中一堆用户在列表上看不见的东西。**⑤ 命中必须标出来**——搜「web01」命中的是副标题里的节点名，不染色的话用户完全不知道这一行为什么会出现（走查 22 原话：过滤了却标不出命中在哪，用户会以为搜索坏了）。**⑥ 启动页那个搜索框不需要新增 `Modal` 项**：launcher 态一块 pane 都没有，没有终端跟它抢键盘（T8 的前提不成立）；项目管理器与切换弹窗本来就在表里 |
| F234 | **列表显示最后打开时间**：三处列表每行右端显示相对时间——刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 超 30 天落回本地日期；从未打开过的显示「从未打开」。纯函数 `localtime::relative(then, now, offset)`，`time` crate 新开 `parsing` feature | P2 | **① 「昨天」与「N 天前」按本地日界判，不按 UTC 日期** —— `2026-09-07T17:00Z` 在 UTC 下是 9 月 7 日（昨天），在 UTC+8 下却是 9 月 8 日凌晨 1 点（今天）。拿 UTC 算的话用户每天早上 8 点前都会看到错误的「昨天」，而且**完全静默**：编译、测试、日志一律正常，只有人眼能发现。**② `now` 与 `offset` 都是入参不是全局读取**（同 `format_unix`）：进程级 `OnceLock` 被别的测试设过就再也改不动；「现在几点」写死才能让断言不随真实时钟在跨整点/跨午夜时随机变红。**③ 时区取 `localtime::offset()`**——进程启动第一步取的那一次，Windows 上来自 `GetTimeZoneInformation`，就是系统设置里的时区（F186）。**④ 已认下的代价：文字不自动刷新**，帧闸只在有事件时重绘，「刚刚」会一直停在「刚刚」直到用户动一下鼠标；为一行时间文字去请求定时重绘，与 F157~F183 一连八个切片抠空闲帧的方向直接相反。**⑤ 解析不出来返回原文**，不编一句「时间未知」——那句话既没有可操作性，又把「配置里到底写了什么」这唯一线索藏起来。**⑥ 时间列的宽度要先量、名称的可用宽度先扣掉它**，不扣的话长项目名会一路截断到右边缘，把时间整列挤出行外（整条看不见） |
| F235 | **访问时间的记账时机**：`dial_project`（「用户确认要开这个项目」的唯一收口点）一进来就写一笔 `last_accessed_at`；F224 的上报跃迁写入**保留**为补充 | P2 | 改之前唯一的写入点是上报跃迁，链路是「pane 上报远端 tmux 名 → `project::hits` 匹配 → 跃迁时写盘」，**中间任何一环没成**（上报自举没跑、连接失败、远端会话名对不上）时间戳就永远是 `None`：右栏显示「从未」、列表退化成按 id 排——用户看到的就是「按时间排序没生效」，且零报错。**① 记在两条分支之前**：`open_project_in_new_tab` 那一支是 early return，记在它后面的话从 launcher 点开的项目一条都不会记，而 launcher 正是这个功能的主入口。**② 不撤掉上报那条**：它还负责「用户不走项目入口、直接连会话 attach 进那个 tmux」的情形（F224①）；两条写的都是「刚刚」，谁后写谁赢都对。**③ 与 F224② 的「跃迁触发」不冲突**：那条约束防的是「每几秒写一次盘」，而这里一次点击只走一遍。**已认下的偏差**：连接失败的尝试也会被记一笔——与 F224② 原本「不让失败的尝试污染列表」的说法相反，本条明确推翻它：用户问的是「我上次碰过什么」而不是「我上次成功连上过什么」，而「点了没反应、列表也没动」比「记了一笔」更难排查 |
| F236 | **一键「+ 添加项目」**：左栏顶部那个「新建项目」输入框整个让位给搜索框；新建改成撞满左栏整宽的描边按钮「+ 添加项目」，移到左栏底部；点它直接建一个「新项目」（撞名补第一个空号），落盘后自动选中、右栏「名称」框拿焦点并全选 | P2 | **① 名字必须去重**：`ProjectIntent::Add` 是立刻落盘的，而 `validate_project` 要求项目名全局唯一——不去重就会在盘上建出一条必然存不进去的记录，列表里两行同名、右栏「保存」灰着，用户看不出为什么。**② 补第一个空号而不是 max+1**：删掉「新项目」再点添加，给出的应该是「新项目」，不是跳过一堆空号变成「新项目 7」。**③ 视觉重点靠位置和尺寸，不靠颜色**：全场唯一一个 accent 实心按钮是会话编辑器的「保存并连接」，再加一颗会把那个层级搅浑；撞满整宽 + 独占底栏已经足够。**④ 按钮走 `TopBottomPanel::bottom(..).show_inside`**：直接按顺序画的话项目一多就把唯一的新建入口顶出可视区（同会话侧「+ 新建」）。**⑤ 焦点标志用完即 `take`**：留着的话每帧抢一次焦点，用户点右栏任何别的输入框都会被当场弹回名称框，且这个状态没有自愈路径。**⑥ `app.rs` 的 `Add` 分支不许再读那个 UI 缓冲**——它已经是搜索框，继续读会建出名字为空的项目 |
| F237 | **项目「说明」改多行**（`desired_rows(3)`）；右栏内容套滚动区，「保存 / 打开 / 删除项目」按钮行钉在底部不跟着滚；项目管理器左栏 192→300、窗口 720→840；右栏「记录」行的时间改成本地相对时间 | P2 | **① 标签必须顶对齐**：`Grid` 每行默认 `Align::Center`，3 行高的 multiline 旁边的短标签会被垂直居中、跟上面几行对不齐（会话侧「备注」为同一个原因写过，走查 P2-17）。**② 按钮行必须钉底**：右栏加完约 640px，在 1080p + 150% 缩放（逻辑高 720）下按顺序画会把「保存 / 打开 / 删除项目」顶出屏幕——而那是这个界面唯一的出口，且这个缺陷只在高 DPI 小屏上复现，开发机上永远碰不到。**③ 底部面板的闭包里拿不到 `draft` 的借用**：`issue` / `blank` / `openable` / `dirty` / 草稿副本都要先算成局部量，滚动区那一段必须排在底部面板之后。**④ 左栏加宽是搜索的连锁**：行里有了副标题和右对齐时间列，192px 装不下，长项目名会被截成一两个字；不同步把窗口从 720 提到 840 的话右栏会从 442 缩到 334，说明框跟着变窄。**⑤「记录」行顺带改本地时间**：列表说「3 小时前」、右栏说「…T05:00:00Z」的话，同一个字段在同一个弹窗里分裂成两种读法 |
```

同时在 spec.md 的「插队」表（第 514 行附近那张）末尾追加一行：

```markdown
| **插队** | F233~F237 | 项目列表搜索 / 最后打开时间 / 记账时机 / 一键添加 / 多行说明（2026-09-08）。三处列表抽出共享的手绘 `project_row`；相对时间的「昨天」按**本地**日界判——按 UTC 算的话用户每天早上 8 点前都会看到错误的「昨天」且完全静默；访问时间改成点开那一刻就记，原来只靠上报跃迁，链路断掉时列表会永远显示「从未」、排序退化成按 id，看起来就是「排序坏了」 |
```

> **不代补 F226~F232**：那七条（v0.1.97）也没进 spec.md，是上一切片的欠账。本切片不顺手补（Scope Discipline），在收尾报告里提一句即可。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/tests/form_guidelines.rs spec.md
git commit -m "docs: spec 补 F233~F237,表单规范登记三个新文件 (F233~F237)

form_guidelines 的扫描范围是逐个登记的 —— 漏登记的话规范对新写的
UI 一条都管不住,而这三个文件正是本切片新写/大改的。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 13: 全绿 + 发版

- [ ] **Step 1: 全量测试**

Run:
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -v "0 failed" | head -20
```
Expected: 只剩 `test result: ok` 行（含 `0 failed` 的行被 grep 掉了，所以理想输出是空）。任何 `FAILED` / `panicked` 都要查到底。

- [ ] **Step 2: clippy**

Run:
```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```
Expected: 无 error、无 warning。

- [ ] **Step 3: fmt**

Run:
```bash
cargo fmt --check
```
Expected: 无输出。有输出就 `cargo fmt` 之后单独提交一个 `style` commit。

- [ ] **Step 4: 字形白名单**

Run:
```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"
```
Expected: `ok`。本批新文案全是 GBK 内汉字 + ASCII，理应直接过；红了就说明混进了 `…` / `—` 之类，改掉，**不要**往 `VERIFIED` 里塞。

- [ ] **Step 5: 发版**

按 `.claude/skills/release-windows/SKILL.md` 一条龙走完：升 patch 版本号（`0.1.97` → `0.1.98`）→ 跑绿 → 交叉编译 `x86_64-pc-windows-gnu` → `objdump` 依赖验收 → 签名 → 发 GitHub Release（`gh` 走 socks 代理，本机 DNS 解析不了 github）。

**别凭记忆做**——每一步都有漏了也不报错的坑。

- [ ] **Step 6: 人工验收清单（写进 Release notes）**

以下全是无头容器里验证不了、只有人眼能判的：

1. **左栏**：搜索框独占一行、宽度看着像会话管理器那一栏；打「web01」（某个节点的会话名）应该只剩挂在那台机器上的项目，且**副标题里的 `web01` 是高亮色**。
2. **左栏底部**：「+ 添加项目」撞满整宽；项目多到需要滚动时，这颗按钮**仍然可见**（不跟着列表滚走）。
3. 点「+ 添加项目」→ 列表里出现「新项目」并被选中 → **右栏名称框已经有光标且文字全选**，直接打字就是改名。再点一次 → 出现「新项目 2」。
4. **右栏**：「说明」是三行高的框，左边的「说明」二字**与上面「名称」的标签左对齐、顶部对齐**（不是垂直居中）。
5. **右栏底部**：把窗口拖到很矮，「保存 / 打开 / 删除项目」**始终在底部可见**，上面的内容自己滚。
6. **时间列**：每行右端有「N 小时前 / 昨天 / 从未打开」；**特别验一次跨午夜**——如果你在早上 8 点前看，昨晚干过的活应该显示「N 小时前」或「昨天」，不能出现明显错一天的情况。
7. **记账**：打开一个从没打开过的项目 →（哪怕连接失败）回到项目管理器，那一行应该已经变成「刚刚」，且排到了列表最上面。
8. **启动页**：标题「继续上次的活」下方有搜索框；每行右端有时间；点一行照常打开。
9. **pane 标题条 → 切换项目**：弹窗里的行也是两行 + 时间；**取消按钮没有被顶出 pane 外面**（这是行高从 22 变 48 之后最可能出事的地方，尤其在很矮的分屏里）。
10. **豆腐块**：以上所有新文字里不该出现任何 `□`。

---

## 自查记录

**规格覆盖**：F233（Task 2/4/5/6/9/10）、F234（Task 1/5/6/9/10）、F235（Task 11）、F236（Task 3/6/7）、F237（Task 8）、支撑改动（Task 5 共享行 / Task 6 左栏宽度与窗口 / Task 8 滚动与钉底）、规范与文档（Task 12）、交付（Task 13）。共识里的每一条都有落点。

**类型一致性**：`project_row::Row` 的字段（`project` / `lamp` / `sessions` / `query` / `selected` / `now` / `list`）在 Task 5 定义，Task 6/9/10 三处构造用的是同一组字段名；`crate::project::matches(p, query, sessions)` 的三参数签名在 Task 2 定下，Task 6/9/10 三处调用一致；`localtime::relative(then, now, offset)` 在 Task 1 定下，Task 5（`time_text`）与 Task 8（记录行）用法一致。

**已知会在执行中需要现场确认的两点**（不是占位，是明确标注了核实命令的地方）：
- Task 5 的 `ts.galley.job.sections` 路径 —— 给了 `grep` 命令；
- Task 8 的 `ts.galley.rect.height()` 是否真能量出多行框高度 —— 给了退路（改用最接近该文字的 `Shape::Rect`）。
两处都必须**先跑一次确认**，不能默认成立就往下写。
