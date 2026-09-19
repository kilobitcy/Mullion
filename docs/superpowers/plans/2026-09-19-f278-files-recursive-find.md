# F278 文件面板递归模糊搜索 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 文件面板远端栏的路径条上加一个放大镜,点开后在**当前目录下递归**模糊搜索文件名,结果列表出相对路径,Enter/双击跳到所在目录并把那一条亮出来。

**Architecture:** 遍历本身是一台**纯状态机**(`files/find.rs`,零 IO / 零 egui / 零 async),形状照抄 `files/queue.rs`:每帧 `take_runnable(n)` 吐出最多 n 个待列目录,app 侧各 spawn 一条 `list_dir`,结果经 `UserEvent::FindListed` 回来喂 `Walk::accept`。这么切的理由和 `queue.rs` 逐字相同 —— 「BFS 顺序对不对」「封顶卡没卡住」「最后一批还在飞的时候会不会提前报搜完」全是状态机 bug,必须能在没有网络的情况下复现。

**Tech Stack:** Rust / egui 0.30 / russh-sftp(`SftpClient::list_dir`)/ 既有的 `UserEvent` + `track_sftp_task` 异步回路。

---

## 已经定死的设计决策(grill 阶段用户确认过,**不要重新讨论**)

写在 `spec.md:360` 那一格里,逐条抄在这里备查:

- **否掉远端 `find`**:SFTP-only 节点无 exec、busybox 参数不齐、引号转义一摊事。**逐目录 `list_dir` 遍历**。
- **否掉边输边搜**:递归场景等于每敲一键把链路打一遍。**Enter 才起搜**。
- 大小写不敏感**子序列**匹配(fzf 式)。
- 封顶 **500 结果 / 2000 目录**,可取消,**不跟符号链接**,隐藏目录随 `show_hidden`。
- 结果列表出**相对路径**;Enter/双击跳到所在目录并置选中(**复用 F218 reveal**)。
- 命中在当前目录时退化成定位模式。

## 本计划自己补的两条范围决策(实现者不要擅自扩大)

1. **只做远端栏**。用户实报点的是「文件面板/**远端**/路径/控件」,spec 那一格写的是「SFTP 逐目录遍历」。路径条是两栏共用的函数,所以放大镜按钮要用 `column == PanelColumn::Remote` 门住 —— 这在 `files_panel.rs` 里是既有写法(见 `files_panel.rs:302`/`581`/`699` 三处同款门)。本地栏递归要另配一条 `spawn_blocking` 通路(本地列目录目前是**同步跑在事件循环线程上**的,见 `app.rs` 里 `apply_local_file_action` 上方那段注释),不在本切片。
2. **不做「搜索结果里再做写操作」**。结果列表只有两个动作:跳过去、关掉。右键菜单、多选、传输一概不接。

---

## File Structure

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-app/src/files/find.rs` | 匹配判据 + 遍历状态机 + 相对路径。纯函数,可单测 | **新建** |
| `crates/mullion-app/src/files/mod.rs` | 挂 `pub mod find;` | 改 |
| `crates/mullion-app/src/ui/icon.rs` | `Glyph::Search` 自绘(T9:不许用 🔍/⌕ 字符) | 改 |
| `crates/mullion-app/src/ui/files_panel.rs` | `PanelFrame::find` 状态、5 个 `FileAction` 变体、放大镜按钮、搜索条、结果列表 | 改 |
| `crates/mullion-app/src/app.rs` | `Modal::FilesFind`、`UserEvent::FindListed`、`spawn_sftp_find_list`、`pump_find`、`accept_find_listed`、`FindPick` 落地、作废收口 | 改 |
| `spec.md` | F278 标成已实现 + 落点 | 改 |

---

## 实现者必须先读的既有约定(照做,别自创)

**A. 混合任务池不许无差别 abort。** `app.rs:5444` 那段长注释(`reopen_sftp_on_focused_host` 上方)写死了:`sftp_tasks` 是列目录/写操作/传输混在一起的池子,`drain` 后 abort 会腰斩传输并把 `load` 永久卡在 `Loading`。**搜索任务照样 `track_sftp_task` 进这个池,取消一律靠序号作废,绝不 abort。**

**B. 后发先至靠 seq。** 每次起搜 `find_seq += 1`,发出去的 `list_dir` 带着它,回来时对不上就整条丢掉。取消 / 换目录 / 换机器 / 关搜索条一律走「递增 seq」,不走 abort。

**C. 新 `UserEvent` 不许蹭 `SftpListed`。** 理由与 `UserEvent::PathProbed` 的文档注释逐字相同:`SftpListed` 的 seq 空间是 `PaneState::request_seq`,搜索的 seq 空间是 `PanelFrame::find_seq`,共用一个事件就会撞号,撞上就是「一次列目录被搜索结果顶掉」,而且完全静默。

**D. `Modal` 是列举式门控,加档必然漏。** 本项目已经踩过三次(见 CLAUDE.md 与 `draft_baseline_is_in_vault` 的文档)。Task 4 把 9 处登记点全列出来了,一处不许少。

**E. `PanelFrame` 加字段的前置警告。** `files_panel.rs` 里 `impl Default for PanelFrame` 上方那段注释:这个结构体的 `default()` 同时当「新标签起始状态」和「借用过桥期间的临时占位」,**不许加没有安全默认值的字段(sftp handle、后台任务句柄)**。本计划加的 `find: Option<Find>` / `find_seq: u64` 都是纯数据,`None` / `0` 是真实可用的初值,符合这条;**任务句柄一律走 `track_sftp_task` 进 `TerminalTab::sftp_tasks`,不许挂到 `PanelFrame` 上。**

**F. `drive_*` / `pump_*` 每帧驱动函数必须遍历全部标签。** `pump_transfers` 不用遍历是因为它的队列是全局一份;`find` 是**每标签一份**,`pump_find` 必须遍历 `self.tabs` 全部,只推活动标签的话「搜索中切到别的标签,回来发现它停在半路」——而且完全静默。

**G. 守护测试的纪律。** 每条断言都要能指出「把哪一行改成什么会让它变红」,并写进 doc 注释(本项目既有写法,见 `files/reveal.rs` 的测试)。变异自证**必须先 `git commit`** 再改;`git checkout` 只带完整路径。

---

### Task 1: `files/find.rs` —— 匹配判据与相对路径

**Files:**
- Create: `crates/mullion-app/src/files/find.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`(加 `pub mod find;`)

- [ ] **Step 1: 建文件,写模块头 + 两个纯函数 + 失败的测试**

新建 `crates/mullion-app/src/files/find.rs`:

```rust
//! F278:文件面板的**递归模糊搜索**。纯逻辑 —— 零 egui、零 IO、零 async。
//!
//! 遍历本身是一台状态机([`Walk`]),形状照抄 `files/queue.rs`:每帧
//! [`Walk::take_runnable`] 吐出最多 n 个待列目录,app 侧各起一条 `list_dir`,
//! 结果回来喂 [`Walk::accept`]。这么切的理由和 `queue.rs` 逐字相同 ——
//! 「BFS 顺序对不对」「封顶卡没卡住」「最后一批还在飞的时候会不会提前报
//! 搜完」全是状态机 bug,必须能在没有网络的情况下复现。
//!
//! **为什么不发一句远端 `find`**(spec F278 已定):SFTP-only 的节点压根
//! 没有 exec 通道;有 exec 的也可能是 busybox,`-iname`/`-maxdepth` 参数
//! 不齐;再加上文件名里的引号与空格转义,是一摊比逐目录遍历更难对的事。

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

/// 名字里出现的这几个字符按**子序列**匹配。大小写不敏感。
///
/// **是子序列不是子串**(fzf 式):用户记得住的往往是 `apprs` 这种骨架,
/// 而不是完整的 `app.rs`。`crate::search::matches` 那一套(会话/项目列表用的)
/// 刻意**不是**子序列 —— 那边的字段里有多行长文本(F237 的项目说明),子序列
/// 在长文本上几乎命中一切。这边匹配的是**单个文件名**,短,子序列正合适。
/// 两处判据不同是有意的,不要合并。
///
/// 空查询恒不匹配 —— 调用方(`Walk::new`)在起搜前就该挡掉空串,
/// 这里返回 `false` 只是兜底:恒 `true` 会让一次空搜索把整棵树当成 500 个
/// 命中吐出来。
pub fn matches(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let mut want = query.chars().flat_map(char::to_lowercase).peekable();
    for c in name.chars().flat_map(char::to_lowercase) {
        match want.peek() {
            Some(&w) if w == c => {
                want.next();
            }
            Some(_) => {}
            None => return true,
        }
    }
    want.peek().is_none()
}

/// 结果行显示的那一串:`root` 下的相对路径。
///
/// **显示相对路径而不是绝对路径**(spec F278 已定):搜索的心智是「在**这个
/// 目录下面**找」,绝对路径每行都顶着同一段前缀,把唯一有信息量的后半截
/// 挤出可视区。
///
/// `path` 不在 `root` 下时**原样返回绝对路径** —— 那说明调用方把两棵树的
/// 东西混一起了,硬切前缀会切出一串没头没尾的乱码,不如把真相摆出来。
pub fn relative(root: &RemotePath, path: &RemotePath) -> String {
    let r = root.as_bytes();
    let p = path.as_bytes();
    // 根是 `/` 时没有「前缀 + 分隔符」可言,直接剥掉那一个字节。
    let cut = if r == b"/" {
        1
    } else if p.starts_with(r) && p.get(r.len()) == Some(&b'/') {
        r.len() + 1
    } else {
        return path.display().to_string();
    };
    RemotePath::from_bytes(p[cut..].to_vec())
        .display()
        .to_string()
}
```

在 `crates/mullion-app/src/files/mod.rs` 的 `pub mod fail;` 之后加一行(按字母序,`drag`/`fail`/`find`/`local`):

```rust
pub mod find;
```

- [ ] **Step 2: 写测试**

追加到 `find.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 子序列,不是子串 —— 用户记得住的是骨架。
    ///
    /// 自证会变红:把 `matches` 改成 `name.to_lowercase().contains(&query.to_lowercase())`
    /// (第一条断言红)。
    #[test]
    fn the_query_matches_as_a_subsequence_not_a_substring() {
        assert!(matches("apprs", "app.rs"));
        assert!(matches("app.rs", "app.rs"));
        assert!(matches("mn", "main.rs"));
        assert!(!matches("rsapp", "app.rs"), "顺序不对不算命中");
        assert!(!matches("appx", "app.rs"));
    }

    /// 大小写不敏感 —— 远端多半是 Linux,用户按 Windows 习惯打小写。
    ///
    /// 自证会变红:把两处 `to_lowercase` 删掉。
    #[test]
    fn matching_ignores_case_on_both_sides() {
        assert!(matches("APP", "app.rs"));
        assert!(matches("app", "APP.RS"));
        assert!(matches("Gm", "GAMMA"));
    }

    /// 空查询恒不匹配。恒 `true` 的话一次空搜索会把整棵树当成 500 个命中吐
    /// 出来 —— 而用户什么都没输。
    ///
    /// 自证会变红:把 `matches` 开头那三行 `if query.is_empty()` 删掉。
    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        assert!(!matches("", "app.rs"));
        assert!(!matches("", ""));
    }

    /// 相对路径剥掉根 + 那一个分隔符。
    ///
    /// 自证会变红:把 `r.len() + 1` 改成 `r.len()`(会多出个前导 `/`)。
    #[test]
    fn a_hit_is_shown_relative_to_the_directory_the_search_started_in() {
        assert_eq!(relative(&rp("/home/u/proj"), &rp("/home/u/proj/src/app.rs")), "src/app.rs");
        assert_eq!(relative(&rp("/home/u/proj"), &rp("/home/u/proj/a.txt")), "a.txt");
    }

    /// 根是 `/` 时只剥那一个字节 —— 按「根 + 分隔符」算会多剥一个字符,
    /// `/etc/hosts` 会显示成 `tc/hosts`。
    ///
    /// 自证会变红:把 `if r == b"/"` 那一支删掉。
    #[test]
    fn searching_from_the_filesystem_root_does_not_eat_a_character() {
        assert_eq!(relative(&rp("/"), &rp("/etc/hosts")), "etc/hosts");
    }

    /// 不在根下面的路径原样给绝对路径 —— 硬切前缀会切出没头没尾的乱码。
    ///
    /// 自证会变红:把最后那条 `return path.display().to_string()` 改成
    /// 无条件切 `r.len()`。
    #[test]
    fn a_path_outside_the_root_keeps_its_absolute_form() {
        assert_eq!(relative(&rp("/home/u/proj"), &rp("/var/log/x")), "/var/log/x");
        // 前缀撞上但不是目录边界(`/home/u/project` vs `/home/u/proj`)。
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/project/x")),
            "/home/u/project/x"
        );
    }
}
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p mullion-app files::find:: 2>&1 | tail -20
```
Expected: 6 passed。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/src/files/find.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 递归搜索的匹配判据与相对路径 (F278)"
```

