//! 布局持久化的**磁盘格式**(F37,设计 E1/E2/E5/E6)。零 UI、零 async。
//!
//! 独立文件 `<config_dir>/layout.toml`,与 `sessions.toml` **各走各的
//! `schema_version`**:会话库是用户的资产(丢了要重配),布局只是上次的运行
//! 现场(丢了只是回到空标签栏)。混在一个文件里会把用户凭据的写入频率从
//! 「点保存时」抬到「每帧」——`Vault::save` 是连加密侧车一起写两个文件的。
//!
//! **本模块不认识 `mullion-core`**(设计 E4,架构不变量)。落盘的树是这里自己
//! 的扁平编码,`mullion_core::layout::Node` 与它的互转住在 app 的
//! `shell/layout_snapshot.rs`。往 `Cargo.toml` 里加一行 `mullion-core` 就能
//! 省掉一整个转换层,代价是布局树的两个真源合并 —— 此后 core 每改一次
//! `Node` 就同时改了磁盘格式,而 core 的作者不知道自己在改文件格式。
//! 由 `the_store_crate_must_not_depend_on_any_other_mullion_crate` 守。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::model::SessionId;

/// 本版本写出的布局格式版本。
pub const CURRENT_LAYOUT_SCHEMA: u32 = 1;

/// 文件名。
pub const LAYOUT_FILE: &str = "layout.toml";

/// 分屏方向。**不复用 `mullion_core::layout::Dir`**,理由见模块文档(E4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedDir {
    /// 左右并排。语义与 `core::layout::Dir::Horizontal` 对齐,互转在 app 侧。
    Horizontal,
    /// 上下堆叠。
    Vertical,
}

/// 标签装的是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedTabKind {
    /// 终端工作区(有分屏树)。
    Terminal,
    /// SFTP 节点标签(D1/F50)。没有分屏概念,`tree` 恒为单叶子。
    Files,
}

/// 树的**前序遍历**里的一项。
///
/// 为什么是扁平数组而不是嵌套结构:TOML 没有 enum,serde 的内部标签
/// (`#[serde(tag = ...)]`)走 `Content` 缓冲、跟 toml 的「值必须在表之前」
/// 规则相性很差,而递归的 `Box<Node>` 编出来是一串 `[tab.tree.a.b.a]` 这种
/// 人读不了也手改不了的深表。
///
/// 扁平编码还白得一个性质:**「树损坏」变成可判定的解码失败**。截断的数组
/// 拼不出完整的树,`from_entries` 直接返回 `None`,不需要另外定义「什么样的
/// 树算坏」。
///
/// 编码规则:`dir`/`ratio` 同时有值 = 二分节点,它的两棵子树紧跟在后面
/// (先 a 后 b);两者都没有 = 叶子。
///
/// F160:叶子还带**身份**(`session_id`/`tmux`)。分割节点上这两个恒为 `None`。
/// **不派生 `Copy`** —— `tmux` 是 `String`。这不是疏忽:身份必须跟叶子走,
/// 摆在别处(比如 `SavedTab` 上一个平行数组)就会出现「树改了、身份没跟着改」
/// 的错位,而错位的现象是某块 pane 接回了另一块的 tmux 会话。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SavedNodeEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<SavedDir>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    /// F160:这个叶子当初连的是哪条会话记录。`None` = 旧版 exe 写的(读回时
    /// 回落到 `SavedTab::session_id`,见 `layout_snapshot::leaf_identities`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// F160/D1:关 exe 那一刻这块 pane **实际所在**的 tmux 会话名
    /// (F123/F124 由远端标题上报,不是会话配置里写的那个)。
    /// `None` = 当初不在 tmux 里 / 远端没开 `set-titles`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
}

impl SavedNodeEntry {
    /// 一个没有身份的叶子。
    pub fn leaf() -> Self {
        Self::default()
    }

