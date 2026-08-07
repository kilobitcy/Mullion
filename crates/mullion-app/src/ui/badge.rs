//! 会话外观(F61 图标 / F62 语义色):数据、解析缓存、落点判定、绘制原语。
//!
//! 三处落点 —— 会话列表行(`session_manager/list.rs`)、pane 标题条
//! (`pane_title.rs`)、状态栏(`chrome.rs`)—— 共用这里,不各画各的。
//!
//! **本模块不调 `inherit::resolve`,除了 `AppearanceCache::rebuild`。**
//! 那个函数的文档注释点名了陷阱 T3(喂数据和重绘没解耦):会话列表每帧要画
//! 几十行,逐行解析继承就是每秒几千次的无谓计算。绘制侧一律只收已解析好的
//! `&Appearance`。

use std::collections::HashMap;

// 全部走 `mullion_store` 顶层再导出(`lib.rs:26-29` 把 model 的这些类型都
// 摆到了顶层)。不混用 `mullion_store::model::X` 和 `mullion_store::X` 两条
// 路径引同一批类型——那会让人以为是两组不同的东西。
use mullion_store::{
    ColorSpec, ColorTarget, GroupRecord, IconKind, IconSpec, PrefsLayer, SessionId, SessionRecord,
};

use crate::theme::{self, Theme};

/// 从 `ResolvedConfig` 摘出来的外观部分。
///
/// 单独立一个类型而不是直接传 `ResolvedConfig`:后者还揣着 scrollback、
/// 代理、跳板、自动化 —— 绘制层不该看见那些,也不该因为它们变了就重画。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    pub icon: Option<IconSpec>,
    pub color: Option<ColorSpec>,
}

/// 这个落点该用什么颜色画。`None` = 不画。
///
/// 三处落点共用,所以「`apply_to` 过滤 + hex 解析失败降级」这两件事
/// 只有一份实现、只能错一次。
pub fn should_paint(a: &Appearance, target: ColorTarget) -> Option<egui::Color32> {
    let c = a.color.as_ref()?;
    if !c.apply_to.contains(&target) {
        return None;
    }
    theme::parse_hex(&c.hex).map(theme::c32)
}

/// 会话外观的解析缓存。
///
/// **存在的唯一理由是陷阱 T3。** `inherit::resolve` 的文档注释明确写着「结果
/// 应由调用方缓存，**不要在渲染热路径 / 每帧里重新调用**」。会话列表每帧要画
/// 几十行，pane 标题条每帧一条，逐个解析继承链就是每秒几千次无谓计算。
///
/// **重算入口只有 `rebuild` 一个**：保存 / 删除 / 分组变更后各调一次。
/// `get` 取 `&self` 且返回引用，类型上就不可能在内部现算再返回——这条约束
/// 是编译器保证的，不靠自觉。
#[derive(Debug, Default)]
pub struct AppearanceCache {
    map: HashMap<SessionId, Appearance>,
}

impl AppearanceCache {
    /// 按当前会话与分组重算全表。
    ///
    /// **层序必须是 `[会话, 分组]`**（`inherit::resolve` 的文档：「调用方负责
    /// 组装层序，当前为 `[会话, 分组]`」）。`cache_falls_back_to_group_appearance`
    /// 和 `session_appearance_overrides_group` 两条测试钉死这个顺序——写反了
    /// 会变成「分组盖掉会话」，用户改会话自己的颜色会不生效。
    ///
    /// `shell::store::SessionStore::resolved(id)` 做的是同一件事，这里**故意
    /// 不用**：那会让 `badge` 模块依赖 `SessionStore`，测试就得构造真 store
    /// （牵扯 keyring 和文件系统）。收纯数据切片换来纯单测，代价是层序组装
    /// 重复了一遍——上面那两条测试就是防这份重复漂移的。
    pub fn rebuild(&mut self, sessions: &[SessionRecord], groups: &[GroupRecord]) {
        self.map.clear();
        for rec in sessions {
            // 分组不存在(悬空 group_id)时只用会话自己这一层,跟
            // `group_manager` 把这类会话归进「未分组」是同一个姿态:一条坏
            // 引用不该让这条会话的外观整个消失。
            let group = rec
                .identity
                .group_id
                .and_then(|gid| groups.iter().find(|g| g.id == gid));
            let cfg = match group {
                Some(g) => mullion_store::resolve(&[rec as &dyn PrefsLayer, g as &dyn PrefsLayer]),
                None => mullion_store::resolve(&[rec as &dyn PrefsLayer]),
            };
            self.map.insert(
                rec.id,
                Appearance {
                    icon: cfg.icon,
                    color: cfg.color,
                },
            );
        }
    }