---

### Task 2: `Walk` 遍历状态机

**Files:**
- Modify: `crates/mullion-app/src/files/find.rs`

- [ ] **Step 1: 在 `relative` 之后、`#[cfg(test)]` 之前插入状态机**

```rust
/// 命中封顶。**500 条之后停**:再多用户也不会往下翻,而每多一条就多一次
/// 无意义的往返。到顶时状态里写明「结果可能不全」,不装作搜完了。
pub const MAX_HITS: usize = 500;

/// 目录封顶。**2000 个之后停**:家目录下随便一个 `node_modules` 就是几万个
/// 目录,而本项目的主场景是高延迟代理链路,一次往返几百毫秒。
pub const MAX_DIRS: usize = 2000;

/// 同时在飞的 `list_dir` 条数。
///
/// **不是 1**:串行的话 2000 个目录 × 300ms RTT ≈ 10 分钟,这个功能等于不存在。
/// **也不是几十**:russh-sftp 在同一条 channel 上复用请求 id,开太多只会把
/// 窗口撑满、把同一条连接上的交互式操作(列目录、编辑保存)挤到后面去。
pub const CONCURRENCY: usize = 4;

/// 停下来的原因。**四档分开**,因为界面上要说的话完全不同 ——
/// 「搜完了,没找到」和「翻了 2000 个目录还没翻完」是两件事,混成一句
/// 「没有结果」会让用户以为文件不存在,而它可能就在第 2001 个目录里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// 整棵树翻完了。
    Exhausted,
    /// 命中到 [`MAX_HITS`] 了。
    HitCap,
    /// 目录翻到 [`MAX_DIRS`] 了。
    DirCap,
    /// 用户按了取消。
    Canceled,
}

/// 一条命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// **绝对路径**。跳转要用它,显示时才现算相对路径(走 [`relative`])——
    /// 只存相对路径的话,跳转那一步要把根拼回去,而根可能已经变了。
    pub path: RemotePath,
    /// 是目录还是文件。决定跳过去之后是「进去」还是「在父目录里亮出来」,
    /// 也决定结果行画哪个图标。
    pub is_dir: bool,
}

/// 一次递归搜索的遍历状态。BFS。
///
/// **BFS 不是 DFS**:用户要找的东西绝大多数就在浅层,深度优先会一头扎进
/// 某个 `node_modules` 里,封顶用完了还没回到第二层。
pub struct Walk {
    root: RemotePath,
    query: String,
    show_hidden: bool,
    /// 还没发出去的目录。
    pending: std::collections::VecDeque<RemotePath>,
    /// 已经发出去、还没回来的条数。**收工判据要用它**,见 `status`。
    inflight: usize,
    /// 已经**列过**的目录数(成功失败都算)。封顶判据。
    visited: usize,
    hits: Vec<Hit>,
    stop: Option<Stop>,
    /// 列不出来的目录数(多半是权限不足)。界面上顺带报一句 ——
    /// 静默跳过的话,用户会以为那几棵子树里真的没有他要的东西。
    skipped: usize,
}

impl Walk {
    /// 起一次搜索。`query` 由调用方保证非空(空串会让 [`matches`] 恒不命中,
    /// 白跑一整棵树)。
    pub fn new(root: RemotePath, query: String, show_hidden: bool) -> Self {
        let mut pending = std::collections::VecDeque::new();
        pending.push_back(root.clone());
        Self {
            root,
            query,
            show_hidden,
            pending,
            inflight: 0,
            visited: 0,
            hits: Vec::new(),
            stop: None,
            skipped: 0,
        }
    }

    /// 这一轮可以发出去几个目录。照 `queue::Queue::take_runnable` 的形状。
    ///
    /// **已经停了就一个都不吐**:到顶之后再发请求,结果只会被 `accept` 原样
    /// 丢掉,白白占着链路。
    pub fn take_runnable(&mut self) -> Vec<RemotePath> {
        if self.stop.is_some() {
            return Vec::new();
        }
        let room = CONCURRENCY.saturating_sub(self.inflight);
        let mut out = Vec::new();
        while out.len() < room {
            // 封顶判在**发出去之前**:判在回来的时候的话,最后一轮会超发
            // 到 2003 个,而且那三个的结果照样被收下。
            if self.visited + self.inflight >= MAX_DIRS {
                // pending 还有东西 = 真的被封顶截断了;空了 = 正好翻完,
                // 那由 `status` 判成 `Exhausted`。
                if !self.pending.is_empty() {
                    self.stop = Some(Stop::DirCap);
                }
                break;
            }
            match self.pending.pop_front() {
                Some(d) => {
                    self.inflight += 1;
                    out.push(d);
                }
                None => break,
            }
        }
        out
    }

    /// 一次列目录回来了。
    ///
    /// **无论成败都要调**:不调的话 `inflight` 永远减不回去,`status` 就
    /// 永远判不出收工 —— 界面上是一个转到天荒地老的「正在搜索」。
    ///
    /// 单条失败**不中止整次搜索**:权限不足的目录在真实机器上遍地都是
    /// (`/proc`、别人的家目录),一条就把整次搜索判死的话,这个功能在
    /// 任何一台真机上都用不了。
    pub fn accept(&mut self, dir: &RemotePath, result: Result<Vec<Entry>, String>) {
        self.inflight = self.inflight.saturating_sub(1);
        self.visited += 1;
        if self.stop.is_some() {
            return;
        }
        let entries = match result {
            Ok(v) => v,
            Err(_) => {
                self.skipped += 1;
                return;
            }
        };
        for e in entries {
            let name = e.name.as_bytes();
            // 隐藏项随 `show_hidden`:**既不匹配也不递归**。只挡匹配不挡
            // 递归的话,关着开关搜一次家目录照样要爬完整个 `.cache`。
            if !self.show_hidden && name.starts_with(b".") {
                continue;
            }
            let full = dir.join(name);
            if matches(&self.query, &e.name.display()) {
                self.hits.push(Hit {
                    path: full.clone(),
                    is_dir: e.kind == EntryKind::Dir,
                });
                if self.hits.len() >= MAX_HITS {
                    self.stop = Some(Stop::HitCap);
                    return;
                }
            }
            // **只有 `EntryKind::Dir` 往下走**。`list_dir` 是 lstat 语义
            // (见 `SftpClient::stat` 的文档),指向目录的软链接是
            // `EntryKind::Symlink`,自然被挡在外面 —— 这正是 spec 要的
            // 「不跟符号链接」。跟了的话 `a -> ..` 这种环会让遍历永不收敛,
            // 只有 2000 的封顶兜着,而那 2000 次往返全是白跑的。
            if e.kind == EntryKind::Dir {
                self.pending.push_back(full);
            }
        }
    }

    /// 用户按了取消。
    pub fn cancel(&mut self) {
        if self.stop.is_none() {
            self.stop = Some(Stop::Canceled);
        }
    }

    /// 还在跑吗。`None` = 还在跑;`Some(stop)` = 停了,原因在里面。
    ///
    /// **收工判据是「待列空 **且** 在飞为零」**。只看 `pending.is_empty()`
    /// 的话,最后一批还在路上的时候就会报「搜完了,0 个结果」,而半秒后
    /// 结果才回来 —— 用户已经看过那句「没找到」并关掉了。
    pub fn status(&self) -> Option<Stop> {
        if let Some(s) = self.stop {
            return Some(s);
        }
        if self.pending.is_empty() && self.inflight == 0 {
            return Some(Stop::Exhausted);
        }
        None
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }
    pub fn visited(&self) -> usize {
        self.visited
    }
    pub fn skipped(&self) -> usize {
        self.skipped
    }
    pub fn root(&self) -> &RemotePath {
        &self.root
    }
    pub fn query(&self) -> &str {
        &self.query
    }
}
```

- [ ] **Step 2: 测试追加到 `mod tests` 里**