    /// F160:一个带身份的叶子。
    pub fn leaf_with(session_id: Option<SessionId>, tmux: Option<String>) -> Self {
        Self {
            dir: None,
            ratio: None,
            session_id,
            tmux,
        }
    }

    /// 一个二分节点。
    pub fn split(dir: SavedDir, ratio: f32) -> Self {
        Self {
            dir: Some(dir),
            ratio: Some(ratio),
            session_id: None,
            tmux: None,
        }
    }

    /// 这一项是不是叶子。**两个字段必须一起判**:只判 `dir` 的话,一个
    /// 手改出来的 `{ dir = "horizontal" }`(漏了 ratio)会被当成分割节点,
    /// 然后拿 `unwrap_or` 补一个默认比例 —— 那是在猜用户的布局。
    ///
    /// F160:**身份字段不参与这条判据**。参与的话,一个记了 tmux 名的叶子
    /// 会被当成分割节点,整棵树判坏,那个标签被 `usable` 整个丢掉。
    /// 守护:`a_leaf_that_carries_an_identity_is_still_a_leaf`。
    pub fn is_leaf(&self) -> bool {
        self.dir.is_none() && self.ratio.is_none()
    }
}

/// 窗口几何。尺寸是**逻辑点**(与 winit 的 `LogicalSize` 同单位),
/// 位置是 outer position 的逻辑点。
///
/// 位置 `Option`:首次运行没有位置可言,恢复时若与所有显示器都无交集也会被
/// 丢掉(E8,判据在 app 侧的 `clamp_to_monitors`)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub width: f32,
    pub height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default)]
    pub maximized: bool,
}

/// 一个标签的落盘形态。
///
/// **只有能重建的标签才在这里**:快速连接(F47)没有 `SessionId`,重启后无从
/// 拨号,快照时就不入盘(E6,判据在 app 侧)。所以这里的 `session_id` 不是
/// `Option`——留 `Option` 等于允许写进来一条永远恢复不出来的记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub kind: SavedTabKind,
    pub session_id: SessionId,
    /// 标签栏上的名字。恢复时先用它,连上之后由 `HostConn` 覆盖。
    ///
    /// 存一份而不是每次从会话库查:会话可能被改了名,而标签栏在**点重连之前**
    /// 就得画出来。查不到会话的标签整个会被丢弃(E6),那是另一回事。
    pub title: String,
    /// 焦点落在**前序遍历第几个叶子**(0-based)。
    ///
    /// 不存 `PaneId`:它是进程内计数器发的,跨重启没有意义(E5)。越界时由
    /// app 侧夹到 0。
    #[serde(default)]
    pub focus_leaf: usize,
    /// 树的前序遍历。**必须是最后一个字段** —— toml 的「值在表之前」规则:
    /// 表数组排在标量字段前面会直接序列化失败。
    #[serde(default)]
    pub tree: Vec<SavedNodeEntry>,
}

/// 整份 `layout.toml`。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SavedLayout {
    #[serde(default)]
    pub schema_version: u32,
    /// 活动标签下标。越界由 app 侧夹到 0(E6)。
    #[serde(default)]
    pub active_tab: usize,
    /// F148:这份记录最后一次写盘的时刻(Unix 秒,UTC)。`0` = 不知道
    /// (v1 老文件迁移过来的、或手改出来的)。
    ///
    /// **必须排在 `window` 之前、`tabs` 之前**:toml 的「值在表之前」规则 ——
    /// 标量字段排在 `[window]` 表和 `[[tab]]` 表数组后面会直接序列化失败。
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<SavedWindow>,
    /// **必须排在 `window` 之后**,理由同 `SavedTab::tree`。
    #[serde(default, rename = "tab")]
    pub tabs: Vec<SavedTab>,
}

impl SavedLayout {
    /// 一份「什么都没恢复」的布局。文件不存在 / 解析失败 / 版本不认识时用它。
    pub fn empty() -> Self {
        Self {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            ..Default::default()
        }
    }
}