    /// 取一条会话的已解析外观。缓存里没有 → `None`(调用方按「没设外观」处理)。
    pub fn get(&self, id: SessionId) -> Option<&Appearance> {
        self.map.get(&id)
    }
}

/// emoji 值的 `char` 上限。ZWJ 家庭序列(👨‍👩‍👧 是 5 个 char)和旗帜要放得下,
/// 同时挡住用户把一整段文字粘进来撑爆行高。
///
/// 刻意不引 `unicode-segmentation` 做真字素分割:为一个上限校验加一个依赖
/// 不划算,而这个上限本来就是个粗筛。
pub const MAX_EMOJI_CHARS: usize = 8;

/// 边缘竖条宽度(逻辑点)。
pub const EDGE_BAR_W: f32 = 3.0;

/// emoji 值能不能画。空值和超长值都不画(走同一条降级路径)。
pub fn emoji_is_paintable(v: &str) -> bool {
    !v.is_empty() && v.chars().count() <= MAX_EMOJI_CHARS
}

/// 竖条画在 `rect` 的哪一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// 边缘竖条的矩形。抽成纯函数是因为「画在哪一边」是唯一能自动验的部分——
/// 画出来好不好看只有人眼能判定,但画反了边可以测。
pub fn edge_bar_rect(rect: egui::Rect, side: Side) -> egui::Rect {
    match side {
        Side::Left => egui::Rect::from_min_size(rect.min, egui::vec2(EDGE_BAR_W, rect.height())),
        Side::Right => egui::Rect::from_min_size(
            egui::pos2(rect.max.x - EDGE_BAR_W, rect.min.y),
            egui::vec2(EDGE_BAR_W, rect.height()),
        ),
    }
}

/// 画一条边缘竖条(F62)。
pub fn paint_edge_bar(p: &egui::Painter, rect: egui::Rect, side: Side, color: egui::Color32) {
    p.rect_filled(edge_bar_rect(rect, side), egui::Rounding::same(2.0), color);
}