```rust
    fn e(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: rp(name),
            kind,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    /// 把一次搜索跑到底:反复 `take_runnable` → 从 `tree` 里取目录内容
    /// → `accept`。返回命中的相对路径,按发现顺序。
    fn run(
        root: &str,
        query: &str,
        show_hidden: bool,
        tree: &[(&str, Vec<Entry>)],
    ) -> (Vec<String>, Stop, usize) {
        let mut w = Walk::new(rp(root), query.to_string(), show_hidden);
        // 上限兜底:状态机写错成不收敛时,让测试**失败**而不是挂死。
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                let found = tree
                    .iter()
                    .find(|(p, _)| p.as_bytes() == d.as_bytes())
                    .map(|(_, v)| v.clone());
                w.accept(&d, found.ok_or_else(|| "没有权限".to_string()));
            }
        }
        let stop = w.status().expect("跑到底之后必须有结论");
        let rels = w
            .hits()
            .iter()
            .map(|h| relative(w.root(), &h.path))
            .collect();
        (rels, stop, w.visited())
    }

    /// 递归真的往下走,而且结果是相对路径。
    ///
    /// 自证会变红:把 `accept` 里 `self.pending.push_back(full)` 那一句删掉
    /// —— 只剩根目录那一层的命中。
    #[test]
    fn the_search_descends_into_subdirectories() {
        let tree = vec![
            ("/r", vec![e("src", EntryKind::Dir), e("a.txt", EntryKind::File)]),
            ("/r/src", vec![e("app.rs", EntryKind::File)]),
        ];
        let (hits, stop, _) = run("/r", "ap", false, &tree);
        assert_eq!(hits, vec!["src/app.rs"]);
        assert_eq!(stop, Stop::Exhausted);
    }

    /// **不跟符号链接**:指向目录的软链接是 `EntryKind::Symlink`,不入队。
    ///
    /// 自证会变红:把 `if e.kind == EntryKind::Dir` 改成
    /// `if matches!(e.kind, EntryKind::Dir | EntryKind::Symlink)` —— `/r/link`
    /// 会被列,`visited` 从 1 变成 2。
    #[test]
    fn a_symlink_is_never_followed_even_when_it_points_at_a_directory() {
        let tree = vec![
            ("/r", vec![e("link", EntryKind::Symlink)]),
            // 真去列它就会命中这一条 —— 不跟链接的话永远看不见。
            ("/r/link", vec![e("inside.txt", EntryKind::File)]),
        ];
        let (hits, _, visited) = run("/r", "inside", false, &tree);
        assert!(hits.is_empty(), "跟着软链接走了:{hits:?}");
        assert_eq!(visited, 1, "只该列根目录一个");
    }

    /// 隐藏项随 `show_hidden`:关着的时候**既不匹配也不递归**。
    ///
    /// 自证会变红:把 `accept` 里 `if !self.show_hidden && ...` 那三行删掉
    /// (第一组断言红);或只改成「过滤命中、照样入队」(`visited` 那条红)。
    #[test]
    fn hidden_entries_are_neither_matched_nor_descended_into() {
        let tree = vec![
            (
                "/r",
                vec![e(".git", EntryKind::Dir), e(".env", EntryKind::File)],
            ),
            ("/r/.git", vec![e("config", EntryKind::File)]),
        ];
        let (off, _, visited_off) = run("/r", "cfg", false, &tree);
        assert!(off.is_empty());
        assert_eq!(visited_off, 1, "关着开关时不该进 .git");

        let (on, _, visited_on) = run("/r", "config", true, &tree);
        assert_eq!(on, vec![".git/config"]);
        assert_eq!(visited_on, 2);

        let (dot, _, _) = run("/r", ".env", true, &tree);
        assert_eq!(dot, vec![".env"], "开着开关时隐藏文件本身也该被匹配");
    }

    /// 命中封顶:到 500 就停,而且状态说得出是「封顶」不是「搜完了」。
    ///
    /// 混成一句的话用户会以为结果就这些 —— 而它可能还差得远。
    ///
    /// 自证会变红:把 `if self.hits.len() >= MAX_HITS` 那一段删掉。
    #[test]
    fn hitting_the_result_cap_stops_and_says_so() {
        let many: Vec<Entry> = (0..MAX_HITS + 50)
            .map(|i| e(Box::leak(format!("f{i}.txt").into_boxed_str()), EntryKind::File))
            .collect();
        let tree = vec![("/r", many)];
        let (hits, stop, _) = run("/r", "f", false, &tree);
        assert_eq!(hits.len(), MAX_HITS);
        assert_eq!(stop, Stop::HitCap);
    }

    /// 目录封顶:判在**发出去之前**。判在回来时的话最后一轮会超发。
    ///
    /// 自证会变红:把 `take_runnable` 里 `self.visited + self.inflight >= MAX_DIRS`
    /// 那一段删掉 —— `visited` 会冲到 2001 以上。
    #[test]
    fn the_directory_cap_is_enforced_before_the_requests_go_out() {
        // 一棵永远生得出新目录的树:根下每个目录里再放一个目录。
        let mut w = Walk::new(rp("/r"), "zzz".to_string(), false);
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                w.accept(&d, Ok(vec![e("sub", EntryKind::Dir)]));
            }
        }
        assert_eq!(w.status(), Some(Stop::DirCap));
        assert!(
            w.visited() <= MAX_DIRS,
            "超发了:列了 {} 个目录,上限 {MAX_DIRS}",
            w.visited()
        );
    }

    /// 正好翻完不该被报成「封顶」—— 那两句话对用户的含义相反。
    ///
    /// 自证会变红:把 `take_runnable` 里 `if !self.pending.is_empty()` 那道
    /// 门去掉,改成无条件 `self.stop = Some(Stop::DirCap)`。
    #[test]
    fn a_tree_that_ends_exactly_at_the_cap_is_exhausted_not_capped() {
        let mut w = Walk::new(rp("/r"), "zzz".to_string(), false);
        let mut left = MAX_DIRS;
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                left -= 1;
                // 最后一个目录不再生子目录 —— 树到此为止,正好 MAX_DIRS 个。
                let kids = if left > 0 { vec![e("sub", EntryKind::Dir)] } else { vec![] };
                w.accept(&d, Ok(kids));
            }
        }
        assert_eq!(w.visited(), MAX_DIRS);
        assert_eq!(w.status(), Some(Stop::Exhausted));
    }

    /// **收工判据是「待列空 且 在飞为零」**。只看 `pending` 的话,最后一批
    /// 还在路上时就会报「搜完了、没找到」,而半秒后结果才回来。
    ///
    /// 自证会变红:把 `status` 里的 `&& self.inflight == 0` 删掉。
    #[test]
    fn a_search_with_requests_still_in_flight_is_not_finished() {
        let mut w = Walk::new(rp("/r"), "a".to_string(), false);
        let batch = w.take_runnable();
        assert_eq!(batch.len(), 1);
        assert_eq!(w.status(), None, "发出去了还没回来,不算搜完");
        w.accept(&batch[0], Ok(vec![e("a.txt", EntryKind::File)]));
        assert_eq!(w.status(), Some(Stop::Exhausted));
    }

    /// 单条目录列不出来(权限不足)**不中止整次搜索**,但要留痕。
    ///
    /// 自证会变红:把 `accept` 里 `Err(_) => { self.skipped += 1; return; }`
    /// 改成 `Err(_) => { self.stop = Some(Stop::Canceled); return; }`。
    #[test]
    fn a_directory_we_cannot_read_is_skipped_rather_than_killing_the_search() {
        let tree = vec![
            (
                "/r",
                vec![e("locked", EntryKind::Dir), e("ok", EntryKind::Dir)],
            ),
            // `/r/locked` 故意不在树里 —— `run` 会给它一个 Err。
            ("/r/ok", vec![e("apple.txt", EntryKind::File)]),
        ];
        let (hits, stop, _) = run("/r", "apple", false, &tree);
        assert_eq!(hits, vec!["ok/apple.txt"]);
        assert_eq!(stop, Stop::Exhausted);
    }

    /// 取消之后不再吐任何请求 —— 吐了的话取消只是界面上的假象,链路还在跑。
    ///
    /// 自证会变红:把 `take_runnable` 开头 `if self.stop.is_some()` 那三行删掉。
    #[test]
    fn canceling_stops_new_requests_from_going_out() {
        let mut w = Walk::new(rp("/r"), "a".to_string(), false);
        let batch = w.take_runnable();
        w.accept(&batch[0], Ok(vec![e("d1", EntryKind::Dir), e("d2", EntryKind::Dir)]));
        w.cancel();
        assert!(w.take_runnable().is_empty());
        assert_eq!(w.status(), Some(Stop::Canceled));
    }

    /// BFS:浅的先出。深度优先会一头扎进某个 node_modules,封顶用完了还没
    /// 回到第二层。
    ///
    /// 自证会变红:把 `pending` 换成 `Vec` + `pop()`(后进先出)。
    #[test]
    fn the_walk_goes_breadth_first_so_shallow_hits_come_out_first() {
        let tree = vec![
            (
                "/r",
                vec![e("deep", EntryKind::Dir), e("a-shallow", EntryKind::File)],
            ),
            ("/r/deep", vec![e("a-deep", EntryKind::File)]),
        ];
        let (hits, _, _) = run("/r", "a", false, &tree);
        assert_eq!(hits, vec!["a-shallow", "deep/a-deep"]);
    }
}
```

**注意:** `run` 里那个 `for _ in 0..100_000` 不是凑数 —— 状态机写错成不收敛(比如封顶判据被删)时,它让测试**失败**而不是挂死。跑测试的人看到红比看到卡住有用得多。

- [ ] **Step 3: 跑测试**

```bash
cargo test -p mullion-app files::find:: 2>&1 | tail -20
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: 16 passed;clippy 无输出。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/src/files/find.rs
git commit -m "feat(app): 递归搜索的遍历状态机(BFS + 封顶 + 不跟链接) (F278)"
```

---

### Task 3: `Glyph::Search` 自绘图标

**Files:**
- Modify: `crates/mullion-app/src/ui/icon.rs`

**背景(T9):** 往 egui 的 UI 串里直接写 `🔍`(U+1F50D)或 `⌕`(U+2315)都是豆腐块 —— 两个都不在 GBK,字体链两级都没有,而且**编译/测试/日志全静默,只有人眼能看见**。既有做法是自绘(见 `Glyph::Refresh` 的文档:它顶掉的正是同一类字符)。

- [ ] **Step 1: 加枚举变体**

在 `Glyph::Project,` 之后加:

```rust
    /// F278:放大镜 —— 文件面板路径条上的「在这个目录下递归搜索」。
    ///
    /// 自绘而不是找一个放大镜字符:U+1F50D 与 U+2315 都在 GBK 外,
    /// 两级字体链都画不出来,而这种缺失**只有人眼能发现**(T9)。
    Search,
```

- [ ] **Step 2: 登记进 `ALL`**

在 `Glyph::Project,` 之后加 `Glyph::Search,`。

- [ ] **Step 3: 在 `shapes()` 的 match 里画**

在 `Glyph::Project => ...` 那一臂之后加(位置照枚举顺序):

```rust
        // 圆环 + 一条向右下的柄。半径取 `h * 0.6`,柄从圆周 45° 处再伸
        // `h * 0.45`,合计 0.6*0.707 + 0.45 ≈ 0.87h,仍在框内 ——
        // `every_glyph_stays_inside_its_rect` 正是为此存在。
        Glyph::Search => {
            let r = h * 0.6;
            // 圆心往左上挪一点,给柄腾地方,整体视觉重心才落在框中间。
            let o = pos2(c.x - h * 0.15, c.y - h * 0.15);
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let from = pos2(o.x + d, o.y + d);
            let to = pos2(o.x + d + h * 0.45, o.y + d + h * 0.45);
            vec![
                Shape::circle_stroke(o, r, stroke),
                Shape::LineSegment {
                    points: [from, to],
                    stroke: stroke.into(),
                },
            ]
        }
```