/// [`load`] 的结果。
///
/// **没有 `Result`,这是刻意的**(E6):布局文件不是用户资产,它坏了的正确
/// 表现是「这次没恢复」,不是「打不开客户端」。返回 `Result` 的话,调用方
/// 一个 `?` 就能把「布局文件被手改坏了」升级成「启动失败」,而那个 `?`
/// 在 review 里看起来完全无害。类型签名不给这个机会。
pub struct Loaded {
    pub layout: SavedLayout,
    /// 降级原因,给日志用。`None` = 正常读到了(含「文件不存在」这种正常情况)。
    pub note: Option<String>,
}

/// 读 `<dir>/layout.toml`。**永不失败**,理由见 [`Loaded`]。
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(LAYOUT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                layout: SavedLayout::empty(),
                note: None,
            }
        }
        Err(e) => {
            return Loaded {
                layout: SavedLayout::empty(),
                note: Some(format!("读不出来({e}),这次不恢复布局")),
            }
        }
    };
    let layout: SavedLayout = match toml::from_str(&text) {
        Ok(l) => l,
        Err(e) => {
            return Loaded {
                layout: SavedLayout::empty(),
                note: Some(format!("解析失败({e}),这次不恢复布局")),
            }
        }
    };
    // 更新版本写出来的文件:**不猜**。字段可能改了含义,按现版本读出来的
    // 布局会是错的,而错的布局比没有布局更难排查。
    if layout.schema_version > CURRENT_LAYOUT_SCHEMA {
        return Loaded {
            layout: SavedLayout::empty(),
            note: Some(format!(
                "layout.toml 是 v{} 写的(本版本认到 v{CURRENT_LAYOUT_SCHEMA}),这次不恢复布局",
                layout.schema_version
            )),
        };
    }
    Loaded { layout, note: None }
}

