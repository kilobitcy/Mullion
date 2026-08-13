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

use crate::theme;

/// 从 `ResolvedConfig` 摘出来的外观部分。
///
/// 单独立一个类型而不是直接传 `ResolvedConfig`:后者还揣着 scrollback、
/// 代理、跳板、自动化 —— 绘制层不该看见那些,也不该因为它们变了就重画。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    pub icon: Option<IconSpec>,
    pub color: Option<ColorSpec>,
}

/// 这个落点该用什么颜色画,**未转成 `Color32`**。
///
/// 收 `Option<&ColorSpec>` 而不是 `&Appearance`:编辑器手上是一份还没保存的
/// `AppearancePrefs`,构造不出 `Appearance`。收最小的那个东西,三处调用方
/// (列表 / pane 标题条 / 编辑器预览)才能共用同一份 `apply_to` 过滤。
///
/// 返回 `Rgb` 而不是 `Color32`:对比度实算(`theme::contrast_ratio`)只吃 `Rgb`,
/// 而会话行背景要跟 `fg` 算对比度。
pub fn color_rgb(
    color: Option<&ColorSpec>,
    target: ColorTarget,
) -> Option<mullion_term::snapshot::Rgb> {
    let c = color?;
    if !c.apply_to.contains(&target) {
        return None;
    }
    theme::parse_hex(&c.hex)
}

/// 这个落点该用什么颜色画。`None` = 不画。`color_rgb` 的 `Color32` 包装。
///
/// 三处落点共用,所以「`apply_to` 过滤 + hex 解析失败降级」这两件事
/// 只有一份实现、只能错一次。
pub fn should_paint(a: &Appearance, target: ColorTarget) -> Option<egui::Color32> {
    color_rgb(a.color.as_ref(), target).map(theme::c32)
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

/// 边缘竖条宽度(逻辑点)。
pub const EDGE_BAR_W: f32 = 3.0;

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

/// 图标底色的圆角。跟着列表行的 6px 走,视觉上是同一套语言。
const ICON_BG_ROUNDING: f32 = 4.0;

/// 纹理缓存的条数上限。超了整片丢弃,下一帧重新解码。
///
/// 上限存在的理由:缓存挂在 `egui::Context` 的 temp data 上,**没有 GC**——
/// 用户每换一次图标就是一个新的 key,旧纹理会一直占着显存。真实上限本该是
/// 「会话数 × 2 档」,64 已经宽出一大截;够不着这个数的人永远不会触发清空,
/// 天天换图标的人也只是偶尔多解一次码。
const TEX_CACHE_MAX: usize = 64;

/// base64 → 已上传的纹理。
///
/// 放在 `Context` 的 temp data 里而不是 `UiState`:图标要在会话列表、Tab 条、
/// pane 标题条三处画,走 `UiState` 就得给这三条路径全部加一个参数,而它们中
/// 有两条现在只拿得到 `Painter`。`Painter::ctx()` 是现成的。
#[derive(Clone, Default)]
struct TexCache {
    map: HashMap<(u64, u32), egui::TextureHandle>,
}

/// 图标正文的指纹。`DefaultHasher` 够用 —— 这不是安全场景,碰撞的后果是
/// 两个图标串味,而不是越权。
fn fingerprint(b64: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b64.hash(&mut h);
    h.finish()
}

/// 取(必要时解码并上传)一张图标纹理。
///
/// **解码只在缓存未命中时发生**。每帧解一次 base64 + PNG 就是陷阱 T3 的
/// 教科书案例:会话列表几十行,一行一张图,GPU 没事干、CPU 先烧起来。
fn texture(ctx: &egui::Context, b64: &str, want: u32) -> Option<egui::TextureHandle> {
    let key = (fingerprint(b64), want);
    if let Some(h) = ctx.data(|d| {
        d.get_temp::<TexCache>(egui::Id::NULL)?
            .map
            .get(&key)
            .cloned()
    }) {
        return Some(h);
    }

    let frames = crate::ui::ico::decode(b64)?;
    let f = frames.pick(want);
    let img = egui::ColorImage::from_rgba_unmultiplied([f.size as usize, f.size as usize], &f.px);
    // LINEAR:32/64 两档之间还要被 GPU 拉伸到实际槽位大小,最近邻会让边缘
    // 出锯齿。图标本来就是给人看轮廓的,糊一点好过毛刺。
    let handle = ctx.load_texture(
        format!("mullion_ico_{}_{want}", key.0),
        img,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<TexCache>(egui::Id::NULL);
        if cache.map.len() >= TEX_CACHE_MAX {
            cache.map.clear();
        }
        cache.map.insert(key, handle.clone());
    });
    Some(handle)
}