- [ ] **Step 4: 跑既有守护**

`icon.rs` 里已经有「每个图标的笔画都不越界」与「`ALL` 覆盖全部变体」两条测试,新变体自动被它们覆盖。

```bash
cargo test -p mullion-app ui::icon:: 2>&1 | tail -15
```
Expected: 全过。**如果 `every_glyph_stays_inside_its_rect` 红了**,说明柄伸出框了 —— 把 `h * 0.45` 调小,不要改测试。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/icon.rs
git commit -m "feat(app): 自绘放大镜图标 (F278)"
```

---

### Task 4: `Modal::FilesFind` 全量登记

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

**为什么必须单独一个任务:** `Modal` 是列举式门控,本项目已经因为「加档漏登记」踩过三次。搜索框是个 `TextEdit`,**不登记的话它一个键都收不到**(T8:面板持有键盘焦点时键根本不喂 egui),而且 Backspace 会被 `handle_panel_key` 解释成「回上级目录」—— 一按就跳走。

- [ ] **Step 1: 九处登记,一处不许少**

1. **枚举变体**(紧跟 `FilesNewName,` 之后):

```rust
    /// F278:文件面板的**递归搜索条**正开着。理由与 `FilesPathEdit` 逐字
    /// 相同 —— 那个输入框收不到任何键(T8),而 Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」,一按就跳走。
    ///
    /// **不进 `touched_store`**:它一行 store 都不写(同 `FilesPathEdit`)。
    FilesFind,
```

2. **`Modal::ALL`**:在 `Modal::FilesNewName,` 之后加 `Modal::FilesFind,`。

3. **`DISMISS_EXEMPT`**:在 `Modal::FilesNewName,` 之后加 `Modal::FilesFind,`;并在它上方那段列表注释里,把 `FilesPathEdit`/`FilesRename`/`FilesNewName` 那一条改成也带上 `FilesFind`:

```
/// - `FilesPathEdit`/`FilesRename`/`FilesNewName`/`FilesFind`:就地输入框,
///   不是窗口,没有「外面」可言(它们的 area 就是文件面板本身)。
```

4. **`draft_baseline_is_in_vault`** 的 `false` 那一臂:把 `| Modal::FilesNewName` 之后接上 `| Modal::FilesFind`。

5. **`dismiss_areas`** 的 `None` 那一臂:`Modal::FilesNewName` 之后接上 `| Modal::FilesFind`。

6. **开关判据分派**(`app.rs:4620` 附近,`Modal::FilesPathEdit => self.files_path_editing(),` 那一组):

```rust
            Modal::FilesFind => self.files_finding(),
```

7. **`app.rs:4712` 那张表**(`Modal::FilesPathEdit | Modal::FilesRename | Modal::FilesNewName => false,`):接上 `| Modal::FilesFind`。

8. **`app.rs:4801` 那张表**:同样在 `| Modal::FilesPathEdit` 所在的那一组里接上 `| Modal::FilesFind`。

9. **新增判据方法**。照 `files_path_editing` 的写法(它把真判据拆成了收显式参数的自由函数 `files_path_editing_of`,好在 `app.rs` 里单测 —— `app.rs` 的测试构造不出 `App`)。在 `files_path_editing` 旁边加:

```rust
    /// F278:哪个标签的文件面板此刻开着搜索条。
    ///
    /// 拆成自由函数的理由同 `files_path_editing_of`:`app.rs` 的测试从来
    /// 构造不出一个 `App`(要窗口/GPU),挂成 `&self` 方法的话这条判据根本
    /// 没法单测,而它正是 `Modal::FilesFind` 唯一读的东西。
    fn files_finding(&self) -> bool {
        files_finding_of(&self.tabs)
    }
```

自由函数放在 `files_path_editing_of` 旁边:

```rust
/// F278:有没有任何一个标签的文件面板开着搜索条。
///
/// **遍历全部标签**,不只活动那个:搜索条开着的时候用户完全可能切到别的
/// 标签 —— 按活动标签判的话,切回来时那个框又收不到键了,而且没有任何提示。
fn files_finding_of(tabs: &Tabs) -> bool {
    tabs.iter()
        .filter_map(|t| t.content.files_panel())
        .any(|f| f.find.is_some())
}
```

> **本任务连 `PanelFrame::find` 字段一起加**,不要先留个 `fn files_finding(&self) -> bool { false }` 的空壳等 Task 5 回填 —— 那样 `the_files_find_bar_is_registered_everywhere_a_modal_has_to_be` 会在一个恒为 `false` 的判据上判绿,而「登记齐了」与「判据真的读得到状态」是两件事。字段是纯数据,先加上不影响任何既有行为。

**所以本任务同时改 `files_panel.rs`**:在 `pub struct PanelFrame` 的 `clip` 字段之后加:

```rust
    /// F278:这一栏此刻开着的递归搜索。`None` = 没开(默认)。
    ///
    /// **是纯数据,符合 `impl Default for PanelFrame` 上方那条警告** ——
    /// `None` 是真实可用的初值,不是编出来的假值。搜索任务的句柄**不在这里**,
    /// 走 `track_sftp_task` 进 `TerminalTab::sftp_tasks`(那条警告点名禁止的
    /// 正是「后台任务句柄进 PanelFrame」)。
    ///
    /// 只有远端栏用得上(本切片范围,见计划里的范围决策 1),所以挂在
    /// `PanelFrame` 上而不是 `PaneState` 上 —— 挂进 `PaneState` 的话本地栏
    /// 也会平白多出一个永远是 `None` 的字段。
    pub find: Option<Find>,
    /// F278:起搜计数器。每起一次搜索 +1,发出去的 `list_dir` 带着它,回来
    /// 对不上就丢 —— 取消/换目录/换机器一律靠递增它作废在途的结果,
    /// **绝不 abort**(`sftp_tasks` 是混合池,见 `reopen_sftp_on_focused_host`
    /// 上方那段长注释)。
    pub find_seq: u64,
```

以及 `Find` 类型本身(放在 `PanelFrame` 定义之前):

```rust
/// F278:一次递归搜索的界面状态。
#[derive(Default)]
pub struct Find {
    /// 搜索框缓冲。
    pub buf: String,
    /// 刚打开搜索条、**还没把键盘焦点要过来**。渲染那侧要一次就清掉。
    ///
    /// 用一次性标志而不是「每帧发现没焦点就抢回来」:同 `RenameEdit::focus_pending`
    /// 的理由 —— 无条件每帧 `request_focus()` 会让两个输入框互抢,先进去的
    /// 那个永远 `lost_focus()` 不了、退不出来。
    pub focus_pending: bool,
    /// 正在跑(或刚跑完)的那次遍历。`None` = 搜索条开着、还没按过 Enter。
    pub walk: Option<crate::files::find::Walk>,
    /// `walk` 那一次的序号,对齐 `PanelFrame::find_seq`。
    pub seq: u64,
}
```

`Default` 里 `PanelFrame` 那两个字段填 `find: None, find_seq: 0`。

> `Walk` 没有 `Default`,但 `Find` 的 `walk` 是 `Option<Walk>`,`#[derive(Default)]` 能推出来。

- [ ] **Step 2: 写完备性守护**

`app.rs` 里已有一条按源码切片钉 `FilesPathEdit` 三处登记的测试(约 `app.rs:28639`)。照它加一条:

```rust
    /// F278:`Modal::FilesFind` 的三处登记一处都不许少。
    ///
    /// 漏 `ALL` 的症状:`modal_open` 照 `ALL` 遍历,漏了就等于这个弹窗
    /// 「开着也不算开着」—— 搜索框一个键都收不到(T8),而且 Backspace 被
    /// `handle_panel_key` 解释成回上级目录,一按就跳走。
    ///
    /// 自证会变红:把 `Modal::FilesFind` 从 `Modal::ALL` 里删掉(第二条红);
    /// 把那条分派臂删掉(第三条红)。
    #[test]
    fn the_files_find_bar_is_registered_everywhere_a_modal_has_to_be() {
        let prod = prod_src();
        assert!(prod.contains("    FilesFind,"), "Modal 枚举里没有 FilesFind");
        assert!(
            prod.contains("        Modal::FilesFind,"),
            "Modal::ALL 里漏了 FilesFind —— modal_open 照 ALL 遍历,漏了就永远算「没开」"
        );
        assert!(
            prod.contains("            Modal::FilesFind => self.files_finding(),"),
            "开关判据没分派 —— FilesFind 永远判成关着"
        );
    }
```

再加一条钉「判据遍历全部标签」的:

```rust
    /// F278:搜索条开着的时候用户完全可能切到别的标签 —— 按活动标签判的话,
    /// 切回来时那个框又收不到键了,而且没有任何提示。
    ///
    /// 自证会变红:把 `files_finding_of` 里的 `tabs.iter()` 换成只看活动标签。
    #[test]
    fn a_find_bar_on_a_background_tab_still_counts_as_open() {
        let body = strip_comments(body_of(prod_src(), "fn files_finding_of("));
        assert!(
            body.contains("tabs.iter()"),
            "没有遍历全部标签 —— 切走再切回来,搜索框就收不到键了:{body}"
        );
        assert!(
            !body.contains("active"),
            "按活动标签判了:{body}"
        );
    }
```

> `prod_src` / `strip_comments` / `body_of` 是 `app.rs` 测试模块里的既有辅助函数,直接用。**注意「源码切片守护不剥注释」那条既有欠账**:上面第二条测试里 `body_of` 之后**必须**套 `strip_comments`,否则函数体里的注释文字会喂饱 `.contains()`。第一条钉的是带缩进的精确字面行,不受注释影响。

- [ ] **Step 3: 跑**

```bash
cargo test -p mullion-app 2>&1 | tail -5
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 递归搜索的面板状态 + Modal::FilesFind 登记 (F278)"
```

---

### Task 5: 放大镜按钮 + 搜索条 UI

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`

- [ ] **Step 1: 加 5 个 `FileAction` 变体**

在 `pub enum FileAction` 末尾加:

```rust
    /// F278:点了路径条上的放大镜。开搜索条;已经开着就关掉(同一颗按钮
    /// 两用,与侧栏开关同一种心智)。
    FindToggle,
    /// F278:在搜索框里按了回车。串是**原文**,匹配判据在 `files::find` 里,
    /// 起搜要拿当前目录当根 —— 而当前目录 app 侧本来就知道。
    FindStart(String),
    /// F278:用户按了取消(或 Esc)—— 停在原地,已经找到的结果留着。
    FindCancel,
    /// F278:关掉搜索条,回到普通列表。
    FindClose,
    /// F278:点中了一条搜索结果。**绝对路径**,由面板从 `Hit` 里直接取 ——
    /// 与 `Rename`/`NewFile` 同一条约定(路径在面板侧拼好),理由见那两条。
    FindPick {
        path: mullion_ssh::sftp::RemotePath,
        is_dir: bool,
    },