/// 画一个图标(F61)。emoji 是唯一的图标载体。
///
/// 两条规则:
/// 1. **认不出的一律不画** —— `IconKind::Builtin`(内置形状,v0.1.24 按用户
///    要求撤掉)、`IconKind::Custom`(要引 image 解码器,顶爆 N6 的 25MB
///    体积线)、超长/空 emoji 共用这一条降级路径。两个枚举变体保留是因为
///    它们是 store schema 的一部分,旧配置里可能存在,读到不该崩。
/// 2. epaint **不支持 COLR/CPAL 彩色字形**,emoji 在界面上是**黑白剪影**。
///    这不是 bug,是 egui 的既有限制(内置字体 `NotoEmoji-Regular` /
///    `emoji-icon-font` 全是黑白轮廓,即使系统装了 Segoe UI Emoji 也一样)。
///
/// **不收会话语义色**:emoji 一律用 `fg` 原色画。内置形状撤掉之后没有任何
/// 可染色的图标载体了,留一个用不上的 `tint` 参数只会让调用方误以为它有效。
pub fn paint_icon(p: &egui::Painter, rect: egui::Rect, icon: &IconSpec, t: &Theme) {
    match icon.kind {
        IconKind::Emoji => {
            if !emoji_is_paintable(&icon.value) {
                return;
            }
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &icon.value,
                egui::FontId::proportional(rect.height().min(rect.width()) * 0.85),
                theme::c32(t.fg),
            );
        }
        IconKind::Builtin | IconKind::Custom => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored(hex: &str, targets: &[ColorTarget]) -> Appearance {
        Appearance {
            icon: None,
            color: Some(ColorSpec {
                hex: hex.to_string(),
                apply_to: targets.to_vec(),
            }),
        }
    }

    /// F62 的核心判定:颜色画在哪由 `apply_to` 说了算,不由落点自己决定。
    /// 三处落点共用这一个函数,所以过滤逻辑只有一份、只能错一次。
    #[test]
    fn should_paint_only_where_apply_to_says_so() {
        let a = colored("#e06767", &[ColorTarget::ListItem, ColorTarget::PaneTitle]);
        assert_eq!(
            should_paint(&a, ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
        assert_eq!(
            should_paint(&a, ColorTarget::PaneTitle),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
        assert_eq!(
            should_paint(&a, ColorTarget::StatusBar),
            None,
            "没勾状态栏就不该在状态栏上色"
        );
        assert_eq!(should_paint(&a, ColorTarget::Tab), None);
    }

    /// 没设色的会话在任何落点都不画。
    #[test]
    fn should_paint_returns_none_when_no_color_is_set() {
        let a = Appearance::default();
        for target in [
            ColorTarget::Tab,
            ColorTarget::ListItem,
            ColorTarget::PaneTitle,
            ColorTarget::StatusBar,
        ] {
            assert_eq!(should_paint(&a, target), None);
        }
    }

    /// `apply_to: []` 是**合法状态** =「色留着,暂时哪都不显示」。
    /// 编辑器里取消勾选所有落点不清除颜色(与跳板「切到无/继承时链条缓冲
    /// 不清空」同一条原则:用户切走再切回,配的东西还在),所以这个组合会
    /// 真实存在于配置里,不能当成坏数据。
    #[test]
    fn empty_apply_to_paints_nowhere_but_is_not_an_error() {
        let a = colored("#e06767", &[]);
        assert_eq!(should_paint(&a, ColorTarget::ListItem), None);
        assert!(a.color.is_some(), "颜色本身必须留着");
    }

    /// 坏 hex 降级成「没设色」,不 panic、不报错。配置文件被手改坏
    /// (或将来引入新写法的旧版本读到)不该让整张会话列表画不出来。
    #[test]
    fn unparseable_hex_degrades_to_no_color_instead_of_panicking() {
        let a = colored("not-a-color", &[ColorTarget::ListItem]);
        assert_eq!(should_paint(&a, ColorTarget::ListItem), None);
    }

    fn icon(kind: IconKind, value: &str) -> IconSpec {
        IconSpec {
            kind,
            value: value.to_string(),
        }
    }

    /// 数一帧里画出来的图形总数(递归展开 `Shape::Vec`)。
    ///
    /// 这是本模块唯一能自动验证「到底画没画」的手段:形状是 painter 直接
    /// 画的,没有 widget、没有 Response、没有文字锚点可以反查。
    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    /// 跑一帧,返回「画了 `icon` 时的图形数」。传 `None` 得到不画任何图标的
    /// 基线 —— `CentralPanel` 自己也会画背景,不能拿绝对数字当断言。
    fn shapes_with(icon: Option<&IconSpec>) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(i) = icon {
                    let rect =
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(16.0, 16.0));
                    paint_icon(ui.painter(), rect, i, &crate::theme::MULLION_DARK);
                }
            });
        });
        count_shapes(&out.shapes)
    }

    /// 能画的 emoji 必须真往 painter 里放东西 —— 这是
    /// `unrecognized_icons_paint_nothing` 的**对照组**。少了它,把 `paint_icon`
    /// 整个函数体删空也能让那条「不画」的测试全绿(恒真),等于没测。
    #[test]
    fn a_paintable_emoji_actually_paints_something() {
        let base = shapes_with(None);
        let n = shapes_with(Some(&icon(IconKind::Emoji, "🔥")));
        assert!(n > base, "能画的 emoji 必须画出图形(基线 {base},实际 {n})");
    }

    /// 认不出的值一律**不画**,与「没设图标」表现一致。五种情况共用这一条
    /// 降级路径,向前向后都不会崩:`IconKind::Builtin`(内置形状 v0.1.24 撤掉,
    /// 但 schema 变体还在、旧配置里可能有值)、`IconKind::Custom`(不做)、
    /// 空 emoji、emoji 超过 8 个 char。
    #[test]
    fn unrecognized_icons_paint_nothing() {
        let base = shapes_with(None);
        for bad in [
            icon(IconKind::Builtin, "circle"),
            icon(IconKind::Builtin, ""),
            icon(IconKind::Custom, "/path/to/some.png"),
            icon(IconKind::Emoji, ""),
            // 9 个 char > MAX_EMOJI_CHARS:用户把一整段文字粘进来会撑爆行高
            icon(IconKind::Emoji, "一二三四五六七八九"),
        ] {
            assert_eq!(
                shapes_with(Some(&bad)),
                base,
                "认不出的图标 {bad:?} 不该画任何东西"
            );
        }
    }

    /// emoji 长度上限:ZWJ 家庭序列(👨‍👩‍👧 是 5 个 char)和旗帜要放得下,
    /// 同时挡住把一整段文字粘进来。刻意不引 `unicode-segmentation` 做真
    /// 字素分割 —— 为一个上限校验加依赖不划算。
    #[test]
    fn emoji_length_limit_admits_zwj_sequences_and_rejects_prose() {
        assert!(emoji_is_paintable("🔥"));
        assert_eq!("👨‍👩‍👧".chars().count(), 5, "ZWJ 家庭序列确实是 5 个 char");
        assert!(emoji_is_paintable("👨‍👩‍👧"), "ZWJ 家庭序列必须放得下");
        assert!(!emoji_is_paintable(""), "空值不画");
        assert!(
            !emoji_is_paintable("这是一整段被粘贴进来的说明文字"),
            "超过上限的长文本必须挡住,否则会撑爆列表行高"
        );
    }

    /// 竖条画在指定的那一边,且宽度恒为 `EDGE_BAR_W`。
    ///
    /// 会话列表行的左 3px 已经被选中态 accent 条占了(见 `list.rs::session_row`),
    /// 语义色条必须走右边;pane 标题条没有这个占用,走左边。画反了两条会重叠,
    /// 选中态和标色在视觉上就合并了。
    #[test]
    fn edge_bar_sits_on_the_requested_side() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(50.0, 44.0));
        assert_eq!(
            edge_bar_rect(rect, Side::Left),
            egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(EDGE_BAR_W, 44.0))
        );
        assert_eq!(
            edge_bar_rect(rect, Side::Right),
            egui::Rect::from_min_size(
                egui::pos2(150.0 - EDGE_BAR_W, 200.0),
                egui::vec2(EDGE_BAR_W, 44.0)
            )
        );
    }

    // `GroupRecord`、`SessionId`、`SessionRecord`、`ColorSpec`、`ColorTarget` 已经从
    // `super::*` 带进来了,这里只补还缺的。
    use mullion_store::{AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, Protocol};

    fn rec(id: u64, group: Option<GroupId>, appearance: AppearancePrefs) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-07T00:00:00Z".into(),
            identity: Identity {
                name: format!("s{id}"),
                note: String::new(),
                group_id: group,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "192.0.2.1".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance,
            network: Default::default(),
            automation: Default::default(),
        }
    }

    fn group_with(id: GroupId, appearance: AppearancePrefs) -> GroupRecord {
        GroupRecord {
            id,
            name: "g".into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance,
            network: Default::default(),
            automation: Default::default(),
        }
    }

    fn appearance_with_color(hex: &str) -> AppearancePrefs {
        AppearancePrefs {
            icon: None,
            color: Some(ColorSpec {
                hex: hex.to_string(),
                apply_to: vec![ColorTarget::ListItem],
            }),
        }
    }

    /// 会话自己设了色就用自己的。
    #[test]
    fn cache_resolves_session_own_appearance() {
        let sessions = vec![rec(1, None, appearance_with_color("#e06767"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
    }

    /// 会话没设、分组设了 → 继承分组。
    ///
    /// 本切片分组管理器里**没有**外观编辑入口(`GroupRecord.appearance` 恒空),
    /// 但解析照走继承链。成本为零,而将来给分组接上外观时三处落点一行都不用改;
    /// 反过来若现在图省事直接读 `rec.appearance`,将来就得**记得**改三处——
    /// 那种「记得」正是漏掉的来源。这条测试就是那个「将来」的预演。
    #[test]
    fn cache_falls_back_to_group_appearance() {
        let gid = GroupId(7);
        let sessions = vec![rec(1, Some(gid), AppearancePrefs::default())];
        let groups = vec![group_with(gid, appearance_with_color("#7fd99b"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &groups);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0x7f, 0xd9, 0x9b)),
            "会话没设外观时应继承分组的"
        );
    }

    /// 会话设了就覆盖分组。**这条和上一条一起钉死层序是 `[会话, 分组]`**——
    /// 写反了会变成「分组盖掉会话」,用户改会话自己的颜色不生效。
    #[test]
    fn session_appearance_overrides_group() {
        let gid = GroupId(7);
        let sessions = vec![rec(1, Some(gid), appearance_with_color("#e06767"))];
        let groups = vec![group_with(gid, appearance_with_color("#7fd99b"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &groups);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
    }

    /// 悬空 `group_id`(分组已被删)不该让这条会话的外观整个消失,
    /// 只用会话自己那一层。
    #[test]
    fn dangling_group_id_falls_back_to_session_layer_only() {
        let sessions = vec![rec(1, Some(GroupId(999)), appearance_with_color("#e06767"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
    }

    /// **本切片最重要的一条结构性守护**:`get` 返回的是缓存住的值,不是当场
    /// 重算的。`inherit::resolve` 的文档注释点名了陷阱 T3——会话列表每帧要画
    /// 几十行,逐行解析继承就是每秒几千次无谓计算。
    ///
    /// 构造方法:`rebuild` 之后把源数据改掉,`get` 必须仍返回旧值。这不是在
    /// 鼓励用陈旧数据(调用方负责在记录变更后调 `rebuild`),而是证明 `get`
    /// 没有在背地里重算——重算入口只有 `rebuild` 一个,调用方才控制得住它
    /// 不落进渲染热路径。
    #[test]
    fn get_returns_the_cached_value_not_a_fresh_resolve() {
        let mut sessions = vec![rec(1, None, appearance_with_color("#e06767"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        sessions[0].appearance = appearance_with_color("#7fd99b");
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67)),
            "get 必须返回 rebuild 时缓存的值;返回新值说明它在渲染时现算,\
             那就是把 resolve 放进了每帧热路径(T3)"
        );
    }

    /// 缓存里没有的会话(比如刚被删掉、或 store 不可用)返回 `None`,
    /// 调用方按「没设外观」处理,不 panic。
    #[test]
    fn unknown_session_id_returns_none() {
        let c = AppearanceCache::default();
        assert!(c.get(SessionId(999)).is_none());
    }
}