/// 画一个图标(F61)。**用户导入的 `.ico` 是唯一的图标载体。**
///
/// 三条规则:
/// 1. **认不出的一律不画** —— `Builtin`(内置形状,v0.1.24 按用户要求撤掉)、
///    `Custom`(从未有 UI 产出过)、`Emoji`(v0.1.26 撤掉,理由见下)共用这条
///    降级路径。三个变体保留是因为它们是 store schema 的一部分,旧配置里
///    可能存在,读到不该崩 —— 但**也不该画**:emoji 在 epaint 下只能是黑白
///    剪影(不支持 COLR/CPAL 彩色字形),而列表新增的 64px 纯图标档里,一屏
///    黑白剪影根本认不出哪台是哪台。
/// 2. **底色垫在图标下面,由调用方给**。用户导入的 .ico 多半是给浅色资源
///    管理器画的,深色图标糊在深色面板上等于没有;垫一块底色是不改图本身就能
///    救回来的唯一办法。底色 = 会话的节点色(过该落点的 `apply_to` 闸门),
///    传 `None` 就直接画在面板上。`IconSpec.bg` 那个独立可配字段已停用。
/// 3. **不收会话语义色**:图标是用户自己的图,染色只会把它毁掉。语义色走
///    边缘竖条(`paint_edge_bar`)。
pub fn paint_icon(p: &egui::Painter, rect: egui::Rect, icon: &IconSpec, bg: Option<egui::Color32>) {
    if icon.kind != IconKind::Ico {
        return;
    }
    if let Some(bg) = bg {
        p.rect_filled(rect, egui::Rounding::same(ICON_BG_ROUNDING), bg);
    }
    let side = rect.width().min(rect.height());
    let Some(tex) = texture(p.ctx(), &icon.value, if side <= 32.0 { 32 } else { 64 }) else {
        return;
    };
    // 正方形居中:槽位可能是长方形(列表行的图标槽),按短边取正方形画,
    // 拉伸变形比留白难看得多。
    let square = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    p.image(
        tex.id(),
        square,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
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
            bg: None,
        }
    }

    /// 造一张真图标(纯色 `rgba`)的 base64,走的是生产代码那条归一化路径。
    fn real_ico(rgba: [u8; 4]) -> String {
        let px: Vec<u8> = std::iter::repeat_n(rgba, 32 * 32).flatten().collect();
        let img = ico::IconImage::from_rgba_data(32, 32, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        crate::ui::ico::import(&raw).unwrap()
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

    /// 跑一帧,返回「画了 `icon`(底色 `bg`)时的图形数」。传 `None` 得到不画
    /// 任何图标的基线 —— `CentralPanel` 自己也会画背景,不能拿绝对数字当断言。
    fn shapes_with_bg(icon: Option<&IconSpec>, bg: Option<egui::Color32>) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(i) = icon {
                    let rect =
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(16.0, 16.0));
                    paint_icon(ui.painter(), rect, i, bg);
                }
            });
        });
        count_shapes(&out.shapes)
    }

    /// 不带底色的简写。现有几条测试只关心「画没画图标」。
    fn shapes_with(icon: Option<&IconSpec>) -> usize {
        shapes_with_bg(icon, None)
    }

    /// 一张真图标必须真往 painter 里放东西 —— 这是
    /// `unrecognized_icons_paint_nothing` 的**对照组**。少了它,把 `paint_icon`
    /// 整个函数体删空也能让那条「不画」的测试全绿(恒真),等于没测。
    #[test]
    fn a_real_ico_actually_paints_something() {
        let base = shapes_with(None);
        let n = shapes_with(Some(&icon(IconKind::Ico, &real_ico([9, 9, 9, 255]))));
        assert!(n > base, "导入的 .ico 必须画出图形(基线 {base},实际 {n})");
    }

    /// 底色现在由**调用方**传:图标画在哪个落点,就用哪个落点的 `apply_to`
    /// 判定,而 `paint_icon` 自己拿不到 `target`(同一张图在列表和标题条上
    /// 该不该有底色,答案可能不同)。
    ///
    /// 第二段钉死 `IconSpec.bg` 真的停用了:schema 里还留着这个字段(旧配置
    /// 里可能有值,读到不该崩),但**不该再影响绘制**。
    ///
    /// 自证会变红:让 `paint_icon` 回头去读 `icon.bg`(第二段炸);
    /// 把那句 `rect_filled` 删掉(第一段炸)。
    #[test]
    fn the_backdrop_comes_from_the_caller_not_from_the_stored_bg_field() {
        let b64 = real_ico([9, 9, 9, 255]);
        let plain = shapes_with_bg(Some(&icon(IconKind::Ico, &b64)), None);
        let with_bg = shapes_with_bg(
            Some(&icon(IconKind::Ico, &b64)),
            Some(egui::Color32::from_rgb(0x1e, 0x88, 0xe5)),
        );
        assert!(
            with_bg > plain,
            "传了底色应当多画一层(没传 {plain},传了 {with_bg})"
        );

        let legacy = shapes_with_bg(
            Some(&IconSpec {
                kind: IconKind::Ico,
                value: b64,
                bg: Some("#1e88e5".into()),
            }),
            None,
        );
        assert_eq!(legacy, plain, "已停用的 IconSpec.bg 不该再影响绘制");
    }

    /// 认不出的值一律**不画**,与「没设图标」表现一致。四种情况共用这一条
    /// 降级路径:三个历史 `IconKind`(schema 里还在、旧配置里可能有值)和
    /// 一段解不开的 base64。
    ///
    /// emoji 尤其要**不画**:epaint 不支持彩色字形,画出来是黑白剪影,而列表
    /// 的 64px 纯图标档下一屏剪影根本认不出哪台是哪台(v0.1.26 据此撤掉)。
    #[test]
    fn unrecognized_icons_paint_nothing() {
        let base = shapes_with(None);
        for bad in [
            icon(IconKind::Builtin, "circle"),
            icon(IconKind::Custom, "/path/to/some.png"),
            icon(IconKind::Emoji, "🔥"),
            icon(IconKind::Ico, "这不是 base64"),
        ] {
            assert_eq!(
                shapes_with(Some(&bad)),
                base,
                "认不出的图标 {bad:?} 不该画任何东西"
            );
        }
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
            sftp: Default::default(),
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

    /// 闸门只有一份实现:`should_paint` 是 `color_rgb` 的 `Color32` 包装。
    /// 拆两层是因为对比度实算(`theme::contrast_ratio`)只吃 `Rgb`,而会话行
    /// 背景要跟 `fg` 算对比度;两处各写一遍 `apply_to` 过滤迟早分叉。
    #[test]
    fn should_paint_is_just_the_color32_wrapper_over_color_rgb() {
        let a = colored("#e06767", &[ColorTarget::ListItem]);
        assert_eq!(
            color_rgb(a.color.as_ref(), ColorTarget::ListItem),
            theme::parse_hex("#e06767")
        );
        assert_eq!(
            color_rgb(a.color.as_ref(), ColorTarget::PaneTitle),
            None,
            "没勾的落点一律 None"
        );
        assert_eq!(
            should_paint(&a, ColorTarget::ListItem),
            color_rgb(a.color.as_ref(), ColorTarget::ListItem).map(theme::c32)
        );
        assert_eq!(
            color_rgb(None, ColorTarget::ListItem),
            None,
            "没设色 → None"
        );
    }
}