```

- [ ] **Step 2: 路径条上加放大镜**

在 `files_panel.rs` 的路径条 `ui.horizontal` 里,**书签 ▾ 那个 `add_enabled_ui` 块之后、`annotate::mark(ui.ctx(), format!("文件面板/{id}/路径"), ui.max_rect());` 之前**插入:

```rust
        // F278:递归搜索。**必须画在路径标签之前** —— 理由与上面书签那两颗
        // 逐字相同:下面那个 `Label` 用 `available_width` 吃掉整行剩余宽度,
        // 排在它后面的按钮会被挤出可视区。
        //
        // **只有远端栏有**(本切片范围):本地栏递归要另配一条
        // `spawn_blocking` 通路 —— 本地列目录目前同步跑在事件循环线程上
        // (见 `app.rs::apply_local_file_action` 上方那段注释),递归 2000 个
        // 目录会把整个窗口连同终端一起卡住。
        if column == PanelColumn::Remote {
            let on = find.is_some();
            if crate::ui::icon::icon_button(
                ui,
                crate::ui::icon::Glyph::Search,
                true,
                if on { "关闭搜索" } else { "在这个目录下递归搜索" },
            ) {
                action = Some(FileAction::FindToggle);
            }
        }
```

> `find` 是本函数新加的参数,见 Step 4。

- [ ] **Step 3: 路径条之后画搜索条**

在路径条那个 `ui.horizontal(|ui| { ... });` 整块**之后**、`match &state.load {` 之前插入:

```rust
    // F278:搜索条。画在路径条下面一行,`None` 时一点高度都不占。
    if let Some(f) = find.as_mut() {
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut f.buf)
                    .id(find_edit_id(id))
                    .hint_text("文件名(模糊匹配,回车开始)")
                    .desired_width(ui.available_width() * 0.5),
            );
            // **只在刚打开那一刻请求一次焦点**。无条件每帧 `request_focus()`
            // 会让它跟路径条的输入框互抢,先进去的那个永远退不出来 ——
            // 同 `PaneState::path_edit` 文档里记的那次复核实测。
            if f.focus_pending {
                resp.request_focus();
                f.focus_pending = false;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let q = f.buf.trim().to_string();
                // 空串不起搜:`find::matches` 对空查询恒不命中,发出去就是
                // 白爬一整棵树。
                if !q.is_empty() {
                    action = Some(FileAction::FindStart(q));
                }
            }
            match f.walk.as_ref() {
                None => {
                    if ui.button("搜索").clicked() {
                        let q = f.buf.trim().to_string();
                        if !q.is_empty() {
                            action = Some(FileAction::FindStart(q));
                        }
                    }
                }
                Some(w) => {
                    let done = w.status();
                    ui.colored_label(theme::c32(t.fg_dim), find_progress_text(w));
                    if done.is_none() && ui.button("取消").clicked() {
                        action = Some(FileAction::FindCancel);
                    }
                    if done.is_some() && ui.button("重新搜索").clicked() {
                        let q = f.buf.trim().to_string();
                        if !q.is_empty() {
                            action = Some(FileAction::FindStart(q));
                        }
                    }
                }
            }
            if crate::ui::icon::icon_button(ui, crate::ui::icon::Glyph::Cross, true, "关闭搜索(Esc)")
            {
                action = Some(FileAction::FindClose);
            }
        });
        annotate::mark(ui.ctx(), format!("文件面板/{id}/搜索"), ui.max_rect());
    }
```

配套的两个自由函数(放在这个文件里既有的 `path_edit_id` 旁边):

```rust
fn find_edit_id(id: &str) -> egui::Id {
    egui::Id::new(("files-find-edit", id))
}

/// F278:搜索条上那句进度/结论。
///
/// **四种收场各说各的话**(`find::Stop` 的四档):「翻完了,没找到」和
/// 「翻了 2000 个目录还没翻完」对用户的含义相反 —— 混成一句「没有结果」
/// 会让他以为文件不存在,而它可能就在第 2001 个目录里。
///
/// 写成收 `&Walk` 的自由函数:这条判据是纯文本,挂在渲染函数里的话就再也
/// 没法单测了,而「哪种收场说哪句话」正是最容易写反的地方。
pub fn find_progress_text(w: &crate::files::find::Walk) -> String {
    use crate::files::find::Stop;
    let n = w.hits().len();
    let d = w.visited();
    let skipped = if w.skipped() > 0 {
        format!(",{} 个目录没权限读", w.skipped())
    } else {
        String::new()
    };
    match w.status() {
        None => format!("正在搜索…已找到 {n} 个,已翻 {d} 个目录{skipped}"),
        Some(Stop::Exhausted) if n == 0 => format!("没有找到(翻了 {d} 个目录{skipped})"),
        Some(Stop::Exhausted) => format!("找到 {n} 个(翻了 {d} 个目录{skipped})"),
        Some(Stop::HitCap) => {
            format!("已达 {} 个结果上限,结果可能不全 —— 换个更长的关键词试试{skipped}", crate::files::find::MAX_HITS)
        }
        Some(Stop::DirCap) => format!(
            "已翻 {} 个目录到上限,结果可能不全 —— 换个更深的目录再搜{skipped}",
            crate::files::find::MAX_DIRS
        ),
        Some(Stop::Canceled) => format!("已取消,当时已找到 {n} 个{skipped}"),
    }
}
```

- [ ] **Step 4: 把 `find` 传进这个渲染函数**

这个栏渲染函数目前收的是 `state: &mut PaneState` 等等。`find`/`find_seq` 挂在 `PanelFrame` 上,所以调用侧(`files_panel.rs` 里画两栏那两处,约 2360/2400 与 2535/2580 各一对)要把 `&mut frame.find` 传下来。**远端栏传 `Some(&mut frame.find)`,本地栏传 `None`** —— 用参数类型把「本地栏没有搜索」这件事变成编译期事实,而不是靠渲染里的 `if` 记得住。

签名改成:

```rust
    find: Option<&mut Option<Find>>,
```

在函数体开头取出:

```rust
    // 本地栏恒 `None`(范围决策 1)。用 `Option<&mut _>` 而不是在函数体里
    // `if column == Remote` —— 本地栏压根传不进来,漏判一处也不会静默出现
    // 一个点了没反应的放大镜。
    let mut find_slot = find;
    let find = find_slot.as_deref_mut().and_then(|s| s.as_mut());
```

> 实现者按实际签名调整;要点只有两个:**本地栏传不进来**;`find` 在函数里是 `Option<&mut Find>`。

- [ ] **Step 5: 测试**

在 `files_panel.rs` 的 `mod tests` 里加:

```rust
    /// F278:四种收场各说各的话。**「翻完了,没找到」和「翻了 2000 个目录
    /// 还没翻完」对用户的含义相反** —— 混成一句会让他以为文件不存在。
    ///
    /// 自证会变红:把 `Some(Stop::DirCap)` 那一臂并进 `Exhausted`。
    #[test]
    fn each_way_a_search_can_end_says_a_different_thing() {
        use crate::files::find::{Stop, Walk, MAX_DIRS, MAX_HITS};
        // 四种状态各造一台 Walk。`Walk` 的内部字段不公开,靠公开 API 摆到位。
        let mut done = Walk::new(rp("/r"), "zz".into(), false);
        let b = done.take_runnable();
        done.accept(&b[0], Ok(vec![]));
        assert_eq!(done.status(), Some(Stop::Exhausted));
        let s_done = find_progress_text(&done);
        assert!(s_done.contains("没有找到"), "{s_done}");

        let mut canceled = Walk::new(rp("/r"), "zz".into(), false);
        canceled.cancel();
        let s_cancel = find_progress_text(&canceled);
        assert!(s_cancel.contains("已取消"), "{s_cancel}");
        assert_ne!(s_cancel, s_done);

        // 跑一次到封顶(树永远生得出新目录)。
        let mut capped = Walk::new(rp("/r"), "zz".into(), false);
        for _ in 0..100_000 {
            let batch = capped.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                capped.accept(&d, Ok(vec![entry_dir("sub")]));
            }
        }
        let s_cap = find_progress_text(&capped);
        assert!(
            s_cap.contains(&MAX_DIRS.to_string()) && s_cap.contains("不全"),
            "目录封顶必须说「可能不全」并报出上限:{s_cap}"
        );
        assert_ne!(s_cap, s_done, "封顶和搜完不能说同一句话");
        let _ = MAX_HITS;
    }

    /// F278:还在跑的时候那句话里有「正在」,不能长得像结论。
    #[test]
    fn a_running_search_does_not_read_like_a_conclusion() {
        use crate::files::find::Walk;
        let mut w = Walk::new(rp("/r"), "zz".into(), false);
        let _ = w.take_runnable(); // 发出去了,还没回来
        let s = find_progress_text(&w);
        assert!(s.contains("正在搜索"), "{s}");
    }
```

> `rp` / `entry_dir` 是本文件测试模块里的辅助函数;没有的话照 `files/find.rs` 测试里的 `rp`/`e` 现写两个。

- [ ] **Step 6: 跑 + Commit**

```bash
cargo test -p mullion-app 2>&1 | tail -5
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 路径条放大镜 + 搜索条 (F278)"
```

---

### Task 6: 异步回路 —— 事件 / spawn / pump / accept

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 加 `UserEvent::FindListed`**

在 `PathProbed { .. }` 之后:

```rust
    /// F278:递归搜索里的一次列目录回来了。
    ///
    /// **不蹭 `SftpListed`**,理由与 `PathProbed` 不蹭 `RevealStat` 逐字相同:
    /// 收方的序号空间不同。`SftpListed` 对齐 `PaneState::request_seq`(每栏
    /// 一份,「用户点得比网络快」的后发先至判据),这一条对齐
    /// `PanelFrame::find_seq`(每面板一份,起搜一次 +1)。共用一个事件就会
    /// 撞号,撞上就是「一次普通列目录被搜索结果顶掉」,面板跳到一个用户
    /// 没去过的目录,而且完全静默。
    ///
    /// `dir` 原样带回来:`Walk::accept` 要拿它拼子项的绝对路径。
    FindListed {
        generation: u64,
        seq: u64,
        dir: mullion_ssh::sftp::RemotePath,
        result: Result<Vec<mullion_ssh::sftp::Entry>, String>,
    },