/// 原子写 `<dir>/layout.toml`。
pub fn save(dir: &Path, layout: &SavedLayout) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir)?;
    let mut out = layout.clone();
    out.schema_version = CURRENT_LAYOUT_SCHEMA;
    let text = toml::to_string_pretty(&out)?;
    crate::vault::write_atomic(&dir.join(LAYOUT_FILE), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SavedLayout {
        SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 1,
            updated_at: 1_755_000_000,
            window: Some(SavedWindow {
                width: 1600.5,
                height: 900.25,
                x: Some(-120.0),
                y: Some(80.0),
                maximized: false,
            }),
            tabs: vec![
                SavedTab {
                    kind: SavedTabKind::Terminal,
                    session_id: SessionId(7),
                    title: "prod-web-01".into(),
                    focus_leaf: 2,
                    // H(L, V(L, L)) —— 前序:split, leaf, split, leaf, leaf
                    tree: vec![
                        SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
                        SavedNodeEntry::leaf(),
                        SavedNodeEntry::split(SavedDir::Vertical, 0.375),
                        SavedNodeEntry::leaf(),
                        SavedNodeEntry::leaf(),
                    ],
                },
                SavedTab {
                    kind: SavedTabKind::Files,
                    session_id: SessionId(12),
                    title: "nas".into(),
                    focus_leaf: 0,
                    tree: vec![SavedNodeEntry::leaf()],
                },
            ],
        }
    }

    /// **spec F37 点名的验收标准**:序列化/反序列化 round-trip。
    ///
    /// 逐字段相等,不是「大致一样」—— `ratio` 差一点点,恢复出来的分屏比例
    /// 就跟用户拖成的不一样,而那是他花了时间调的。
    #[test]
    fn a_layout_survives_a_round_trip_through_toml() {
        let before = sample();
        let text = toml::to_string_pretty(&before).expect("序列化不该失败");
        let after: SavedLayout = toml::from_str(&text).expect("解析不该失败");
        assert_eq!(after, before, "round-trip 后不一致:\n{text}");
    }

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

    /// 老文件(没有这个字段)读进来是 0,不失败 —— 后续 Task 的迁移要读的正是
    /// 这种文件。
    #[test]
    fn a_file_without_an_updated_at_reads_back_as_zero() {
        let text = "schema_version = 1\n";
        let got: SavedLayout = toml::from_str(text).expect("缺字段不该解析失败");
        assert_eq!(got.updated_at, 0);
    }

    /// `f32` 的 ratio 必须精确回来。TOML 的浮点是 f64 文本,写出去再读回来
    /// 若走了一次 `f32 → f64 → f32` 的不当舍入,拖过的分隔条会每次重启都
    /// 偏一点点 —— 十次之后用户就发现布局「自己在漂」。
    #[test]
    fn split_ratios_come_back_bit_for_bit() {
        for r in [0.1f32, 0.333_333_34, 0.5, 0.666_666_7, 0.987_654_3] {
            let one = SavedLayout {
                schema_version: CURRENT_LAYOUT_SCHEMA,
                active_tab: 0,
                updated_at: 0,
                window: None,
                tabs: vec![SavedTab {
                    kind: SavedTabKind::Terminal,
                    session_id: SessionId(1),
                    title: "t".into(),
                    focus_leaf: 0,
                    tree: vec![
                        SavedNodeEntry::split(SavedDir::Horizontal, r),
                        SavedNodeEntry::leaf(),
                        SavedNodeEntry::leaf(),
                    ],
                }],
            };
            let text = toml::to_string_pretty(&one).unwrap();
            let back: SavedLayout = toml::from_str(&text).unwrap();
            assert_eq!(
                back.tabs[0].tree[0].ratio,
                Some(r),
                "ratio {r} 漂了:\n{text}"
            );
        }
    }

    /// 叶子编成空表、分割编成带两个键的表。这条钉的是**文件格式本身**——
    /// 它是要被人手改、被下一版读的东西,不能因为内部重构悄悄变形。
    #[test]
    fn a_leaf_is_encoded_as_an_empty_entry_and_a_split_carries_both_keys() {
        assert!(SavedNodeEntry::leaf().is_leaf());
        let s = SavedNodeEntry::split(SavedDir::Vertical, 0.5);
        assert!(!s.is_leaf());
        assert_eq!(s.dir, Some(SavedDir::Vertical));
        assert_eq!(s.ratio, Some(0.5));
    }

    /// 手改出来的半拉分割节点(有 dir 没 ratio)**不算叶子**——它是坏数据,
    /// 该在解码时被判失败,不该被当成叶子悄悄吞掉。
    #[test]
    fn a_half_written_split_is_not_mistaken_for_a_leaf() {
        let half = SavedNodeEntry {
            dir: Some(SavedDir::Horizontal),
            ratio: None,
            ..Default::default()
        };
        assert!(!half.is_leaf());
        let other = SavedNodeEntry {
            dir: None,
            ratio: Some(0.5),
            ..Default::default()
        };
        assert!(!other.is_leaf());
    }

    /// F160:叶子带上身份字段之后**仍然是叶子**。
    ///
    /// `is_leaf` 只看 `dir`/`ratio` —— 把身份字段也算进去的话,一个记了
    /// tmux 名的叶子会被解码器当成分割节点,整棵树判坏,那个标签直接被
    /// `usable` 丢掉(现象:恢复列表里的标签莫名其妙少了几个)。
    ///
    /// 自证会变红:把 `is_leaf` 改成 `self.dir.is_none() && self.ratio.is_none()
    /// && self.session_id.is_none()`。
    #[test]
    fn a_leaf_that_carries_an_identity_is_still_a_leaf() {
        let l = SavedNodeEntry::leaf_with(Some(SessionId(7)), Some("web01".into()));
        assert!(l.is_leaf(), "带身份的叶子被当成了分割节点");
        assert_eq!(l.session_id, Some(SessionId(7)));
        assert_eq!(l.tmux.as_deref(), Some("web01"));
        assert_eq!(l.dir, None);
        assert_eq!(l.ratio, None);
    }

    /// F160:没有身份的叶子编出来必须还是**空表**,与今天的文件逐字节一致。
    ///
    /// 这条钉的是**降级兼容的另一半**(D9 不升 schema 的前提):新版 exe 写出来
    /// 的、没有身份的那些叶子,旧版 exe 读到的东西跟它自己写的一模一样。
    ///
    /// 自证会变红:让 `leaf()` 带上身份(例如 `session_id: Some(SessionId(9))`)——
    /// 叶子那层多写一个 `session_id`,`count()` 变成 2。
    ///
    /// **别把「去掉 `skip_serializing_if`」当成自证路径**:toml 的序列化器本来就
    /// 无条件跳过 `None`,去掉那个属性这条测试照样绿。属性留着是为了别的 serde
    /// 格式(JSON 会写出 `null`)和表明意图,不是这条测试守的东西。
    #[test]
    fn a_leaf_without_an_identity_is_still_encoded_as_an_empty_entry() {
        let one = SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at: 0,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(1),
                title: "t".into(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf()],
            }],
        };
        let text = toml::to_string_pretty(&one).unwrap();
        assert!(
            !text.contains("session_id = ") || text.matches("session_id").count() == 1,
            "空叶子不该写出 session_id(只有 [[tab]] 那一处该有):\n{text}"
        );
        assert!(!text.contains("tmux"), "空叶子不该写出 tmux:\n{text}");
    }

    /// F160:身份字段经过真实文件一个来回不变形。
    ///
    /// 会话名里**故意带空格和单引号** —— TOML 的字符串转义与后面 F161 的
    /// shell 转义是两套东西,这里钉的是前者。
    #[test]
    fn leaf_identities_round_trip_through_the_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let before = SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at: 0,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(3),
                title: "prod".into(),
                focus_leaf: 1,
                tree: vec![
                    SavedNodeEntry::split(SavedDir::Horizontal, 0.4),
                    SavedNodeEntry::leaf_with(Some(SessionId(3)), Some("web 01".into())),
                    SavedNodeEntry::leaf_with(Some(SessionId(7)), Some("it's me".into())),
                ],
            }],
        };
        save(dir.path(), &before).unwrap();
        let got = load(dir.path());
        assert!(got.note.is_none(), "正常读回不该降级:{:?}", got.note);
        assert_eq!(got.layout, before);
    }

    /// D9:**旧版 exe 写出来的记录**(叶子只有 dir/ratio,没有身份字段)照样读得回来,
    /// 身份字段是 `None`。没有这条,升级之后第一次启动会把用户上次的现场整份判坏。
    #[test]
    fn a_file_written_by_an_older_exe_still_parses_with_empty_identities() {
        let text = r#"
schema_version = 1
active_tab = 0
updated_at = 0

[[tab]]
kind = "terminal"
session_id = 3
title = "prod"
focus_leaf = 0

  [[tab.tree]]
  dir = "horizontal"
  ratio = 0.5

  [[tab.tree]]

  [[tab.tree]]
"#;
        let got: SavedLayout = toml::from_str(text).expect("旧格式必须还读得动");
        assert_eq!(got.tabs[0].tree.len(), 3);
        assert!(got.tabs[0].tree[1].is_leaf());
        assert_eq!(got.tabs[0].tree[1].session_id, None);
        assert_eq!(got.tabs[0].tree[1].tmux, None);
    }

    #[test]
    fn a_layout_round_trips_through_the_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let before = sample();
        save(dir.path(), &before).unwrap();
        let got = load(dir.path());
        assert!(got.note.is_none(), "正常读回不该有降级说明:{:?}", got.note);
        assert_eq!(got.layout, before);
    }

    /// E6:文件不存在 = 空布局 + **不报警**。首次运行走的就是这条,
    /// 给它一句「读不出来」的日志只会让人以为出事了。
    #[test]
    fn a_missing_file_is_an_empty_layout_without_a_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let got = load(dir.path());
        assert_eq!(got.layout, SavedLayout::empty());
        assert!(got.note.is_none());
        assert!(got.layout.tabs.is_empty());
    }

    /// E6:垃圾字节 → 空布局 + 一句说明,**绝不挡住启动**。
    ///
    /// 自证会变红:把 `load` 里解析失败那条分支改成 panic / 改成把原文
    /// 硬塞进 layout。
    #[test]
    fn a_corrupt_file_degrades_to_an_empty_layout_instead_of_blocking_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LAYOUT_FILE), b"\x00\xff not toml [[[").unwrap();
        let got = load(dir.path());
        assert_eq!(got.layout, SavedLayout::empty());
        assert!(got.note.is_some(), "降级了却不说一声,排查时无从下手");
    }

    /// E6:更新版本写的文件**不猜**。字段可能改了含义,按旧版本读出来的
    /// 布局是错的,而错的布局比没有布局更难排查。
    #[test]
    fn a_file_from_a_newer_version_is_ignored_rather_than_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let mut future = sample();
        future.schema_version = CURRENT_LAYOUT_SCHEMA + 1;
        let text = toml::to_string_pretty(&future).unwrap();
        std::fs::write(dir.path().join(LAYOUT_FILE), text).unwrap();
        let got = load(dir.path());
        assert!(got.layout.tabs.is_empty(), "读了未来版本的标签");
        assert!(got.note.is_some());
    }

    /// 缺字段一律走 `#[serde(default)]`,不失败 —— 下一版加字段时,这一版
    /// 写出的文件还得能被读(反过来由上一条守)。
    #[test]
    fn missing_optional_fields_fall_back_to_defaults() {
        let text = "schema_version = 1\n";
        let got: SavedLayout = toml::from_str(text).expect("缺字段不该解析失败");
        assert_eq!(got.active_tab, 0);
        assert!(got.window.is_none());
        assert!(got.tabs.is_empty());
    }

    /// `save` 覆盖调用方填的版本号 —— 写出去的必须是**本版本**的格式,
    /// 否则一份被 v2 读过又被 v1 存回去的文件会带着 v2 的版本号和 v1 的内容。
    #[test]
    fn saving_always_stamps_the_current_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut lying = sample();
        lying.schema_version = 99;
        save(dir.path(), &lying).unwrap();
        let got = load(dir.path());
        assert_eq!(got.layout.schema_version, CURRENT_LAYOUT_SCHEMA);
        assert_eq!(got.layout.tabs.len(), 2, "版本被改对了,内容也要在");
    }

    /// 设计 E4 的**机械守护**:`mullion-store` 不许依赖任何别的 mullion crate。
    ///
    /// 破坏它只需要一行(`mullion-core.workspace = true`),收益是省掉 app 侧
    /// 一整个转换层 —— 所以它一定会有人想加。代价是布局树的两个真源合并:
    /// 此后 core 每改一次 `Node` 就同时改了磁盘格式。
    ///
    /// 自证会变红:往 `[dependencies]` 里加 `mullion-core.workspace = true`。
    #[test]
    fn the_store_crate_must_not_depend_on_any_other_mullion_crate() {
        let manifest = include_str!("../Cargo.toml");
        let deps = manifest
            .split("[dependencies]")
            .nth(1)
            .expect("Cargo.toml 里没有 [dependencies] 节?");
        // 只看到下一个节标题为止 —— `[dev-dependencies]` 里将来放测试辅助
        // crate 是允许的(它不进产物,不构成依赖方向问题)。
        let deps = deps.split("\n[").next().unwrap_or(deps);
        assert!(
            !deps.contains("mullion-"),
            "mullion-store 依赖了别的 mullion crate,违反架构不变量(设计 E4):\n{deps}"
        );
    }
}