```

同时在 `app.rs:15666` 附近那个列事件名的地方(`| SftpOpened { .. } | SftpListed { .. }`)接上 `| FindListed { .. }`。

- [ ] **Step 2: 加 spawn 函数**

在 `spawn_sftp_path_probe` 之后:

```rust
/// F278:递归搜索里的一次列目录。结果经 `UserEvent::FindListed` 回送
/// (`App::accept_find_listed` 接)。
///
/// 跟 `spawn_sftp_list_dir` 发的是同一句 `list_dir`,**分成两个函数只为了发
/// 不同的事件** —— 收方的序号空间不同,理由见 `UserEvent::FindListed` 的文档。
///
/// 同样**返回 `JoinHandle`,调用方必须存进 `sftp_tasks`** —— 理由同
/// `spawn_sftp_list_dir`。注意收口**只靠 `wind_down`**:取消搜索走的是
/// 「递增 `find_seq` 让迟到的结果对不上号」,**绝不 abort**
/// (`sftp_tasks` 是混合池,无差别 abort 会腰斩传输 —— 见
/// `reopen_sftp_on_focused_host` 上方那段长注释)。
fn spawn_sftp_find_list(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    client: Arc<mullion_ssh::sftp::SftpClient>,
    dir: mullion_ssh::sftp::RemotePath,
    seq: u64,
) -> tokio::task::JoinHandle<()> {
    let proxy = proxy.clone();
    runtime.spawn(async move {
        let result = client
            .list_dir(&dir)
            .await
            .map_err(|e| format!("读取目录失败:{e}"));
        let _ = proxy.send_event(UserEvent::FindListed {
            generation,
            seq,
            dir,
            result,
        });
    })
}
```

- [ ] **Step 3: `pump_find` —— 每帧驱动**

在 `pump_transfers` 旁边:

```rust
    /// F278:每帧调一次 —— 每个标签的搜索状态机放行几个目录就发几条 `list_dir`。
    ///
    /// **遍历全部标签,不只活动那个**(那条「`drive_*` 每帧驱动函数必须遍历
    /// 全部标签」的纪律):搜索要跑好几秒,期间用户完全可能切到别的标签去 ——
    /// 只推活动标签的话,切回来会发现它停在半路,而且完全静默。
    /// (`pump_transfers` 不用遍历是因为它的队列是全局一份;`find` 每标签一份。)
    fn pump_find(&mut self) {
        // 先把「哪个标签要发哪几个目录」收集出来,再统一 spawn ——
        // `track_sftp_task` 要 `&mut self`,借着 `self.tabs` 是调不了的。
        let mut work: Vec<(u64, Arc<mullion_ssh::sftp::SftpClient>, u64, Vec<mullion_ssh::sftp::RemotePath>)> =
            Vec::new();
        for tab in self.tabs.iter_mut() {
            let generation = tab.generation;
            let Some(client) = tab.content.sftp_client() else {
                continue;
            };
            let Some(files) = tab.content.files_panel_mut() else {
                continue;
            };
            let Some(f) = files.find.as_mut() else { continue };
            let seq = f.seq;
            let Some(walk) = f.walk.as_mut() else { continue };
            let batch = walk.take_runnable();
            if !batch.is_empty() {
                work.push((generation, client, seq, batch));
            }
        }
        for (generation, client, seq, batch) in work {
            for dir in batch {
                let task = spawn_sftp_find_list(
                    &self._runtime,
                    &self.proxy,
                    generation,
                    client.clone(),
                    dir,
                    seq,
                );
                self.track_sftp_task(generation, task);
            }
        }
    }
```

> `self.tabs.iter_mut()` / `tab.generation` / `tab.content.sftp_client()` 的实际名字按 `app.rs` 里既有写法对齐(`files_owner_generation_of` 与 `drive_attach_checks_of` 附近有现成的遍历例子)。

在 `self.pump_transfers();`(`app.rs:12901`)那一行之后加 `self.pump_find();`。

- [ ] **Step 4: `accept_find_listed`**

```rust
    /// F278:搜索里的一次列目录回来了。
    ///
    /// `seq` 对不上就**整条丢掉**:取消、换关键词、换目录、换机器都靠递增
    /// `find_seq` 作废在途结果(绝不 abort,见 `spawn_sftp_find_list` 的文档)。
    /// 不校验的话,取消之后半秒里回来的结果会往一个已经不存在的搜索里塞,
    /// 或者更糟 —— 塞进用户刚起的**新**那一次。
    fn accept_find_listed(
        &mut self,
        generation: u64,
        seq: u64,
        dir: mullion_ssh::sftp::RemotePath,
        result: Result<Vec<mullion_ssh::sftp::Entry>, String>,
    ) {
        let Some(files) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
        else {
            return;
        };
        let Some(f) = files.find.as_mut() else { return };
        if f.seq != seq {
            return;
        }
        let Some(walk) = f.walk.as_mut() else { return };
        walk.accept(&dir, result);
        self.request_ui_redraw();
    }
```

在事件分派处(`app.rs:12005` 附近 `UserEvent::SftpListed { .. } => ...` 那一组)接上:

```rust
            UserEvent::FindListed {
                generation,
                seq,
                dir,
                result,
            } => self.accept_find_listed(generation, seq, dir, result),
```

- [ ] **Step 5: 五个 `FileAction` 的落地**

在 `apply_remote_file_action` 里,**跟 `Ask`/`CopyPath` 一组**(在借出 `files_panel_mut()` 之前分流的那一段)加:

```rust
            // F278:搜索的五个动作都只改这个标签自己的面板状态,不需要
            // `&mut self` 以外的东西;`FindStart` 要发请求,但发的那一步交给
            // 下一帧的 `pump_find` —— 这里只把状态机摆好。
            FileAction::FindToggle => {
                self.toggle_files_find(generation);
                return;
            }
            FileAction::FindStart(q) => {
                self.start_files_find(generation, q.clone());
                return;
            }
            FileAction::FindCancel => {
                self.cancel_files_find(generation);
                return;
            }
            FileAction::FindClose => {
                self.close_files_find(generation);
                return;
            }
            FileAction::FindPick { .. } => {
                // Task 7 接。这里先什么都不做会**静默**——老实记一条。
                log::warn!("F278:FindPick 还没接线");
                return;
            }
```

四个方法:

```rust
    /// F278:开/关搜索条。
    fn toggle_files_find(&mut self, generation: u64) {
        let Some(files) = self.files_panel_mut(generation) else {
            return;
        };
        if files.find.is_some() {
            // 关掉时也要**作废在途结果**:不作废的话,半秒后回来的那批会
            // 往一个已经没有 `find` 的面板里塞(`accept` 会早退,无害),
            // 但用户紧接着又点开搜索条的话,`seq` 还是老的 —— 那批旧结果
            // 会被当成新这次的收下。
            files.find_seq += 1;
            files.find = None;
        } else {
            files.find = Some(crate::ui::files_panel::Find {
                focus_pending: true,
                ..Default::default()
            });
        }
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F278:起一次搜索。根是**远端栏此刻的当前目录**。
    fn start_files_find(&mut self, generation: u64, query: String) {
        let Some(files) = self.files_panel_mut(generation) else {
            return;
        };
        let root = files.remote.cwd.clone();
        let show_hidden = files.remote.show_hidden;
        files.find_seq += 1;
        let seq = files.find_seq;
        let Some(f) = files.find.as_mut() else { return };
        f.walk = Some(crate::files::find::Walk::new(root, query, show_hidden));
        f.seq = seq;
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F278:用户按了取消 —— 停在原地,**已经找到的结果留着**。
    ///
    /// 留着而不是清空:用户按取消多半是因为「要的那条已经出来了」,
    /// 清掉等于把他刚等来的东西没收。
    fn cancel_files_find(&mut self, generation: u64) {
        let Some(files) = self.files_panel_mut(generation) else {
            return;
        };
        // 序号先递增:在途那几条回来时 `accept_find_listed` 会整条丢掉,
        // 不会让 `visited` 在「已取消」之后还继续往上跳。
        files.find_seq += 1;
        if let Some(w) = files.find.as_mut().and_then(|f| f.walk.as_mut()) {
            w.cancel();
        }
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F278:关掉搜索条,回普通列表。
    fn close_files_find(&mut self, generation: u64) {
        let Some(files) = self.files_panel_mut(generation) else {
            return;
        };
        files.find_seq += 1;
        files.find = None;
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F278:某个标签的文件面板。四个搜索方法共用 —— 各写一遍
    /// `by_generation_mut(..).and_then(..)` 的话,漏掉其中一处的 `None` 早退
    /// 会变成一次 panic。
    fn files_panel_mut(
        &mut self,
        generation: u64,
    ) -> Option<&mut crate::ui::files_panel::PanelFrame> {
        self.tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
    }
```

> `files_panel_mut` 如果 `app.rs` 里已经有同名的,直接用既有的,别再加一个。

- [ ] **Step 6: 测试**

```rust
    /// F278:取消一次搜索**先递增序号再标取消**。反过来的话,在途那几条
    /// 回来时序号还对得上,`visited` 会在「已取消」之后继续往上跳 ——
    /// 界面上就是「已取消,当时已找到 3 个」后面那个目录数还在动。
    ///
    /// 自证会变红:把 `cancel_files_find` 里的 `files.find_seq += 1;` 删掉。
    #[test]
    fn canceling_a_search_invalidates_the_requests_already_out() {
        let body = strip_comments(body_of(prod_src(), "fn cancel_files_find("));
        let bump = body.find("find_seq += 1").expect("取消时必须递增序号");
        let cancel = body.find("w.cancel()").expect("取消时必须标 Walk");
        assert!(
            bump < cancel,
            "递增序号必须排在标取消之前,否则在途结果照样收下:{body}"
        );
    }

    /// F278:搜索结果**绝不 abort** `sftp_tasks` —— 那是个混合池,无差别
    /// abort 会腰斩传输并把 `load` 永久卡在 `Loading`
    /// (见 `reopen_sftp_on_focused_host` 上方那段长注释)。
    ///
    /// 自证会变红:在 `cancel_files_find` 里加一句
    /// `if let Some(t) = self.tabs...sftp_tasks_mut() { for h in t.drain(..) { h.abort(); } }`。
    #[test]
    fn canceling_a_search_never_aborts_the_shared_task_pool() {
        for name in [
            "fn cancel_files_find(",
            "fn close_files_find(",
            "fn toggle_files_find(",
            "fn start_files_find(",
        ] {
            let body = strip_comments(body_of(prod_src(), name));
            assert!(
                !body.contains("abort()") && !body.contains("sftp_tasks"),
                "{name} 动了共享任务池 —— 会腰斩在跑的传输:{body}"
            );
        }
    }

    /// F278:`pump_find` 遍历全部标签。只推活动标签的话,搜索中切走再切回来
    /// 会发现它停在半路,而且完全静默。
    ///
    /// 自证会变红:把 `pump_find` 里的 `self.tabs.iter_mut()` 换成只取活动标签。
    #[test]
    fn the_find_pump_walks_every_tab_not_just_the_active_one() {
        let body = strip_comments(body_of(prod_src(), "fn pump_find("));
        assert!(
            body.contains("self.tabs.iter_mut()"),
            "没遍历全部标签:{body}"
        );
    }

    /// F278:`pump_find` 真的每帧被调 —— 不调的话状态机摆好了也永远发不出
    /// 第一条请求,界面卡在「正在搜索…已找到 0 个」。
    ///
    /// 自证会变红:把那一行调用删掉。
    #[test]
    fn the_find_pump_actually_runs_every_frame() {
        let src = strip_comments(prod_src());
        assert!(src.contains("self.pump_find();"), "pump_find 没有被调用");
    }
```

- [ ] **Step 7: 跑 + Commit**

```bash
cargo test --workspace 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 递归搜索的异步回路(事件/spawn/pump/accept) (F278)"
```

---

### Task 7: 结果列表

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`

- [ ] **Step 1: 在 `Load` 分支之后插入结果列表分支**

在 `files_panel.rs` 那个 `match &state.load { ... Load::Ready => {} }` **之后**、`// F136:先占住一条横带留给列头` 之前插入:

```rust
    // F278:搜索结果**取代**这一栏的文件列表。
    //
    // 取代而不是挤在下面:两个列表叠在一栏里,用户分不清「选中的这一条」
    // 是哪个列表里的,而选中集本来就只有一份(`PaneState::selected`)。
    // 关掉搜索条就回到原来那个目录,一个字节都没动过。
    if let Some(w) = find.as_ref().and_then(|f| f.walk.as_ref()) {
        let root = w.root().clone();
        let hits: Vec<_> = w.hits().to_vec();
        egui::ScrollArea::vertical()
            .id_salt(("files-find-results", id))
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, hits.len(), |ui, range| {
                for i in range {
                    let h = &hits[i];
                    let rel = crate::files::find::relative(&root, &h.path);
                    let resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(&rel).color(theme::c32(if h.is_dir {
                                t.accent
                            } else {
                                t.fg_mid
                            })),
                        )
                        .truncate()
                        .sense(egui::Sense::click()),
                    );
                    // 单击也算 —— 结果列表里没有「展开」这种二段动作,
                    // 双击才跳的话用户会以为点不动。双击同样接住:
                    // 习惯了文件列表的人会下意识双击。
                    if resp.clicked() || resp.double_clicked() {
                        action = Some(FileAction::FindPick {
                            path: h.path.clone(),
                            is_dir: h.is_dir,
                        });
                    }
                    resp.on_hover_text(h.path.display());
                }
            });
        annotate::mark(ui.ctx(), format!("文件面板/{id}/搜索结果"), ui.max_rect());
        return action;
    }
```

> `w.hits().to_vec()` 要求 `Hit: Clone` —— Task 2 已经 derive 了。
> 借用:`find` 在这里是 `Option<&mut Find>`,用 `as_ref()` 拿不可变借用,`state` 在这一段不参与,不会打架。

- [ ] **Step 2: Esc 关搜索条**

在 `handle_panel_key`(或面板的键盘处理处)里,**排在既有的 Backspace/方向键判据之前**:

```rust
    // F278:Esc 关搜索条。**必须排在其它键判据之前** —— 搜索条开着的时候
    // 整栏画的是结果列表,把 Esc 交给别的解释(比如清选中)等于这个框
    // 关不掉,而它已经遮住了文件列表。
    if find_open && key == egui::Key::Escape {
        return Some(FileAction::FindClose);
    }
```

> 实现者按 `handle_panel_key` 的实际签名接上 `find_open: bool` 参数;这一条同样要配一个守护(见 Step 3)。

- [ ] **Step 3: 测试**

```rust
    /// F278:结果行显示的是**相对路径**,不是绝对路径 —— 绝对路径每行都顶着
    /// 同一段前缀,把唯一有信息量的后半截挤出可视区。
    ///
    /// 自证会变红:把 `relative(&root, &h.path)` 换成 `h.path.display()`。
    #[test]
    fn the_results_list_shows_relative_paths() {
        let src = strip_comments(prod_src_panel());
        assert!(
            src.contains("crate::files::find::relative(&root, &h.path)"),
            "结果行没走 relative —— 会画成一整条绝对路径"
        );
    }

    /// F278:Esc 的判据**排在其它键之前**。排后面的话搜索条关不掉,
    /// 而它已经遮住了整个文件列表。
    ///
    /// 自证会变红:把那一段挪到 Backspace 判据之后。
    #[test]
    fn escape_closes_the_find_bar_before_any_other_key_is_interpreted() {
        let body = strip_comments(body_of(prod_src_panel(), "fn handle_panel_key("));
        let esc = body.find("FindClose").expect("Esc 没接关搜索条");
        let back = body.find("Backspace").expect("找不到 Backspace 的判据");
        assert!(esc < back, "Esc 排在别的键后面了,搜索条关不掉:{body}");
    }
```

> `prod_src_panel()` 是这个文件里的源码切片辅助函数;没有的话照 `app.rs` 的 `prod_src` 现写一个(`include_str!("files_panel.rs")` + 切掉 `#[cfg(test)]` 之后的部分)。**记得剥注释**(「源码切片守护不剥注释」那条既有欠账)。

- [ ] **Step 4: 跑 + Commit**

```bash
cargo test --workspace 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 递归搜索结果列表 + Esc 关闭 (F278)"
```

---

### Task 8: `FindPick` 落地 —— 复用 F218 reveal

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

**这是 spec 明写的复用点。** 落地那三步必须与 `accept_reveal_stat`(划选跳转)和 `accept_path_probe`(路径条)共用同一套 `reveal_destination` + `Goto` + `set_reveal_pick` —— 三处不同源的话,同一条路径从三个入口过去会落在不同的地方,而这种错只有人按下去才知道。

- [ ] **Step 1: 换掉 Task 6 留的那句 `log::warn!`**

```rust
            FileAction::FindPick { path, is_dir } => {
                self.pick_find_hit(generation, path.clone(), *is_dir);
                return;
            }
```

```rust
    /// F278:点中了一条搜索结果 —— 跳到它所在的目录并把它亮出来。
    ///
    /// **落地三步与 `accept_reveal_stat` / `accept_path_probe` 共用**
    /// (`reveal_destination` + `Goto` + `set_reveal_pick`):三处不同源的话,
    /// 同一条路径从「划选跳过去」「路径条敲过去」「搜索点过去」会落在三个
    /// 不同的地方,而这种错只有人按下去才知道。
    ///
    /// **不发 `stat`**,这是与那两处唯一的不同:`is_dir` 是遍历时
    /// `list_dir` 一起带回来的,已经知道了。再问一遍等于在高延迟链路上白加
    /// 一次往返,而这个功能的全部价值就是省往返。
    ///
    /// **命中就在当前目录时天然退化成定位模式**(spec F278 最后一句):
    /// `reveal_destination` 对文件给的是「父目录 + 末段」,父目录正好等于
    /// 当前目录 —— `Goto` 过去不换目录,`set_reveal_pick` 把那一条选中。
    ///
    /// 跳完**关掉搜索条**:用户点结果的意思是「就是它,带我过去」,留着
    /// 一个盖住文件列表的结果列表等于没带他过去。
    fn pick_find_hit(
        &mut self,
        generation: u64,
        path: mullion_ssh::sftp::RemotePath,
        is_dir: bool,
    ) {
        let target = RevealTarget {
            generation,
            column: crate::files::PanelColumn::Remote,
            // 搜索就长在这个标签这条 channel 上,不存在「发起时在另一台」
            // 的情形(那是 F218 划选跳转独有的)。同 `accept_path_probe`。
            host_ix: None,
            path,
            arrived: false,
        };
        let (goto, pick) = self.reveal_destination(&target, is_dir);
        // **先关搜索条再派发 `Goto`**:`Goto` 会走 `begin_load` 把这一栏置成
        // 加载中,而结果列表是画在 `Load` 分支**之后**的 —— 顺序反了的话,
        // 那一帧仍然画结果列表,用户看不到自己已经跳过去了。
        self.close_files_find(generation);
        self.apply_remote_file_action(generation, crate::ui::files_panel::FileAction::Goto(goto));
        self.set_reveal_pick(&target, pick);
        self.request_ui_redraw();
    }
```

- [ ] **Step 2: 测试**

```rust
    /// F278:落地三步与另外两个入口同源。各写一份的话,同一条路径从三个
    /// 入口过去会落在三个不同的地方,而这种错只有人按下去才知道。
    ///
    /// 自证会变红:把 `pick_find_hit` 里的 `reveal_destination` 换成手写的
    /// 「取父目录 + 取末段」。
    #[test]
    fn a_search_hit_lands_through_the_same_three_steps_as_the_other_two_entries() {
        let body = strip_comments(body_of(prod_src(), "fn pick_find_hit("));
        for step in ["reveal_destination", "FileAction::Goto", "set_reveal_pick"] {
            assert!(body.contains(step), "少了 {step} 这一步:{body}");
        }
    }

    /// F278:`set_reveal_pick` **必须排在 `Goto` 之后**。
    ///
    /// `PaneState::begin_load` 会 `clear_selection`,写在它前面的话这一条会被
    /// 自己清掉 —— 症状是「跳过去了但什么都没选中」,看着像没生效,查起来
    /// 却查不到任何错误(`set_reveal_pick` 的文档已经为另外两个入口写过同
    /// 一句话)。
    ///
    /// 自证会变红:把那两行对调。
    #[test]
    fn the_pick_is_written_after_the_goto_or_it_clears_itself() {
        let body = strip_comments(body_of(prod_src(), "fn pick_find_hit("));
        let goto = body.find("FileAction::Goto").expect("没派发 Goto");
        let pick = body.find("set_reveal_pick").expect("没写 reveal_pick");
        assert!(goto < pick, "set_reveal_pick 排在 Goto 前面了:{body}");
    }

    /// F278:**不再问一次 `stat`** —— `is_dir` 遍历时已经带回来了,再问等于
    /// 在高延迟链路上白加一次往返,而这个功能的全部价值就是省往返。
    ///
    /// 自证会变红:在 `pick_find_hit` 里加一句 `spawn_sftp_stat(..)`。
    #[test]
    fn picking_a_hit_costs_no_extra_round_trip() {
        let body = strip_comments(body_of(prod_src(), "fn pick_find_hit("));
        assert!(
            !body.contains("spawn_sftp_stat") && !body.contains("spawn_sftp_path_probe"),
            "又问了一遍 stat:{body}"
        );
    }

    /// F278:跳之前先关搜索条。顺序反了的话,`Goto` 把这一栏置成加载中,
    /// 而结果列表画在 `Load` 分支**之后** —— 那一帧仍然画结果列表,用户看
    /// 不到自己已经跳过去了。
    ///
    /// 自证会变红:把 `close_files_find` 挪到 `apply_remote_file_action` 之后。
    #[test]
    fn the_find_bar_closes_before_the_jump_so_the_user_sees_where_he_landed() {
        let body = strip_comments(body_of(prod_src(), "fn pick_find_hit("));
        let close = body.find("close_files_find").expect("没关搜索条");
        let goto = body.find("apply_remote_file_action").expect("没派发 Goto");
        assert!(close < goto, "关搜索条排在跳转之后了:{body}");
    }
```

- [ ] **Step 3: 跑 + Commit**

```bash
cargo test --workspace 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 搜索结果跳转复用 F218 reveal (F278)"
```

---

### Task 9: 作废与收口 + spec 登记

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/ui/files_panel.rs`
- Modify: `spec.md`

**为什么单独一个任务:** 前八个任务把功能做出来了,但「搜索跑着的时候用户干别的」这一族路径全没接。这正是本项目反复踩的那类坑(F132/F128/F160:判据放错了层、意图表换节点没人清)。

- [ ] **Step 1: 换机器时作废**

`reopen_sftp_on_focused_host` 里,`files.remote.invalidate();` 那一段之后加:

```rust
                // F278:换机器 = 搜索的根没了。`invalidate()` 只够得着
                // `PaneState`,`find` 挂在更外层的 `PanelFrame` 上,结构上
                // 天然摸不到 —— 不清的话,新机器上回来的目录会被塞进旧机器
                // 那次遍历里,结果列表变成两台机器的文件混在一起,而相对
                // 路径看上去完全正常。(F220 的 `clip` 在这里踩过同一个坑,
                // 就在上面几行。)
                files.find_seq += 1;
                files.find = None;
```

- [ ] **Step 2: 换目录时作废**

搜索的根是「起搜那一刻的当前目录」。用户在搜索中通过别的入口换了目录(书签、路径条、`Goto`),那次搜索的根就与面板显示的目录对不上了。

在 `apply_remote_file_action` 里 `FileAction::Goto`/`GotoInput`/`Up` 落到 `begin_load` 那条路之前加:

```rust
        // F278:换目录 = 搜索的根变了。**只对真正换目录的那几个动作** ——
        // `Refresh` 不算(根没变),`FindPick` 自己已经关过搜索条了。
        //
        // 不清的话:结果列表还挂着旧根,而 `relative()` 拿旧根去切新路径,
        // 切不出来就原样吐绝对路径 —— 列表突然一半相对一半绝对,而没有
        // 任何报错。
        if matches!(
            action,
            FileAction::Goto(_) | FileAction::GotoInput(_) | FileAction::Up
        ) {
            if let Some(files) = self.files_panel_mut(generation) {
                if files.find.is_some() {
                    files.find_seq += 1;
                    files.find = None;
                }
            }
        }
```

> **注意**:`pick_find_hit` 自己先调了 `close_files_find` 再派发 `Goto`,走到这里 `find` 已经是 `None`,不会二次伤害。

- [ ] **Step 3: 关标签时不需要额外做什么(但要证明)**

`wind_down` 关标签时对 `sftp_tasks` 里的在途 task 直接 `abort()`,`FindListed` 事件永远不会抵达 —— 而 `find` 状态随 `PanelFrame` 一起被丢掉,不存在「队列越堆越高」的问题(T11 那条兜底纪律针对的是**挂在 App 级队列上**的东西;`find` 是每标签一份,标签没了它也没了)。加一条测试把这个推理钉住:

```rust
    /// F278:搜索状态**挂在标签上**,标签没了它也没了 —— 不需要 T11 那种
    /// 「按世代号兜底回收」。
    ///
    /// 这条钉的是结构事实:`find` 是 `PanelFrame` 的字段,而 `PanelFrame`
    /// 是 `TabContent` 的一部分。哪天有人把它挪去 `App` 上(比如为了「全局
    /// 搜索历史」),这条会变红,提醒他同时补上回收。
    ///
    /// 自证会变红:把 `find` 字段从 `PanelFrame` 搬到 `App` 上。
    #[test]
    fn the_find_state_lives_on_the_tab_so_closing_the_tab_reclaims_it() {
        let panel = strip_comments(prod_src_panel());
        let frame = body_of(&panel, "pub struct PanelFrame {");
        assert!(
            frame.contains("pub find: Option<Find>"),
            "find 不在 PanelFrame 上了 —— 挪到别处的话要自己补回收:{frame}"
        );
        assert!(
            !strip_comments(prod_src()).contains("    find: Option<crate::ui::files_panel::Find>"),
            "App 上多了一份 find —— 关标签回收不到它"
        );
    }
```

- [ ] **Step 4: 换目录/换机器的守护**

```rust
    /// F278:换机器要清掉搜索。不清的话新机器回来的目录会塞进旧机器那次
    /// 遍历里 —— 结果列表变成两台机器的文件混在一起,而相对路径看上去
    /// 完全正常。(F220 的 `clip` 在同一个函数里踩过同一个坑。)
    ///
    /// 自证会变红:把 `reopen_sftp_on_focused_host` 里那两行删掉。
    #[test]
    fn switching_hosts_throws_away_the_running_search() {
        let body = strip_comments(body_of(prod_src(), "fn reopen_sftp_on_focused_host("));
        assert!(
            body.contains("files.find = None"),
            "换机器没清搜索:{body}"
        );
        assert!(
            body.contains("files.find_seq += 1"),
            "换机器清了搜索但没递增序号 —— 在途结果会被下一次搜索收下:{body}"
        );
    }

    /// F278:换目录要清掉搜索,但 `Refresh` **不算**换目录(根没变)。
    ///
    /// 一起清掉的话,搜索中按一下 F5 结果就没了,而用户只是想刷新。
    ///
    /// 自证会变红:把那个 `matches!` 里加上 `| FileAction::Refresh`。
    #[test]
    fn changing_directory_drops_the_search_but_refreshing_does_not() {
        let body = strip_comments(body_of(prod_src(), "fn apply_remote_file_action("));
        let guard = body
            .split("FileAction::Goto(_) | FileAction::GotoInput(_) | FileAction::Up")
            .nth(1)
            .expect("找不到换目录清搜索那道门");
        let head = &guard[..guard.len().min(200)];
        assert!(
            !head.contains("Refresh"),
            "Refresh 也被当成换目录了 —— 搜索中按 F5 结果就没了:{head}"
        );
    }
```

- [ ] **Step 5: 登记 spec.md**

把 `spec.md:360` 那一行的 `**片三,未实现。**` 换成 `**已实现(v0.1.114)。**`,并在后面补落点:

```
落点:`files/find.rs`(匹配 + BFS 状态机,纯函数)、`ui/icon.rs::Glyph::Search`、
`ui/files_panel.rs`(放大镜 + 搜索条 + 结果列表 + `Find` 状态)、
`app.rs`(`Modal::FilesFind` / `UserEvent::FindListed` / `pump_find` /
`pick_find_hit`)。**只做远端栏** —— 本地栏递归要另配 `spawn_blocking` 通路
(本地列目录目前同步跑在事件循环线程上),不在本切片。
守护:`files::find::tests` 十六条;`app::tests::canceling_a_search_never_aborts_the_shared_task_pool`
(混合任务池纪律)、`the_pick_is_written_after_the_goto_or_it_clears_itself`(F218 同款顺序)。
```

原有的「否掉远端 `find`」「否掉边输边搜」那两句**原样保留** —— 那是当时的权衡,结论之外理由更值钱。

- [ ] **Step 6: 全量跑绿 + Commit**

```bash
cargo test --workspace 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
git add crates/mullion-app/src/app.rs crates/mullion-app/src/ui/files_panel.rs spec.md
git commit -m "feat(app): 搜索的作废与收口(换目录/换机器)+ 登记 F278"
```

---

## 交付(全部任务完成之后)

按 CLAUDE.md 的「交付约定」一条龙走完,**不要停下来问**:升 patch 到 `0.1.114` → 跑绿 → 交叉编译 + objdump → 签名 → 发 Release → 报链接和人工验收清单。完整步骤见 `.claude/skills/release-windows/SKILL.md`。

**人工验收清单**(无头容器里验不了的那些,写进 notes):

1. 远端栏路径条上出现放大镜,**本地栏没有**(有意为之)。点一下展开搜索条,光标自动落在输入框里。
2. 在一个有几层子目录的目录下搜一个存在的文件名(比如 `apprs` 搜 `app.rs`):结果列表出相对路径,能看见它在哪一层。
3. **点一条结果** → 面板跳到它所在的目录,那一条是选中态且滚到了可见处,搜索条自动关掉。
4. **命中就在当前目录**时:不换目录,只把那一条亮出来。
5. 搜一个大目录(家目录)→ 中途点「取消」→ 进度停住,**已找到的结果留着**,并且文字写的是「已取消,当时已找到 N 个」而不是「没有找到」。
6. 搜索中按 **Esc** → 搜索条关掉,回到原来那个目录,文件列表一个字节没变。
7. 搜索中**换机器 / 点书签换目录** → 结果列表消失,不会出现两台机器的文件混在一起。
8. 搜索中按 **F5 刷新** → 结果**不该**消失(根没变)。
9. 搜索期间**同时开一个上传/下载**:传输不被打断(混合任务池纪律),传完了搜索也还在跑。
10. 放大镜图标在实机上画出来不是豆腐块(T9;自绘理论上不会,但这一条只有人眼能判)。
11. 隐藏文件开关关着时,搜索**不进** `.git`/`.cache`;开着时进得去、隐藏文件本身也搜得到。
12. 常规:不闪 / CJK 对齐 / 输入法 / 手感。

---

## 计划自查

**спec 覆盖**:路径条尾放大镜(Task 5)/ 下方搜索条(Task 5)/ Enter 起搜(Task 5)/ SFTP 逐目录遍历(Task 2+6)/ 大小写不敏感子序列(Task 1)/ 封顶 500+2000(Task 2)/ 可取消(Task 2+6)/ 不跟符号链接(Task 2)/ 隐藏目录随 `show_hidden`(Task 2)/ 结果列表出相对路径(Task 1+7)/ Enter 双击跳转并置选中复用 F218 reveal(Task 8)/ 命中在当前目录退化成定位模式(Task 8)—— 十二条全有落点。

**已知留给实现者的判断点**(不是占位符,是必须按实际代码对齐的名字):`self.tabs.iter_mut()` / `tab.generation` / `handle_panel_key` 的签名 / `prod_src_panel` 辅助函数是否已存在 / `files_panel_mut` 是否已存在。每一处计划里都写了「按既有写法对齐」和参照的既有例子。

**类型一致性**:`Find`(Task 4 定义)→ `find: Option<&mut Find>`(Task 5 参数)→ `f.walk: Option<Walk>`(Task 2 定义)→ `Walk::hits() -> &[Hit]`、`Hit { path, is_dir }`(Task 2)→ `FileAction::FindPick { path, is_dir }`(Task 5)→ `pick_find_hit(generation, path, is_dir)`(Task 8)。`find_progress_text(&Walk)`(Task 5)读 `Walk::status/hits/visited/skipped`(Task 2 全部 `pub`)。一致。
