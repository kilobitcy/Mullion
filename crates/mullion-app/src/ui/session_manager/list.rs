//! 会话管理器**左栏**:搜索框、分组树、手绘会话行、底部「+ 新建」、删除二次确认。
//!
//! 只读 `UiFrame` 的数据,只往 `UiState` 写意图 —— 不碰 `SessionStore`
//! (egui 闭包里拿不到 `&mut SessionStore`,这是 app 侧的硬约束)。

use egui::{NumExt as _, Ui};
use mullion_store::model::SessionRecord;
use mullion_store::{GroupRecord, SessionId};

use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::session_manager::{group_header, SwitchTarget};
use crate::ui::UiState;

/// 左栏的三档密度(F61)。**宽度是唯一输入** —— 用户拖分隔条就是在选档。
///
/// 为什么按宽度自动切、而不是加一个「视图」菜单:菜单要占位置、要存状态、
/// 还要教用户去哪儿找;而「我想让左栏占多少地方」这个意图,拖分隔条本身
/// 已经表达完了,再问一遍是多余的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Density {
    /// 32px 图标 + 名称/副标题两行。默认档。
    Full,
    /// 32px 图标 + 名称单行。副标题(user@host)让位。
    Compact,
    /// 只有 32px 图标。**没设图标的行整条隐藏** —— 这一档认图标不认字,
    /// 留一行空白比不留更糟。
    Icons,
}

/// 切档阈值。`Compact` 的上限踩的是「名称 + 副标题两行还读得出东西」的下界;
/// `Icons` 的上限踩的是「32px 图标右边还剩得下几个字」的下界。
///
/// `ICONS_BELOW` 必须**严格大于** `LIST_MIN_W`,否则 `density_for` 永远落不到
/// `Icons`,那一档等于不存在(`narrowing_the_list_only_ever_simplifies_it` 钉着)。
const COMPACT_BELOW: f32 = 208.0;
const ICONS_BELOW: f32 = 88.0;

pub(crate) fn density_for(width: f32) -> Density {
    if width < ICONS_BELOW {
        Density::Icons
    } else if width < COMPACT_BELOW {
        Density::Compact
    } else {
        Density::Full
    }
}

/// 一行会话的高度。`Full` 是设计稿 §4.1(两行文字 + 上下 8px);另两档由
/// 图标边长决定 —— 图标是这两档唯一的内容,行高小于它就会被裁掉。
pub(crate) fn row_h(d: Density) -> f32 {
    match d {
        Density::Full => 48.0,
        Density::Compact => 40.0,
        Density::Icons => 48.0,
    }
}

/// 图标边长。三档统一 32 —— 正是用户导入 .ico 时归一化出来的小那一帧
/// (`ui::ico::SMALL`)。64 那一帧从此没有绘制点在用,但归一化仍产出它
/// (那是存储格式的一部分,改它要迁移已有配置,收益为零)。
fn icon_px(_d: Density) -> f32 {
    crate::ui::ico::SMALL as f32
}

/// 图标槽位中心距行左边缘(逻辑点)。= 左边距 8 + 半个图标。状态点已下线,
/// 图标直接贴左边缘,不再给点留位置。
const ICON_SLOT_X: f32 = 24.0;
/// 文字左边界。= 图标槽位右沿 + 8px 间距,**恒定**(见 `session_row` 注释)。
fn text_x(d: Density) -> f32 {
    ICON_SLOT_X + icon_px(d) / 2.0 + 8.0
}

/// `Full` 档名称文字的上沿距行顶。
const NAME_TOP: f32 = 9.0;
/// `Full` 档副标题文字的上沿距行顶。
const SUB_TOP: f32 = 27.0;
/// 副标题字号。基线居中的断言要用它算下留白,所以不能只写在
/// `FontId::proportional(11.0)` 那一处 —— 两处写同一个数迟早分叉。
const SUB_FONT_PX: f32 = 11.0;

/// 选中态节点色的混合比例。低透明度而不是纯色铺满:8 个预设里有浅色(黄),
/// 纯色铺满时 `fg` 白字会掉到不可读;混合后底色始终由 `panel_bg` 主导,
/// 文字对比度不随用户选的颜色漂移(`every_preset_colour_keeps_the_row_text_readable_when_selected` 钉着)。
const SELECTED_ALPHA: f32 = 0.28;
/// 悬停态的混合比例。必须小于 `SELECTED_ALPHA`,否则两态分不出来。
const HOVER_ALPHA: f32 = 0.14;

/// 把 `top` 按 `a` 的比例混到 `base` 上,得到一个**不透明**的结果色。
///
/// 不用 `Color32::from_rgba_unmultiplied` 交给 GPU 混:那样算出来的最终像素
/// 依赖底下画了什么,测不了。这里显式跟 `panel_bg` 混,结果是确定的一个色值。
fn blend(base: egui::Color32, top: egui::Color32, a: f32) -> egui::Color32 {
    let mix = |b: u8, t: u8| {
        (b as f32 + (t as f32 - b as f32) * a)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(
        mix(base.r(), top.r()),
        mix(base.g(), top.g()),
        mix(base.b(), top.b()),
    )
}

/// 一行会话的背景色。`None` = 不铺(普通态)。
///
/// 抽成纯函数(不收 `Ui`/`Painter`)是这一整块能被测的前提:混在 `session_row`
/// 里的话,「选中背景到底是什么色」只能靠数图元反推。
///
/// `node` 已经过了 `apply_to` 闸门(调用方传 `badge::color_rgb(..., ListItem)`)——
/// 用户明确取消勾选「会话列表」之后,颜色不该还从背景里冒出来。
pub(crate) fn row_bg(
    selected: bool,
    hovered: bool,
    node: Option<mullion_term::snapshot::Rgb>,
    t: &Theme,
) -> Option<egui::Color32> {
    let alpha = if selected {
        SELECTED_ALPHA
    } else if hovered {
        HOVER_ALPHA
    } else {
        return None;
    };
    Some(match node {
        Some(c) => blend(theme::c32(t.panel_bg), theme::c32(c), alpha),
        None if selected => theme::c32(t.sunken_bg),
        None => theme::c32(t.panel_head),
    })
}

/// 会话是否命中搜索。空查询放行全部。名称 / 主机 / 标签三处都查,
/// 大小写不敏感 —— 用户记得住的常是 IP 尾数或标签,不是当初起的名字。
pub(crate) fn matches(rec: &SessionRecord, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    rec.identity.name.to_lowercase().contains(&q)
        || rec.connection.host.to_lowercase().contains(&q)
        || rec
            .identity
            .tags
            .iter()
            .any(|t| t.to_lowercase().contains(&q))
}

/// 手绘一行会话。不用 `selectable_label`:设计稿要「图标 + 名称 + user@host
/// 两行 + 选中态节点色背景」,`selectable_label` 只画得出单行文本。
///
/// F61/F62 加了两样东西:**右**边缘的语义色竖条(未选中行认色全靠它 ——
/// 选中行有背景色,未选中行只有这条竖条)、贴左边缘的 32px 图标槽位。
/// **槽位恒占**——没设图标的行也留白,否则有图标和没图标的行文字左边界参差。
#[allow(clippy::too_many_arguments)]
fn session_row(
    ui: &mut Ui,
    t: &Theme,
    rec: &SessionRecord,
    sub: &str,
    selected: bool,
    appearance: &crate::ui::badge::Appearance,
    query: &str,
    d: Density,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h(d)),
        egui::Sense::click(),
    );
    let p = ui.painter();

    // F62:选中/悬停背景由会话自己的节点色主导(过 `ListItem` 闸门)。原来的
    // 「左 3px accent 竖条」已删 —— 整行都是节点色之后,再压一条固定色的竖条
    // 是两套颜色语言在同一行里打架。
    let node = crate::ui::badge::color_rgb(
        appearance.color.as_ref(),
        mullion_store::ColorTarget::ListItem,
    );
    if let Some(bg) = row_bg(selected, resp.hovered(), node, t) {
        p.rect_filled(rect, egui::Rounding::same(6.0), bg);
    }

    paint_row_body(
        p,
        rect,
        t,
        &rec.identity.name,
        sub,
        rec.connection.protocol,
        appearance,
        query,
        d,
    );

    // 窄档把名字(以及 `Icons` 档连副标题)全藏了 —— 不补 tooltip,用户就只
    // 剩「挨个点开看」这一条路。`Full` 档不补:那里字都在,再挂一层重复的
    // 悬浮框只会挡住下一行。
    match d {
        Density::Full => resp,
        Density::Compact => resp.on_hover_text(sub),
        Density::Icons => resp.on_hover_text(format!("{name}\n{sub}", name = rec.identity.name)),
    }
}

/// 会话名后面要不要挂一个协议标签。**ssh 不挂** —— 列表里 99% 的行都是 ssh,
/// 全挂等于没挂,还把名字挤窄了。
///
/// 走查没提这条,是决策树里补的:`Protocol` 已经是 schema 的一部分、编辑器
/// 也让用户选,但列表上看不出来,存了一条 sftp 会话跟 ssh 长得一模一样。
pub(crate) fn protocol_pill(p: mullion_store::Protocol) -> Option<&'static str> {
    match p {
        mullion_store::Protocol::Ssh => None,
        mullion_store::Protocol::Sftp => Some("SFTP"),
    }
}

/// 协议标签的内边距、与名字之间的间距。抽成常量是因为算「名字还剩多少宽度」
/// 时要用同一组数 —— 两处写死同一个数字迟早分叉,那时名字会把 pill 挤出行外。
const PILL_PAD: egui::Vec2 = egui::vec2(5.0, 1.0);
const PILL_GAP: f32 = 6.0;

/// 先把协议标签的文字排好版但不画。分两步是为了在画名字**之前**就知道 pill
/// 要占多宽(名字的截断宽度得先扣掉它)。
fn pill_galley(p: &egui::Painter, t: &Theme, text: &str) -> std::sync::Arc<egui::Galley> {
    p.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(10.0),
        theme::c32(t.fg_muted),
    )
}

/// pill 的整体占宽(含内边距和左侧间距)。
fn pill_w(g: &egui::Galley) -> f32 {
    PILL_GAP + g.size().x + PILL_PAD.x * 2.0
}

/// 画协议标签。**中性灰**(`sunken_bg` 底 + `fg_muted` 字),不碰 F62 的语义
/// 配色 —— 那套色是用户自己赋的含义,协议是客观事实,混一起两边都读不准。
fn paint_pill(
    p: &egui::Painter,
    t: &Theme,
    left_top: egui::Pos2,
    galley: std::sync::Arc<egui::Galley>,
) {
    let rect = egui::Rect::from_min_size(left_top, galley.size() + PILL_PAD * 2.0);
    p.rect_filled(rect, egui::Rounding::same(4.0), theme::c32(t.sunken_bg));
    p.galley(rect.min + PILL_PAD, galley, theme::c32(t.fg_muted));
}

/// 文字右侧必须留出的空白:F62 语义色竖条 3px + 5px 呼吸。名称和副标题的可用
/// 宽度都要先扣掉它 —— 不扣的话字会压在竖条上。
const TEXT_RIGHT_PAD: f32 = crate::ui::badge::EDGE_BAR_W + 5.0;

/// 一行会话的**内容**:语义色竖条 + 图标 + 两行文字。背景与选中条不在
/// 这里(那两样只对真列表行有意义)。
///
/// 抽出来是给「图标」页的实时预览复用的(走查 4 后半)。预览要是另写一套绘制,
/// 两边迟早漂移 —— 那时候预览就是在骗人,比没有预览更糟。
///
/// 参数多是因为它**不认识 `SessionRecord`** —— 预览页手上只有一份还没保存的
/// 表单缓冲,拿不出记录来。收散装数据是让两边共用同一份绘制的代价。
#[allow(clippy::too_many_arguments)]
fn paint_row_body(
    p: &egui::Painter,
    rect: egui::Rect,
    t: &Theme,
    name: &str,
    sub: &str,
    protocol: mullion_store::Protocol,
    appearance: &crate::ui::badge::Appearance,
    query: &str,
    d: Density,
) {
    // F62:语义色竖条走**右**边缘 —— 左 3px 归选中态 accent,两者各占一边。
    if let Some(c) =
        crate::ui::badge::should_paint(appearance, mullion_store::ColorTarget::ListItem)
    {
        crate::ui::badge::paint_edge_bar(p, rect, crate::ui::badge::Side::Right, c);
    }

    // F61:图标槽位。`Full`/`Compact` 档**恒占**,画不画都留着 —— 有图标的行
    // 和没图标的行文字左边界必须对齐,否则列表看起来像坏了。
    // `Icons` 档没有文字要对齐,图标居中摆,而且没图标的行压根不会走到这里
    // (`show()` 已经把它们滤掉了)。
    let px = icon_px(d);
    let icon_center = match d {
        Density::Icons => rect.center(),
        _ => egui::pos2(rect.left() + ICON_SLOT_X, rect.center().y),
    };
    if let Some(icon) = &appearance.icon {
        crate::ui::badge::paint_icon(
            p,
            egui::Rect::from_center_size(icon_center, egui::vec2(px, px)),
            icon,
            crate::ui::badge::should_paint(appearance, mullion_store::ColorTarget::ListItem),
        );
    }
    // `Icons` 档到此为止:名称、副标题、协议 pill 全部让位给那张 32px 图。
    if d == Density::Icons {
        return;
    }

    // `Compact` 档只有名称一行,竖直居中;`Full` 档名称在上、副标题在下。
    let name_y = match d {
        Density::Compact => rect.center().y - 9.0,
        _ => rect.top() + NAME_TOP,
    };
    let text_left = rect.left() + text_x(d);
    // 两行文字共同的可用宽度。行本身是 `allocate_exact_size` 给的固定矩形,
    // 超出去的部分被 `ScrollArea` 的 clip **硬裁**(没有省略号),用户看到的是
    // 一个从中间断掉的 host —— 这正是要修的。
    let text_avail = (rect.right() - TEXT_RIGHT_PAD - text_left).max(0.0);

    // 协议 pill 挂在名字右边,所以名字的可用宽度要**先**扣掉它。不扣的话名字
    // 会一路截断到右边缘,pill 100% 被挤到行外(整条看不见)。
    let pill = protocol_pill(protocol).map(|tag| pill_galley(p, t, tag));
    let name_avail = (text_avail - pill.as_ref().map_or(0.0, |g| pill_w(g))).max(0.0);

    let name_rect = paint_highlighted(
        p,
        egui::pos2(text_left, name_y),
        name,
        query,
        egui::FontId::proportional(14.0),
        theme::c32(t.fg),
        t,
        name_avail,
    );
    if let Some(g) = pill {
        paint_pill(p, t, name_rect.right_top() + egui::vec2(PILL_GAP, 1.0), g);
    }
    if d == Density::Compact {
        return;
    }
    paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + SUB_TOP),
        sub,
        query,
        egui::FontId::proportional(SUB_FONT_PX),
        // WCAG AA:fg_faint(#565b70) on panel_bg(#14161f) 只有 2.69:1,
        // fg_dimmer(#8a90a8) 是 5.71:1。不动 token 本身 —— 它在别处
        // (禁用态、装饰线)是对的。
        theme::c32(t.fg_dimmer),
        t,
        text_avail,
    );
}

/// 画一行文字,命中搜索的那几段染成 accent(走查 22),**超过 `max_width` 的部分
/// 用省略号收掉**。返回它占的矩形(协议 pill 要挂在名字右边)。
///
/// 用一个 `LayoutJob` 分段着色而不是画好几次 `p.text`:后者要自己算每段的
/// 宽度再累加,一遇到连字/CJK 混排就对不齐。
///
/// 原来还有一条「没在搜索就走 `p.text`」的快路径,已经删掉:`Painter::text`
/// 内部走 `layout_no_wrap`(`max_width = INFINITY`),文字想画多长画多长,超出
/// 行矩形的部分被外层 clip 硬裁 —— 于是长 host 显示成从中间断掉的
/// `ubuntu@very-long-hostname.internal.examp`,既没有省略号也没有任何「后面还有」
/// 的提示。省一次 `LayoutJob` 分配换来这个,不值。
///
/// `TextWrapping::truncate_at_width` 带 `break_anywhere: true` —— 对 hostname 和
/// CJK 都对:不该为了凑词边界而把断点提前一大截。
#[allow(clippy::too_many_arguments)]
fn paint_highlighted(
    p: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    query: &str,
    font: egui::FontId,
    color: egui::Color32,
    t: &Theme,
    max_width: f32,
) -> egui::Rect {
    let segs = super::highlight::segments(text, query);
    let mut job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping::truncate_at_width(max_width),
        ..Default::default()
    };
    for (piece, hit) in &segs {
        job.append(
            piece,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: if *hit { theme::c32(t.accent) } else { color },
                ..Default::default()
            },
        );
    }
    let galley = p.layout_job(job);
    let rect = egui::Rect::from_min_size(pos, galley.size());
    p.galley(pos, galley, color);
    rect
}

/// 「图标」页的实时预览(走查 4 后半):画一行**假的**会话行,让用户当场看到
/// 自己配的图标和颜色在列表里长什么样。
pub(crate) fn preview_row(
    ui: &mut Ui,
    t: &Theme,
    name: &str,
    sub: &str,
    protocol: mullion_store::Protocol,
    appearance: &crate::ui::badge::Appearance,
) {
    // 宽度取 `LIST_W` 一档(280),不吃满右栏 —— 预览要像左边那条列表,
    // 拉成整页宽反而不像。
    let w = ui.available_width().min(280.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(w, row_h(Density::Full)), egui::Sense::hover());
    // 铺 `panel_bg`:真列表行的底色来自左栏 `SidePanel` 的填充,不铺的话
    // 预览行浮在 `bar_status` 上,颜色对比跟实际不一样。
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(6.0), theme::c32(t.panel_bg));
    paint_row_body(
        ui.painter(),
        rect,
        t,
        name,
        sub,
        protocol,
        appearance,
        "",
        Density::Full,
    );
}

/// 「+ 新建」按钮的显式 id。原来用 `ui.button(...)`,egui 给它分配的是自动 id
/// (`self.next_auto_id_salt` 计数器,只保证同一次调用序列内跨帧稳定,外部测试
/// 代码算不出来,见 egui-0.30.0 `ui.rs::next_auto_id`/`allocate_space`)。复核 F90
/// 缺陷时发现:守护测试靠反查渲染出的「+ 新建」文字锚点来判定按钮是否被挤出
/// 屏幕,但文字锚点只在按钮矮(默认字号)时约等于按钮矩形——按钮被撑高后
/// (比如放大界面缩放/无障碍字号),锚点还停在按钮内容区顶部附近,真正该测的
/// 按钮底边早就跑到远处去了。实测过一次假阳性:按钮真实矩形底边 694,文字
/// 锚点只有 637,屏幕高 680——14px 的真实溢出被锚点判定当成了通过。
///
/// 跟 `toolbar.rs::button_id` 同一个理由、同一种做法:挂一个不依赖任何父 `Ui`
/// id 栈的显式全局 id,测试侧用 `Context::read_response` 直接读回真实
/// `Response::rect` 来判定。这个按钮全程序只出现一次,不会跟别处撞 id。
pub(crate) fn new_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_new_button")
}

/// 手绘「+ 新建」按钮,挂 `new_button_id()`(为什么不能再用 `ui.button()` 的
/// 自动 id,见上方注释)。背景色/描边直接取 `ui.style().interact(&resp)`——跟
/// `egui::Button::ui()` 内部算 `frame_fill`/`frame_stroke` 用的是同一套视觉规则
/// (见 egui-0.30.0 `widgets/button.rs`),所以外观跟默认按钮基本一致。
///
/// `ui.allocate_space` 只预留布局空间、不注册交互(不像 `allocate_exact_size`
/// 会顺带用自动 id 注册一次 `Sense::hover` 部件)——跟 `toolbar.rs::show_in` 里
/// 「先 `allocate_space` 占位,再逐个用显式 id `interact`」是同一个套路,避免
/// 同一块矩形被注册成两个互相打架的部件。
fn new_button(ui: &mut Ui) -> egui::Response {
    let galley = egui::WidgetText::from("+ 新建").into_galley(
        ui,
        None,
        ui.available_width(),
        egui::TextStyle::Button,
    );
    let padding = ui.spacing().button_padding;
    let size =
        (galley.size() + padding * 2.0).at_least(egui::vec2(0.0, ui.spacing().interact_size.y));
    let (_auto_id, rect) = ui.allocate_space(size);
    let resp = ui.interact(rect, new_button_id(), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
        );
        let text_pos = ui
            .layout()
            .align_size_within_rect(galley.size(), rect.shrink2(padding))
            .min;
        ui.painter().galley(text_pos, galley, visuals.text_color());
    }
    resp
}

/// 列表里从上到下的会话顺序,搜索过滤后的那一份(走查 16 的 ↑↓ 要照着它走)。
///
/// 跟 `show()` 一样先用 `group_manager::group_sessions` 归桶再按 `matches`
/// 过滤 —— 顺序必须跟眼睛看到的一致,自己另写一套排序迟早跟渲染那边分叉。
///
/// **不看分组的折叠状态**:折叠状态存在 `ctx.data()` 里,拿它要 `&Context`,
/// 而这是个纯函数。代价是方向键会走进折叠着的分组(选中了但那一行看不见);
/// 收益是这段顺序逻辑能脱离 egui 单测。折叠+键盘导航是小众组合,先记在这儿。
pub(crate) fn visible_order(
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    query: &str,
    protocol: mullion_store::Protocol,
) -> Vec<SessionId> {
    crate::ui::group_manager::group_sessions(groups, sessions)
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        .filter(|r| on_page(r, query, protocol))
        .map(|r| r.id)
        .collect()
}

/// 这条记录在当前页可见吗 —— 协议 + 搜索词,两个判据成对出现。
///
/// 只判搜索会让另一档协议的记录漏进来;只判协议会让搜索失效。
/// `visible_order`(键盘顺序)与 `show`(渲染)都走这一个函数,
/// 是「方向键跳到看不见的行」那条失效模式的唯一防线 —— 谁再加一个
/// 过滤点,也必须调它,不许在别处重写这个条件。
fn on_page(rec: &SessionRecord, query: &str, protocol: mullion_store::Protocol) -> bool {
    rec.connection.protocol == protocol && matches(rec, query)
}

// 同 `session_manager::show`:内部叶子函数,为压参数数引一个专用结构体
// 没有实际收益(`row` 更早就是这个情况,见它头上的同款 allow)。
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    tunnels: &[mullion_store::TunnelRecord],
    tunnel_states: &[(mullion_store::TunnelId, mullion_ssh::tunnel::TunnelState)],
    appearance: &crate::ui::badge::AppearanceCache,
    protocol: mullion_store::Protocol,
) {
    // D3:待确认删除的那条会话,引用它的隧道里有几条正在跑。整个列表只有
    // 这一行用得上,所以在这里算一次。
    let running_note = ui_state
        .pending_delete
        .and_then(|id| super::tunnel_list::running_note(id, tunnels, tunnel_states));
    // 搜索框
    let search_resp = ui.add(
        egui::TextEdit::singleline(&mut ui_state.search)
            .hint_text(theme::hint_text(t, "搜索名称 / 主机 / 标签"))
            .desired_width(f32::INFINITY),
    );
    annotate::mark(ui.ctx(), "会话管理器/左栏/搜索框", search_resp.rect);
    ui.add_space(crate::ui::metrics::SP_S);

    // 待确认删除的目标一旦这一帧没被真正渲染出来 —— 原因可能是搜索词把它
    // 滤掉、所在分组被手动折叠(`CollapsingHeader` 折叠时根本不会执行 body
    // 闭包,见 egui-0.30.0 `collapsing_header.rs:199-205` 的 `openness <= 0.0`
    // 分支)、会话本身已经不存在,或者将来任何新增的隐藏方式 —— 就必须清空
    // `pending_delete`。不清的话:确认框跟着那一行一起从视觉上消失,但状态
    // 还在原地;用户清空搜索词、重新展开分组、或者关闭再打开会话管理器,
    // 确认框会带着上次的意图凭空重新出现,用户可能在不知情的情况下点到
    // 「删除」——这正好抵消了做二次确认的初衷。
    //
    // 用「渲染前捕获的旧值」而不是逐个原因特判:`pending_delete_target` 在
    // 循环开始前就固定下来,`row()` 只在 `rec.id == pending_delete_target`
    // 时才把 `pending_delete_rendered` 置位。这样如果本帧内某一行刚被右键
    // 新设了 `pending_delete`(新值不等于循环前捕获的旧 `pending_delete_target`),
    // 不会被误当成「目标已渲染」,于是不会在同一帧里被下面的清空逻辑立刻抹掉。
    let pending_delete_target = ui_state.pending_delete;
    let mut pending_delete_rendered = false;

    // 三档密度只看左栏这一刻有多宽。在 `ScrollArea` **之前**取:进了滚动区
    // 之后 `available_width` 已经扣掉滚动条,会在阈值附近来回抖档。
    let d = density_for(ui.available_width());

    // SFTP 节点连不上(F50)。行为由 mod.rs 的统一闸门保证;这里管的是
    // 「让用户看得出来为什么点不动」。
    let connectable = protocol == mullion_store::Protocol::Ssh;

    // 底部「分隔线 + 新建按钮」用 `TopBottomPanel::bottom` 先占位:egui 的面板
    // 布局保证面板先分配自己的高度,再把外层 `ui` 的可用区底边收缩到面板上沿
    // (见 egui-0.30.0 `containers/panel.rs::show_inside` 里 `TopBottomSide::Bottom`
    // 分支的 `cursor.max.y = rect.min.y`),下面的 `ScrollArea` 就能吃到真实剩余
    // 高度——不再需要手算一个「必须跟底部实际渲染内容同步」的魔法数字
    // (原 `BOTTOM_BAR_H`,已删除,这正是那条注释警告过的坑)。
    // **必须在 `ScrollArea` 之前调用**,顺序颠倒的话可用区收缩不会对后面的
    // 部件生效。
    //
    // `.frame(Frame::none())`:面板默认背景取 `style.visuals.panel_fill`,跟外层
    // `SidePanel`(`mod.rs` 里用主题色 `t.panel_bg` 铺底)对不上,不清空会在底部
    // 露出一条颜色不一致的色带。`.show_separator_line(false)`:面板自带分隔线,
    // 不关掉会跟手绘的 `ui.separator()` 叠成两条线。
    egui::TopBottomPanel::bottom(ui.id().with("sm_list_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                let b = new_button(ui);
                annotate::mark(ui.ctx(), "会话管理器/左栏/新建按钮", b.rect);
                if b.clicked() {
                    ui_state.pending_switch = Some(SwitchTarget::NewDraft);
                }
            });
        });

    let searching = !ui_state.search.trim().is_empty();
    // 走查 22:搜不到任何东西时,列表是一整片空白 —— 用户分不清「没有匹配」
    // 和「会话都没了」。给一句话 + 一个回到全部列表的出口。
    if searching
        && !sessions
            .iter()
            .any(|r| on_page(r, &ui_state.search, protocol))
    {
        ui.add_space(crate::ui::metrics::SP_XS);
        ui.colored_label(theme::c32(t.fg_dimmer), "没有匹配的会话");
        if ui.button("清空搜索").clicked() {
            ui_state.search.clear();
        }
    }

    let mut hidden = 0usize;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // 用 `group_manager::group_sessions` 归桶(而不是自己按 group_id 手动
            // 分组):它已经处理了「分组被删后会话落进未分组桶而不是消失」这条
            // 既有保证(见 `group_manager::tests::session_with_dangling_group_id_falls_into_ungrouped_not_dropped`),
            // 自己重写一遍分组逻辑会悄悄丢掉这条回归保护。
            for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
                let matched: Vec<&SessionRecord> = bucket
                    .into_iter()
                    .filter(|r| on_page(r, &ui_state.search, protocol))
                    .collect();
                // `Icons` 档只认图标:没设图标的行画出来是一格空白,既点不
                // 明白也占地方,整条藏掉。藏了多少条在列表末尾如实说一声 ——
                // 悄悄少几条会被当成「会话丢了」。
                let members: Vec<&SessionRecord> = if d == Density::Icons {
                    matched
                        .iter()
                        .copied()
                        .filter(|r| appearance.get(r.id).is_some_and(|a| a.icon.is_some()))
                        .collect()
                } else {
                    matched.clone()
                };
                hidden += matched.len() - members.len();
                if members.is_empty() && (searching || d == Density::Icons) {
                    continue; // 搜索时 / 图标档下不显示空分组
                }
                let title = match gid {
                    Some(id) => groups
                        .iter()
                        .find(|g| g.id == id)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "未分组".to_string()),
                    None => "未分组".to_string(),
                };
                // 搜索期间强制展开:`default_open` 只在 CollapsingState 首次
                // 加载时生效,用户手动折叠过就被持久化进 ctx.data(),再也展不开。
                let force = if searching { Some(true) } else { None };
                let header = group_header(&title, gid, members.len())
                    .open(force)
                    .show(ui, |ui| {
                        for r in &members {
                            row(
                                ui,
                                t,
                                ui_state,
                                r,
                                sessions,
                                groups,
                                tunnels,
                                running_note.as_deref(),
                                pending_delete_target,
                                &mut pending_delete_rendered,
                                appearance,
                                d,
                                connectable,
                            );
                        }
                    });
                // 只标表头那一条,不标展开后的整块:整块必然包含每一行,而 hit test
                // 取面积最小者,标了也只会在「点到行与行之间的空隙」时才命中。
                annotate::mark(
                    ui.ctx(),
                    format!("会话管理器/左栏/分组头「{title}」"),
                    header.header_response.rect,
                );
            }
            if hidden > 0 {
                ui.add_space(crate::ui::metrics::SP_XS);
                ui.colored_label(theme::c32(t.fg_dimmer), format!("+{hidden} 无图标"))
                    .on_hover_text(format!(
                        "这一档只显示设了图标的会话,另有 {hidden} 条被藏起来了。\n把左栏拖宽一点就能看到它们。"
                    ));
            }
        });

    // 多加一层「当前值仍等于帧初捕获的旧值」才清空:没有这层的话,
    // 「旧目标 X 这一帧没渲染」+「同一帧内另一行 Z 被右键新设了
    // `pending_delete`」这两件事一旦同时发生,会把 Z 刚写下的新值一并
    // 当成「目标未渲染」误删——即使当下的调用路径(右键菜单一次只能设
    // 一个目标、且设置发生在渲染期间而非渲染后)让这个组合在今天走不到,
    // 这个不变量也不该靠「时序上凑不出来」去担保:后续任务要改右栏和
    // 切换确认接线,谁也不能保证不会在渲染循环中间插入新的赋值点。加一次
    // `Option<SessionId>` 比较,把「只清我这一帧开始时看到的那个目标」变成
    // 结构上精确的不变量,不再依赖任何时序论证。
    if pending_delete_target.is_some()
        && !pending_delete_rendered
        && ui_state.pending_delete == pending_delete_target
    {
        ui_state.pending_delete = None;
    }
}

/// 画一行 + 挂交互(单击选中 / 双击连接 / 右键删除确认)。
///
/// `pending_delete_target` / `pending_delete_rendered`:「这一帧是否真的渲染过
/// 待确认删除的目标行」的事后判定标志,见调用侧 `show()` 里的说明——
/// 只在 `rec.id == pending_delete_target` 时置位,不直接读 `ui_state.pending_delete`,
/// 避免本帧内刚发生的新右键覆盖被误当成「旧目标已渲染」。
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &SessionRecord,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    tunnels: &[mullion_store::TunnelRecord],
    // 删除确认里那句「其中 N 条正在运行」。只有待确认的那一行用得上,
    // 所以在 `show` 里算一次传下来,不是每行都去查一遍运行时表。
    running_note: Option<&str>,
    pending_delete_target: Option<SessionId>,
    pending_delete_rendered: &mut bool,
    appearance: &crate::ui::badge::AppearanceCache,
    d: Density,
    connectable: bool,
) {
    if pending_delete_target == Some(rec.id) {
        *pending_delete_rendered = true;
    }

    let selected = ui_state.editor_id == Some(rec.id);
    // 缓存里没有这条(store 刚删掉、或还没 rebuild)就按「没设外观」画。
    let default_appearance = crate::ui::badge::Appearance::default();
    let a = appearance.get(rec.id).unwrap_or(&default_appearance);
    // 走查 3:列表上有另一行长得一模一样时,副标题后面追加一段区分信息
    // (分组名 / 端口 / 备注首句)。没有重名时 `disambiguate` 返回 `None`,
    // 副标题保持原样 —— 不给每行平白加尾巴。
    let sub = match super::dedupe::disambiguate(rec, sessions, groups) {
        Some(extra) => format!("{}@{} · {}", rec.auth.user, rec.connection.host, extra),
        None => format!("{}@{}", rec.auth.user, rec.connection.host),
    };
    let resp = session_row(ui, t, rec, &sub, selected, a, &ui_state.search, d);
    // 带上会话名:同一个插桩点会登记出十几行,只写「会话行」的话导出里全是
    // 一模一样的路径,读的人分不出说的是哪一行。
    annotate::mark(
        ui.ctx(),
        format!("会话管理器/左栏/会话行「{}」", rec.identity.name),
        resp.rect,
    );
    if resp.clicked() {
        ui_state.pending_switch = Some(SwitchTarget::Session(rec.id));
    }
    // egui 的点击检测在双击时也会让 `clicked()` 为 true(实现见
    // egui-0.30.0 `response.rs:138-145` 配合 `context.rs:1306-1308` 的点击
    // 计数逻辑,并非某条可引用的文档原文),所以双击这一行会在同一帧里把
    // `pending_switch` 和 `connect_request` 都写下。目前无害(`pending_switch`
    // 还没有消费点),但 Task 14 接脏检查确认时,需要决定 `connect_request`
    // 是否也要走那道确认门。
    if connectable && resp.double_clicked() {
        ui_state.connect_request = Some(rec.id);
    }
    resp.context_menu(|ui| {
        // 走查 3:菜单里原本只有「跳过自动化」这一条连接项 —— 右键一打开,
        // 主操作(直接连)反而不在,用户只能关掉菜单再双击。普通连接排第一。
        if ui
            .add_enabled(connectable, egui::Button::new("连接"))
            .on_disabled_hover_text(super::editor::SFTP_NOT_YET)
            .clicked()
        {
            ui_state.connect_request = Some(rec.id);
            ui.close_menu();
        }
        // F44:一次性逃生门。远端 tmux 里正跑着 Claude Code 时,用户可能只想
        // 连上去看一眼,不想让自动化再发一遍 attach。
        if ui
            .add_enabled(connectable, egui::Button::new("连接(跳过自动化)"))
            .on_disabled_hover_text(super::editor::SFTP_NOT_YET)
            .clicked()
        {
            ui_state.connect_request = Some(rec.id);
            ui_state.connect_skip_automation = true;
            ui.close_menu();
        }
        ui.separator();
        // 走查 3:改分组原本只能进右栏编辑器改下拉再保存。这里给一条直路。
        ui.menu_button("移动到分组", |ui| {
            // 当前所在的那一项也列出来但禁掉 —— 直接不列的话,用户会以为
            // 「这个分组不存在了」。
            let cur = rec.identity.group_id;
            if ui
                .add_enabled(cur.is_some(), egui::Button::new("未分组"))
                .clicked()
            {
                ui_state.move_to_group = Some((rec.id, None));
                ui.close_menu();
            }
            for g in groups {
                if ui
                    .add_enabled(cur != Some(g.id), egui::Button::new(&g.name))
                    .clicked()
                {
                    ui_state.move_to_group = Some((rec.id, Some(g.id)));
                    ui.close_menu();
                }
            }
        });
        if ui.button("删除").clicked() {
            ui_state.pending_delete = Some(rec.id);
            ui.close_menu();
        }
    });

    // §4.3:删除确认内联展开在被删那一行下面,不再弹第三个窗口。
    if ui_state.pending_delete == Some(rec.id) {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .inner_margin(8.0)
            .rounding(6.0)
            .show(ui, |ui| {
                ui.colored_label(
                    theme::c32(t.danger_soft),
                    format!("删除「{}」?", rec.identity.name),
                );
                // F110:这条会话被删掉后,引用它的隧道会变成悬垂。删之前
                // 就说清楚影响面 —— 事后才发现「有条端口转发不动了」,
                // 排查起点离原因太远。没有隧道引用时**一行都不多画**。
                let affected = super::tunnel_list::affected_lines(rec.id, tunnels);
                if !affected.is_empty() {
                    ui.colored_label(theme::c32(t.warn), "以下隧道会失去引用:");
                    for line in affected {
                        ui.colored_label(theme::c32(t.fg_dimmer), format!("· {line}"));
                    }
                    // D3:正在跑的那些是**此刻**要被切断的连接,比「配置悬垂」
                    // 分量重,单独一行、用告警色说。
                    if let Some(note) = running_note {
                        ui.colored_label(theme::c32(t.danger_soft), note);
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("删除").clicked() {
                        ui_state.delete_request = Some(rec.id);
                        ui_state.pending_delete = None;
                    }
                    if ui.button("取消").clicked() {
                        ui_state.pending_delete = None;
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol, SessionRecord};
    use mullion_store::SessionId;

    fn rec(id: u64, name: &str, host: &str, tags: &[&str]) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-03T00:00:00Z".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: tags.iter().map(|s| s.to_string()).collect(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 协议标签只给非 ssh 的行挂。列表里 99% 都是 ssh,全挂等于没挂,还把
    /// 名字挤窄了 —— 但存了一条 sftp 会话时必须一眼看得出来。
    ///
    /// 自证会变红:把 `protocol_pill` 的 `Protocol::Ssh` 分支改成 `Some("SSH")`。
    #[test]
    fn only_non_ssh_rows_get_a_protocol_pill() {
        use mullion_store::Protocol;
        assert_eq!(protocol_pill(Protocol::Ssh), None, "ssh 是默认,不该挂标签");
        assert_eq!(protocol_pill(Protocol::Sftp), Some("SFTP"));

        // 渲染层:一行 sftp 会话应该比同样的 ssh 行多画东西(底 + 字)。
        let mut ssh = rec(1, "dev-box", "192.0.2.10", &[]);
        let mut sftp = ssh.clone();
        sftp.connection.protocol = Protocol::Sftp;
        ssh.connection.protocol = Protocol::Ssh;
        let n_ssh = count_shapes(&run_list_with(&[ssh], Protocol::Ssh).shapes);
        let n_sftp = count_shapes(&run_list_with(&[sftp], Protocol::Sftp).shapes);
        assert!(
            n_sftp > n_ssh,
            "sftp 行应多画出协议标签(ssh {n_ssh} 个图形,sftp {n_sftp} 个)"
        );
    }

    /// 搜索要覆盖名称 / 主机 / 标签三处,且大小写不敏感 —— 用户记得住的往往是
    /// IP 尾数或标签,不是当初起的名字。
    #[test]
    fn search_matches_name_host_and_tags_case_insensitively() {
        let r = rec(1, "Prod-DB", "192.0.2.10", &["生产", "MySQL"]);
        assert!(matches(&r, ""), "空查询放行全部");
        assert!(matches(&r, "  "), "只有空白的查询等同空查询");
        assert!(matches(&r, "prod"), "名称匹配应大小写不敏感");
        assert!(matches(&r, "2.10"), "主机子串应匹配");
        assert!(matches(&r, "mysql"), "标签匹配应大小写不敏感");
        assert!(!matches(&r, "staging"), "无关词不该匹配");
    }

    /// 复核坑:待确认删除的会话被搜索过滤掉后,`pending_delete` 必须清空。
    /// 不清的话,那一行连同确认框一起从视觉上消失,但状态还在原地——用户
    /// 清空搜索词、或关闭再打开会话管理器,确认框会带着上次的意图凭空重新
    /// 出现,用户可能在不知情下点到「删除」,抵消了二次确认的意义。
    ///
    /// 自证会变红:把 `show()` 里 `pending_delete_target.is_some() && !pending_delete_rendered`
    /// 这段清空逻辑注释掉,这条立刻报 `pending_delete` 仍是 `Some(SessionId(1))`。
    #[test]
    fn pending_delete_is_cleared_when_the_session_is_filtered_out_by_search() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            pending_delete: Some(SessionId(1)),
            search: "no-match-at-all".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    &[],
                    &[],
                    &crate::ui::badge::AppearanceCache::default(),
                    mullion_store::Protocol::Ssh,
                );
            });
        });
        assert_eq!(
            ui_state.pending_delete, None,
            "该会话被搜索过滤掉后,待确认删除状态必须清空,否则确认框会在\
             搜索词清空或重新打开窗口时凭空复现"
        );
    }

    /// 复核坑:待确认删除的会话所在分组被**手动折叠**时,`pending_delete` 也
    /// 必须清空——不是靠搜索过滤,而是 `CollapsingHeader` 折叠时根本不执行
    /// body 闭包(`openness <= 0.0` 直接返回,见 egui-0.30.0
    /// `collapsing_header.rs:199-205`),`row()` 从未被调用。上一轮「查
    /// sessions 里是否存在 + 是否命中搜索」的清空逻辑对这条路径完全无效:
    /// 分组折叠根本不经过搜索过滤,`matches()` 仍然为真,`pending_delete`
    /// 就会原地悬空,直到用户重新展开分组时凭空复现确认框。
    ///
    /// 用真实的 `CollapsingState::load_with_default_open`、`set_open(false)`
    /// 和 `store` 在渲染前把「未分组」桶的持久化折叠状态落地成「已折叠」——
    /// 这跟用户之前手动点过一次折叠按钮落地的状态完全一样,不是绕过注入点
    /// 的假测试。id 按生产代码里的真实推导链手工算出,一共叠了两层
    /// `Id::from("child")`(不是一层——曾经因为漏算这一层,用错的 id 导致
    /// 这条测试第一次跑起来是绿的假阳性,靠打印 `.value()` 全量 64 位跟生产
    /// 代码实际用的 id 逐层比对,才挖出这第二层):
    /// 1. `ScrollArea` 的 `content_ui`:id_salt 缺省是 `Id::from("child")`
    ///    (见 egui `ui.rs:265` `new_child` + `ui.rs:592-596` `ScrollArea::begin`
    ///    没有显式设置 `.id_salt()`);
    /// 2. `CollapsingHeader::show` 内部把 header + body 包了一层
    ///    `ui.vertical(|ui| { self.begin(ui) .. })`(见 `collapsing_header.rs:639-648`
    ///    的 `show_dyn`),`Ui::vertical` 同样没设 id_salt(见 `ui.rs:2519-2524`),
    ///    也缺省成 `Id::from("child")`;
    /// 3. 最后叠 `group_header` 里 `.id_salt(gid)` 对应的 `Id::new(gid)`
    ///    (`ui.make_persistent_id` = `self.id.with(&id_salt)`,见 `ui.rs:1022-1027`)。
    ///
    /// 自证会变红:把 `show()` 里 `pending_delete_target.is_some() && !pending_delete_rendered`
    /// 这段清空逻辑注释掉,这条立刻报 `pending_delete` 仍是 `Some(SessionId(1))`。
    #[test]
    fn pending_delete_is_cleared_when_the_group_is_manually_collapsed() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            pending_delete: Some(SessionId(1)),
            search: String::new(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // 把「未分组」桶(gid = None)的折叠状态,用跟生产代码完全
                // 一致的 id 推导链(见上方文档注释的三步),提前落地成「已折叠」。
                let header_id = ui
                    .id()
                    .with(egui::Id::from("child")) // ScrollArea content_ui
                    .with(egui::Id::from("child")) // CollapsingHeader 内部的 ui.vertical()
                    .with(egui::Id::new(None::<mullion_store::GroupId>));
                let mut state =
                    egui::containers::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        header_id,
                        true,
                    );
                state.set_open(false);
                state.store(ui.ctx());

                show(
                    ui,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    &[],
                    &[],
                    &crate::ui::badge::AppearanceCache::default(),
                    mullion_store::Protocol::Ssh,
                );
            });
        });
        assert_eq!(
            ui_state.pending_delete, None,
            "分组被手动折叠后那一行根本没有渲染,待确认删除状态必须清空,\
             否则用户重新展开分组时确认框会带着上次意图凭空复现"
        );
    }

    /// 在渲染出的 `FullOutput.shapes` 里找一段文本第一次出现的锚点(`TextShape::pos`)。
    /// 跟 `ui/mod.rs` 里 `rendered_text` helper 一样,是这个项目验证「真按坐标点下去」
    /// 时的既有手法(见 `build_ui_clicking_a_preset_button_wires_through_to_actions_f82`),
    /// 不是猜像素——`session_row`/`row()` 都是手绘的,没有 label 能挂 id,只能反过来
    /// 从已经画出来的文本反推矩形。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => Some(t.pos),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 复核明确要求验证的「同帧竞态」:`show()` 在渲染前把 `ui_state.pending_delete`
    /// 复制成局部变量 `pending_delete_target`(`Option<SessionId>` 是 `Copy`,复制后
    /// 这份局部值不可能再被后续任何赋值影响到)。如果右键删除确认恰好在**这一帧**
    /// 里真正落地(点了菜单里的「删除」),新值必然不等于循环前复制的旧
    /// `pending_delete_target`,所以帧尾那段「未渲染就清空」的逻辑
    /// (只在 `pending_delete_target.is_some() && !pending_delete_rendered` 时才清)
    /// 不会把这一帧刚写下的新值当场抹掉。
    ///
    /// 用真实指针事件驱动(右键在会话行上打开菜单 → 下一帧点菜单里的「删除」
    /// 按钮),不是直接手动赋值 `ui_state.pending_delete` 后调 `show()`——那样测的是
    /// 「值已经在那儿」的稳态,证不了「这一帧刚写入」的竞态时序。菜单按钮的矩形
    /// 通过扫描 `FullOutput.shapes` 里已经画出来的「删除」文字反推(`find_text_pos`),
    /// 不是猜像素坐标。
    #[test]
    fn pending_delete_set_this_frame_by_context_menu_is_not_erased_in_the_same_frame() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![
            rec(1, "session-a-unique-name", "192.0.2.10", &[]),
            rec(2, "session-b-unique-name", "192.0.2.20", &[]),
        ];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };

        // 前两帧只是让布局稳定下来,不带任何指针事件。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        let row_pos = find_text_pos(&out.shapes, "session-a-unique-name")
            .expect("session-a 这一行应该已经画出来了");
        // `session_row` 画名字时用的锚点是 `rect.left()+30, rect.top()+7`
        // (见本文件顶部 `session_row` 的 `p.text` 调用),反推回行内一个安全点。
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);

        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );
        // 右键这一帧不该直接写 `pending_delete`——菜单里的「删除」按钮还没被点。
        assert_eq!(
            ui_state.pending_delete, None,
            "右键只应该打开菜单,不应该在同一次点击里就直接确认删除"
        );

        // 菜单弹出用的是 `Area`,跟 `rendered_text` 文档注释里说的一样:
        // 首次遇到某个 id 时先做一趟不可见的 sizing pass,真正把内容画出来
        // 要等下一帧——所以这里再空跑一帧(不带任何指针事件)才能看到「删除」。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let delete_btn_pos =
            find_text_pos(&out.shapes, "删除").expect("右键打开的菜单里应该画出了「删除」按钮");
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(delete_btn_pos),
                    primary_click(delete_btn_pos, true),
                    primary_click(delete_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.pending_delete,
            Some(SessionId(1)),
            "点「删除」这一帧刚写下的 pending_delete 不该被同一帧末尾的清空逻辑\
             当场抹掉——它是本帧渲染前复制的旧值(None)对比出来的,不该影响\
             本帧新写入的值"
        );
    }

    /// F44:右键菜单「连接(跳过自动化)」必须**同时**设对两个字段——
    /// `connect_request`(要连哪一条)和 `connect_skip_automation`(跳过自动化
    /// 这个意图)。手法照抄上面 `pending_delete_set_this_frame_by_context_menu_is_not_erased_in_the_same_frame`:
    /// 真实指针事件右键打开菜单,下一帧用 `find_text_pos` 反推「连接(跳过自动化)」
    /// 按钮矩形再点下去,不直接手动赋值 `ui_state` 字段。
    #[test]
    fn context_menu_skip_automation_sets_both_connect_request_and_skip_flag() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "session-a-unique-name", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };

        // 前两帧只是让布局稳定下来,不带任何指针事件。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        let row_pos = find_text_pos(&out.shapes, "session-a-unique-name")
            .expect("session-a 这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);

        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );

        // 菜单弹出用的是 `Area`,首次遇到某个 id 先做一趟不可见的 sizing pass,
        // 真正把内容画出来要等下一帧,所以再空跑一帧(不带任何指针事件)。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let skip_btn_pos = find_text_pos(&out.shapes, "连接(跳过自动化)")
            .expect("右键打开的菜单里应该画出了「连接(跳过自动化)」按钮");
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(skip_btn_pos),
                    primary_click(skip_btn_pos, true),
                    primary_click(skip_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.connect_request,
            Some(SessionId(1)),
            "点「连接(跳过自动化)」必须设 connect_request,否则点了没反应,\
             用户会以为菜单项是坏的"
        );
        assert!(
            ui_state.connect_skip_automation,
            "点「连接(跳过自动化)」必须设 connect_skip_automation,否则只是\
             普通连接——用户点了跳过自动化,自动化却照样跑了"
        );
    }

    /// D4:SFTP 档(F50 未实现)右键菜单里的「连接」必须是真的按不动,不只是
    /// 看起来灰——`add_enabled(false, ..)` 让 egui 在 `enabled=false` 时把
    /// `Response::clicked` 钉死为 `false`(见 egui-0.30.0 `context.rs::get_response`
    /// 里 `res.clicked = true` 的赋值必须 `enabled && ...` 同时成立)。
    ///
    /// 这条测试专测**视觉层**(list.rs 的 `add_enabled`),不是 mod.rs 的兜底
    /// 闸门——闸门测的是「万一某条入口漏挡,出了 `show()` 也会被清空」;这里
    /// 测的是「入口本身一开始就不该被点动」,两层分别有测试,任何一层被削弱
    /// 都要变红。手法照抄上面 `context_menu_skip_automation_sets_both_connect_request_and_skip_flag`:
    /// 真实指针事件右键打开菜单、`find_text_pos` 反推「连接」按钮矩形再点下去,
    /// 不直接手动赋值 `ui_state.connect_request`。
    ///
    /// 自证会变红:把 `row()` 里两处 `ui.add_enabled(connectable, egui::Button::new(..))`
    /// 改回 `ui.button(..)`,`connect_request` 就会被设上。
    #[test]
    fn sftp_row_context_menu_connect_button_is_truly_unclickable_not_just_grey() {
        let t = crate::theme::MULLION_DARK;
        let mut sftp = rec(1, "sftp-node-unique-name", "192.0.2.30", &[]);
        sftp.connection.protocol = Protocol::Sftp;
        let sessions = vec![sftp];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Sftp,
                    );
                });
            })
        };

        // 前两帧只是让布局稳定下来,不带任何指针事件。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        let row_pos = find_text_pos(&out.shapes, "sftp-node-unique-name")
            .expect("sftp 节点这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);

        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );

        // 菜单弹出用的是 `Area`,首次遇到某个 id 先做一趟不可见的 sizing pass,
        // 真正把内容画出来要等下一帧,所以再空跑一帧(不带任何指针事件)。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        // `find_text_pos` 是子串匹配(见本文件 `walk` 里的 `.contains(needle)`),
        // "连接" 在这个菜单里同时命中「连接」和「连接(跳过自动化)」两个按钮
        // (右栏 Tab 条第一个页签也叫「连接」)。这里子串匹配是安全的,前提有二:
        // 1. 两个菜单按钮的 `add_enabled(connectable, ..)` 门控完全一样 —— 命中
        //    哪一个,点击后 `connect_request` 的断言结果都相同;
        // 2. 本测试全程只发 secondary(右键)事件,从未 `PointerButton::Primary`
        //    点过会话行本身,`ui_state.editor` 恒为 `None`,右栏画的是空态提示
        //    而不是 Tab 条,撞不上同名页签。
        // 谁要是往这条测试里加一次左键点击(哪怕只是顺便验证选中态),第 2 条
        // 前提就破了,子串匹配可能悄悄定位到页签而不是菜单按钮 —— 测试会变成
        // 恒绿却依旧通过,加左键点击前必须换成唯一锚点(比如带上「(跳过自动化)」
        // 排除歧义,或直接测右栏 Tab 条隐藏)。
        let connect_btn_pos = find_text_pos(&out.shapes, "连接")
            .expect("右键打开的菜单里应该画出了「连接」按钮(即便是禁用状态,文字也照常画)");
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(connect_btn_pos),
                    primary_click(connect_btn_pos, true),
                    primary_click(connect_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.connect_request, None,
            "SFTP 档「连接」按钮必须真的点不动——egui 的 `enabled=false` \
             应该在按钮层面就拦下点击,不能指望 mod.rs 的兜底闸门兜底"
        );
    }

    /// 找到含 `needle` 的那个 `TextShape`,连它所在的 clip 矩形一起回。
    /// 判「有没有被硬裁」必须两样都有:光有 galley 只知道文字多宽,不知道
    /// 容器允许它多宽。
    fn find_clipped_text(
        shapes: &[egui::epaint::ClippedShape],
        needle: &str,
    ) -> Option<(std::sync::Arc<egui::Galley>, egui::Pos2, egui::Rect)> {
        fn walk(
            shape: &egui::Shape,
            clip: egui::Rect,
            needle: &str,
        ) -> Option<(std::sync::Arc<egui::Galley>, egui::Pos2, egui::Rect)> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, clip, needle)),
                egui::Shape::Text(ts) if ts.galley.job.text.contains(needle) => {
                    Some((ts.galley.clone(), ts.pos, clip))
                }
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|cs| walk(&cs.shape, cs.clip_rect, needle))
    }

    /// 用给定宽度渲染一次左栏。**宽度必须显式给**:它同时决定密度档和文字的
    /// 截断宽度,而 `RawInput::default()` 不带 `screen_rect`,egui 会兜底成一个
    /// 极宽的矩形,再长的 host 也撑不满,截断类的测试会恒绿。
    ///
    /// 也调 `apply_egui`:滚动条样式挂在全局 `Style` 上,不调的话测的是 egui
    /// 的默认样式,不是这个应用真正跑的那套。
    fn run_list_sized(sessions: &[SessionRecord], size: egui::Vec2) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &t);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut out = None;
        // `ScrollArea` 第一帧还不知道内容有多高,滚动条要到下一帧才决定画不画
        // (`show_scroll_this_frame` 取自上一帧存下的 `State`)。跑三帧取最后一帧。
        for _ in 0..3 {
            out = Some(ctx.run(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        &mut ui_state,
                        sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            }));
        }
        out.unwrap()
    }

    /// 长 `user@host` 必须用省略号收掉,不能画出行外让 clip 硬裁。
    ///
    /// 离屏截图 harness 的第一批产出就是撞在这儿:副标题显示成
    /// `ubuntu@very-long-hostname.internal.examp` —— 从中间断掉、没有省略号、
    /// 也没有任何「后面还有」的提示,用户没法判断自己看到的是完整主机名还是
    /// 半截。根因是 `Painter::text` 走 `layout_no_wrap`(`max_width = INFINITY`)。
    ///
    /// 自证会变红:把 `paint_highlighted` 里的
    /// `TextWrapping::truncate_at_width(max_width)` 换成 `TextWrapping::default()`
    /// (= `no_max_width`),这条立刻报「文字右边缘超出 clip」。
    #[test]
    fn a_long_host_is_elided_with_an_ellipsis_instead_of_being_hard_clipped() {
        let mut r = rec(1, "短名", "very-long-hostname.internal.example.com", &[]);
        r.auth.user = "ubuntu".into();
        // 280 = `LIST_W`,左栏的默认宽度,也就是用户实际看到的那一档。
        let out = run_list_sized(&[r], egui::vec2(280.0, 400.0));
        let (galley, pos, clip) =
            find_clipped_text(&out.shapes, "ubuntu@").expect("副标题应该已经画出来了");

        assert!(
            pos.x + galley.size().x <= clip.max.x + 0.5,
            "副标题右边缘 {} 超出了 clip 右边缘 {} —— 超出的字被硬裁,用户看到\
             一个从中间断掉的主机名",
            pos.x + galley.size().x,
            clip.max.x
        );
        // 只测「没超出」还不够:宽度足够时它天然不超,断言会恒绿。必须同时
        // 证明**截断真的发生了**,而且是以省略号收尾。
        let last = galley
            .rows
            .last()
            .and_then(|row| row.glyphs.last())
            .map(|g| g.chr);
        assert_eq!(
            last,
            Some('…'),
            "这么长的 host 在 280 宽的左栏里必须被截断并以省略号收尾,实际末\
             字符是 {last:?}"
        );
    }

    /// 内容比视口高时,滚动条的滑块在**指针不在列表里**的时候也必须看得见。
    ///
    /// egui 默认 `ScrollStyle::floating()` 的 `dormant_handle_opacity = 0.0`
    /// ——静止时整条滚动条 alpha 为 0。后果:12 条会话只看得见 9 条,屏幕上
    /// 没有任何提示,用户以为列表就这么长(滚动本身一直是好的,这也是离屏
    /// 截图 harness 的产出之一)。
    ///
    /// 判据取「画出来的那个窄竖条 alpha 非零」而不是去读 `Style` 里的数字:
    /// 后者只证明配置写对了,证不到 egui 真的按它画。宽度上界取 2.5 是为了跟
    /// 行内其它 3px 竖条区分开(选中态 accent 条、F62 语义色条)——本测试
    /// 既不选中任何行、也不给任何语义色,那两条压根不会画。
    ///
    /// 自证会变红:把 `theme::scroll_style` 里的 `dormant_handle_opacity: 0.45`
    /// 删掉(退回 `floating()` 的 0.0),这条立刻报找不到可见的滑块。
    #[test]
    fn scrollbar_handle_stays_visible_when_the_pointer_is_not_over_the_list() {
        let sessions: Vec<SessionRecord> = (1..=12)
            .map(|i| rec(i, &format!("节点 {i:02}"), "192.0.2.10", &[]))
            .collect();
        // 300 高装不下 12 行(每行 44 + 分组头),必然溢出。整个渲染过程不喂
        // 任何指针事件,所以滚动条全程处于 dormant 态。
        let out = run_list_sized(&sessions, egui::vec2(280.0, 300.0));

        fn find_thin_bar(shape: &egui::Shape) -> Option<egui::Color32> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(find_thin_bar),
                egui::Shape::Rect(r)
                    if r.rect.width() > 0.0
                        && r.rect.width() <= 2.5
                        && r.rect.height() >= 20.0
                        && r.fill.a() > 0 =>
                {
                    Some(r.fill)
                }
                _ => None,
            }
        }
        let handle = out.shapes.iter().find_map(|cs| find_thin_bar(&cs.shape));
        assert!(
            handle.is_some(),
            "列表内容溢出,但静止状态下画不出一条可见的滚动条滑块 —— 用户看\
             不出下面还压着几条会话"
        );
    }

    /// 按文字内容找到它的 `LayoutJob`(要看分段着色,`find_text_pos` 只回位置)。
    /// `editor.rs` 的测试里有同名辅助,但那边是私有的,按项目既有做法各留一份。
    fn find_galley_job(
        shapes: &[egui::epaint::ClippedShape],
        needle: &str,
    ) -> Option<egui::text::LayoutJob> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::text::LayoutJob> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text() == needle => {
                    Some((*ts.galley.job).clone())
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 走查 22 的接线守护:命中的那几个字必须真的被染成 accent。
    /// `highlight::segments` 的单测只证切分对,证不了它被接上了。
    ///
    /// 自证会变红:把 `paint_highlighted` 换回 `p.text(...)` → 名字变成
    /// 单段、没有 accent 色的段。
    #[test]
    fn the_matching_part_of_a_name_is_painted_in_the_accent_color() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "web01", "10.0.0.1", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            search: "eb0".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut run = || {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        &mut ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };
        let _ = run();
        let out = run();

        let job = find_galley_job(&out.shapes, "web01").expect("会话名该画出来了");
        let hit: Vec<&str> = job
            .sections
            .iter()
            .filter(|s| s.format.color == crate::theme::c32(t.accent))
            .map(|s| &job.text[s.byte_range.clone()])
            .collect();
        assert_eq!(hit, vec!["eb0"], "命中的那一段该是 accent 色,其余不该是");
    }

    /// 走查 22:搜不到东西时给一句话和一个出口。一整片空白分不清
    /// 「没有匹配」和「会话都没了」。
    #[test]
    fn an_empty_search_result_says_so_and_offers_a_way_back() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "web01", "10.0.0.1", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            search: "zzz-nothing".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let run = |ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };
        let _ = run(&mut ui_state, egui::RawInput::default());
        let out = run(&mut ui_state, egui::RawInput::default());
        assert!(
            find_text_pos(&out.shapes, "没有匹配的会话").is_some(),
            "搜不到东西时该明说"
        );

        let pos = find_text_pos(&out.shapes, "清空搜索").expect("该有「清空搜索」按钮");
        let click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    click(pos, true),
                    click(pos, false),
                ],
                ..Default::default()
            },
        );
        assert!(
            ui_state.search.is_empty(),
            "点「清空搜索」该真的把搜索框清空"
        );
    }

    /// 走查 22 的「记住上次选中」:`close_session_manager` **不碰**
    /// `editor`/`editor_id`/`search`,所以重新打开时上次选的那条还在。
    ///
    /// 这条属性今天是靠「没写清空代码」成立的,没有任何东西钉住它 ——
    /// 以后谁往 `close_session_manager` 里顺手加一句 `self.editor = None`
    /// (它已经清了 5 个字段,看起来很像该一起清),这个体验就没了,而且
    /// 不会有任何编译错误或别的测试报警。
    #[test]
    fn closing_the_manager_keeps_the_selected_session_so_reopening_lands_where_you_left() {
        let mut st = UiState {
            session_manager_open: true,
            editor_id: Some(SessionId(7)),
            editor: Some(crate::ui::session_manager::EditorBuffer::default()),
            search: "web".into(),
            ..Default::default()
        };
        st.close_session_manager();
        assert_eq!(
            st.editor_id,
            Some(SessionId(7)),
            "关窗不该丢掉上次选中的会话"
        );
        assert!(st.editor.is_some(), "表单缓冲也该留着,否则重开是一片空态");
        assert_eq!(st.search, "web", "搜索词也该留着");
    }

    /// 走查 3 的接线守护:`disambiguate` 算出来的区分信息必须真的画进副标题。
    /// 纯函数测试(`dedupe::tests`)只证判据对,证不了它被接上了 —— 把
    /// `row()` 里那句 `match disambiguate(..)` 改回无条件 `format!("{user}@{host}")`,
    /// 所有 dedupe 单测照样绿,只有这条会红。
    #[test]
    fn two_rows_that_look_identical_get_the_group_name_appended() {
        let groups = vec![
            GroupRecord {
                id: mullion_store::GroupId(1),
                name: "生产环境".into(),
                tags: Vec::new(),
                terminal: Default::default(),
                appearance: Default::default(),
                network: Default::default(),
                automation: Default::default(),
            },
            GroupRecord {
                id: mullion_store::GroupId(2),
                name: "测试环境".into(),
                tags: Vec::new(),
                terminal: Default::default(),
                appearance: Default::default(),
                network: Default::default(),
                automation: Default::default(),
            },
        ];
        let mut a = rec(1, "web01", "10.0.0.1", &[]);
        let mut b = rec(2, "web01", "10.0.0.1", &[]);
        a.identity.group_id = Some(mullion_store::GroupId(1));
        b.identity.group_id = Some(mullion_store::GroupId(2));

        let t = crate::theme::MULLION_DARK;
        let sessions = vec![a, b];
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();
        let mut run = || {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        &mut ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };
        let _ = run();
        let out = run();

        assert!(
            find_text_pos(&out.shapes, "user@10.0.0.1 · 生产环境").is_some(),
            "两行长得一样时,副标题该带上分组名"
        );
        assert!(
            find_text_pos(&out.shapes, "user@10.0.0.1 · 测试环境").is_some(),
            "另一行也要带 —— 只标一行的话,没标的那行看起来像「正常的那条」"
        );
    }

    /// 走查 3:右键菜单要给出**普通连接**和**移动到分组**两条直路。
    /// 原来菜单里只有「连接(跳过自动化)」,主操作反而不在;改分组只能进右栏
    /// 编辑器改下拉再保存。手法同上:真实指针事件走完整条路径,不手动赋值。
    ///
    /// 自证会变红:把「连接」按钮那个分支删掉 → `find_text_pos` 找不到该按钮,
    /// `expect` 直接 panic;把 `move_to_group = Some(...)` 改成不赋值 → 末尾
    /// 断言失败。
    #[test]
    fn right_click_offers_connect_and_move_to_group() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "session-ctx-menu-target", "192.0.2.11", &[])];
        let groups = vec![GroupRecord {
            id: mullion_store::GroupId(7),
            name: "生产环境".into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }];
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };
        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let click_at = |ctx: &egui::Context, ui_state: &mut UiState, pos| {
            run(
                ctx,
                ui_state,
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        primary_click(pos, true),
                        primary_click(pos, false),
                    ],
                    ..Default::default()
                },
            )
        };

        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let row_pos =
            find_text_pos(&out.shapes, "session-ctx-menu-target").expect("这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);
        let open_menu = |ctx: &egui::Context, ui_state: &mut UiState| {
            let _ = run(
                ctx,
                ui_state,
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(row_click_pos),
                        secondary_click(row_click_pos, true),
                        secondary_click(row_click_pos, false),
                    ],
                    ..Default::default()
                },
            );
            // 菜单是 `Area`,首帧先做一趟不可见的 sizing pass。
            run(ctx, ui_state, egui::RawInput::default())
        };

        // ---- 「移动到分组 ▸ 生产环境」----
        let out = open_menu(&ctx, &mut ui_state);
        let move_pos =
            find_text_pos(&out.shapes, "移动到分组").expect("菜单里应该有「移动到分组」");
        let _ = click_at(&ctx, &mut ui_state, move_pos);
        // 子菜单同样要多跑一帧才画得出来。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let group_pos =
            find_text_pos(&out.shapes, "生产环境").expect("子菜单里应该列出了「生产环境」");
        let _ = click_at(&ctx, &mut ui_state, group_pos);
        assert_eq!(
            ui_state.move_to_group,
            Some((SessionId(1), Some(mullion_store::GroupId(7)))),
            "点子菜单里的分组名必须落下移动意图,否则点了没反应"
        );
        assert!(
            ui_state.connect_request.is_none(),
            "移动分组不该顺带发起连接"
        );

        // ---- 「连接」----
        let out = open_menu(&ctx, &mut ui_state);
        let connect_pos = find_text_pos(&out.shapes, "连接").expect("菜单里应该有「连接」");
        let _ = click_at(&ctx, &mut ui_state, connect_pos);
        assert_eq!(
            ui_state.connect_request,
            Some(SessionId(1)),
            "点「连接」必须设 connect_request"
        );
        assert!(
            !ui_state.connect_skip_automation,
            "普通「连接」不该跳过自动化——那是另一条菜单项的语义"
        );
    }

    /// 复核指出的边界洞:「旧目标 X 这一帧没渲染」+「同一帧内另一行 Z 被
    /// 右键新设了 `pending_delete`」同时发生时,不能把 Z 刚写下的新值当成
    /// 「目标未渲染」误删。构造方法:搜索词从头到尾固定成只命中 session-a,
    /// 让 X(session-b)自始至终都不被渲染(避免菜单弹窗跟着行位置重新计算——
    /// 我第一版让搜索词中途变化,结果菜单挪了位置,用旧坐标点击落空,那是
    /// 另一个问题,不是这里要测的东西);先用真实右键在 session-a(Z)上打开
    /// 删除确认菜单,再直接把 `ui_state.pending_delete` 预置成
    /// `Some(session-b)`(相当于「进这一帧之前,待确认删除的目标就已经是
    /// 那个此刻并不渲染的 X」,是搭建前置状态,不是绕过注入点),最后在
    /// **同一帧**里真的点掉 session-a 菜单里的「删除」——`ui_state.pending_delete`
    /// 会在这一帧的渲染过程中被改写成 `Some(session-a)`。这样帧尾清空逻辑
    /// 看到的就是「target=X 这一帧没渲染」+「当前值已经不是 X」同时成立,
    /// 真实复现了这条边界。
    ///
    /// 自证会变红:把 `show()` 里新加的
    /// `ui_state.pending_delete == pending_delete_target` 这层判定去掉(退回
    /// 只看 `is_some() && !rendered` 就清空),这条立刻报 `pending_delete`
    /// 变成了 `None`,而不是 `Some(SessionId(1))`——Z 刚写下的新值被 X 的
    /// 清空逻辑误删了。
    #[test]
    fn pending_delete_newly_set_this_frame_is_not_erased_by_a_different_stale_target_hiding() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![
            rec(1, "session-a-unique-name", "192.0.2.10", &[]),
            rec(2, "session-b-unique-name", "192.0.2.20", &[]),
        ];
        let groups: Vec<GroupRecord> = Vec::new();
        // 搜索词固定只命中 session-a:X(session-b)整场测试都不会被渲染,
        // 布局不会因为搜索词变化而中途改变,菜单弹窗的位置全程稳定。
        let mut ui_state = UiState {
            search: "session-a".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        ui_state,
                        &sessions,
                        &groups,
                        &[],
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        mullion_store::Protocol::Ssh,
                    );
                });
            })
        };

        // 前两帧只是让布局稳定下来。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        // 在 session-a(Z)上右键,打开它的删除确认菜单。
        let row_pos = find_text_pos(&out.shapes, "session-a-unique-name")
            .expect("session-a 这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);
        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );
        // 菜单要多等一帧才画出「删除」文字(Area 首次出现有一趟不可见的
        // sizing pass,见前一条测试同样的说明)。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let delete_btn_pos =
            find_text_pos(&out.shapes, "删除").expect("右键打开的菜单里应该画出了「删除」按钮");

        // 搭建前置状态:进关键帧之前,待确认删除的目标是 X(session-b)——
        // 它整场都没被渲染过,`pending_delete_target` 这一帧会捕获到这个值。
        ui_state.pending_delete = Some(SessionId(2));

        // 关键帧:X 这一帧仍不渲染(搜索词没变),同时真的点掉 session-a
        // 菜单里的「删除」——两件事在这一帧里同时成立。
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(delete_btn_pos),
                    primary_click(delete_btn_pos, true),
                    primary_click(delete_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.pending_delete,
            Some(SessionId(1)),
            "session-b(X)这一帧没有渲染、同时 session-a(Z)在这同一帧被右键\
             确认删除——Z 刚写下的新值不该被『X 未渲染』的清空逻辑误删"
        );
    }

    /// 数一帧里画出来的图形总数。同 `badge.rs::tests::count_shapes` 的手法:
    /// 竖条和图标都是 painter 直接画的,没有 widget、没有 Response 可以反查。
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

    fn cache_with(
        color: Option<(&str, Vec<mullion_store::ColorTarget>)>,
    ) -> crate::ui::badge::AppearanceCache {
        let mut sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        if let Some((hex, apply_to)) = color {
            sessions[0].appearance = mullion_store::AppearancePrefs {
                icon: None,
                color: Some(mullion_store::ColorSpec {
                    hex: hex.to_string(),
                    apply_to,
                }),
            };
        }
        let mut c = crate::ui::badge::AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        c
    }

    /// 用给定的会话集渲染一次左栏。`run_list` 只能换外观缓存,换不了会话本身。
    ///
    /// F118 之后左栏按 `protocol` 分页,调用方必须传入跟 `sessions` 里记录
    /// 相符的协议 —— 否则记录会被 `on_page` 判定为不在这一页,渲染不出来。
    fn run_list_with(
        sessions: &[SessionRecord],
        protocol: mullion_store::Protocol,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();
        ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    &mut ui_state,
                    sessions,
                    &groups,
                    &[],
                    &[],
                    &crate::ui::badge::AppearanceCache::default(),
                    protocol,
                );
            });
        })
    }

    fn run_list(appearance: &crate::ui::badge::AppearanceCache) -> usize {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    &[],
                    &[],
                    appearance,
                    mullion_store::Protocol::Ssh,
                );
            });
        });
        count_shapes(&out.shapes)
    }

    /// F62:勾了「会话列表」的会话,行上要多画一条竖色条。
    #[test]
    fn list_row_paints_an_edge_bar_when_apply_to_includes_list_item() {
        use mullion_store::ColorTarget;
        let none = run_list(&cache_with(None));
        let with = run_list(&cache_with(Some(("#e06767", vec![ColorTarget::ListItem]))));
        assert!(
            with > none,
            "勾了「会话列表」的会话应该多画一条竖色条(无色 {none} 个图形,有色 {with} 个)"
        );
    }

    /// 没勾「会话列表」就不画——`apply_to` 说了算,不是「设了色就到处画」。
    #[test]
    fn list_row_paints_nothing_when_apply_to_excludes_list_item() {
        use mullion_store::ColorTarget;
        let none = run_list(&cache_with(None));
        let other = run_list(&cache_with(Some((
            "#e06767",
            vec![ColorTarget::PaneTitle, ColorTarget::StatusBar],
        ))));
        assert_eq!(
            other, none,
            "只勾了 pane 标题条/状态栏的会话,不该在列表行上画竖色条"
        );
    }

    /// 收集本帧画出来的所有文本。`find_text_pos` 只答「有没有」,这里要的是
    /// 「有哪些」—— 断言失败时能把实际画了什么一并打出来。
    fn drawn_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => out.push(t.galley.job.text.clone()),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut out));
        out
    }

    /// 数一帧里画了几个圆。状态点是左栏唯一的圆形图元 —— 图标走
    /// `Shape::Image`、背景/竖条/按钮走 `Shape::Rect`、文字走 `Shape::Text`、
    /// 分组头的三角走 `Shape::Path`。所以「圆的个数」就是「状态点的个数」。
    fn circle_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Circle(_) => 1,
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    /// 按指定的左栏宽度渲染一次列表。`Frame::none()` 是必需的:`CentralPanel`
    /// 默认带 8px 内边距,不清掉的话 `available_width` 比给的宽度小一圈,
    /// 测出来的档位跟 `density_for(width)` 对不上。
    fn run_list_at(
        width: f32,
        sessions: &[SessionRecord],
        appearance: &crate::ui::badge::AppearanceCache,
    ) -> egui::FullOutput {
        run_list_selecting(width, sessions, appearance, None)
    }

    /// 同上,但可以指定哪一条处于选中态(`UiState::editor_id`)。
    fn run_list_selecting(
        width: f32,
        sessions: &[SessionRecord],
        appearance: &crate::ui::badge::AppearanceCache,
        selected: Option<SessionId>,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            editor_id: selected,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        show(
                            ui,
                            &t,
                            &mut ui_state,
                            sessions,
                            &groups,
                            &[],
                            &[],
                            appearance,
                            mullion_store::Protocol::Ssh,
                        );
                    });
            },
        )
    }

    /// 数一帧里画了几个矩形。
    fn rect_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Rect(_) => 1,
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    /// 行背景的四条规矩,一次钉死:
    /// 1. 普通态透明(`None`)
    /// 2. 有节点色时,选中/悬停都由它主导
    /// 3. 选中比悬停**更靠近**节点色(否则两态分不出来)
    /// 4. 没节点色时回落到改动前的 `sunken_bg` / `panel_head`
    ///
    /// 抽成纯函数才测得了:混在 `session_row` 里的话,「选中背景到底是什么色」
    /// 只能靠数图元反推,测了也不知道测的是不是那一块。
    ///
    /// 自证会变红:让 `row_bg` 忽略 `node` 直接返回 `sunken_bg`(第 2、3 段炸)。
    #[test]
    fn row_background_is_driven_by_the_node_colour_with_a_theme_fallback() {
        let t = &crate::theme::MULLION_DARK;
        let node = crate::theme::parse_hex("#e06767").unwrap();

        assert_eq!(row_bg(false, false, None, t), None, "普通态必须透明");
        assert_eq!(
            row_bg(false, false, Some(node), t),
            None,
            "没选中也没悬停时,配了颜色也不铺背景"
        );

        let sel = row_bg(true, false, Some(node), t).expect("选中态要有背景");
        let hov = row_bg(false, true, Some(node), t).expect("悬停态要有背景");
        assert_ne!(sel, crate::theme::c32(t.sunken_bg), "配了色就不该还是灰底");

        // 选中比悬停更靠近节点色:用「与 panel_bg 的距离」当单调性代理。
        let dist = |c: egui::Color32| {
            let b = crate::theme::c32(t.panel_bg);
            (c.r() as i32 - b.r() as i32).abs()
                + (c.g() as i32 - b.g() as i32).abs()
                + (c.b() as i32 - b.b() as i32).abs()
        };
        assert!(
            dist(sel) > dist(hov),
            "选中({sel:?})必须比悬停({hov:?})更浓"
        );

        assert_eq!(
            row_bg(true, false, None, t),
            Some(crate::theme::c32(t.sunken_bg)),
            "没配色的选中行保持改动前的样子"
        );
        assert_eq!(
            row_bg(false, true, None, t),
            Some(crate::theme::c32(t.panel_head)),
            "没配色的悬停行保持改动前的样子"
        );
    }

    /// 8 个预设色板铺成选中背景之后,`fg` 白字仍要读得出来。这正是选低透明度
    /// 混色而不是纯色铺满的理由 —— 纯色铺满时「黄」上的白字会掉到 1.5:1。
    ///
    /// 阈值取 WCAG AA 正文 4.5:1。
    ///
    /// 自证会变红:把 `SELECTED_ALPHA` 提到 0.9。
    #[test]
    fn every_preset_colour_keeps_the_row_text_readable_when_selected() {
        let t = &crate::theme::MULLION_DARK;
        for (name, hex, _) in crate::theme::LABEL_PALETTE {
            let node = crate::theme::parse_hex(hex).unwrap();
            let bg = row_bg(true, false, Some(node), t).unwrap();
            let bg_rgb = mullion_term::snapshot::Rgb::new(bg.r(), bg.g(), bg.b());
            let ratio = crate::theme::contrast_ratio(t.fg, bg_rgb);
            assert!(
                ratio >= 4.5,
                "预设色「{name}」({hex})铺成选中底后,fg 对比度只有 {ratio:.2}:1"
            );
        }
    }

    /// 选中态只多画**一块**背景 —— 那条左侧 3px 强调条已经删了。
    ///
    /// 数矩形而不是比颜色:强调条和背景都是 `Shape::Rect`,差值为 1 说明只多了
    /// 背景那一块,为 2 说明强调条还在。`row_bg` 的单测管不到这件事(它只回答
    /// 「背景是什么色」,回答不了「除了背景还画了什么」)。
    ///
    /// 自证会变红:把 `session_row` 里那段 accent `rect_filled` 加回去。
    #[test]
    fn selecting_a_row_adds_only_a_background_no_accent_bar() {
        let sessions = vec![with_icon(rec(1, "dev-box", "192.0.2.10", &[]))];
        let cache = cache_of(&sessions);
        let id = sessions[0].id;
        let plain =
            rect_count(&run_list_selecting(super::super::LIST_W, &sessions, &cache, None).shapes);
        let sel = rect_count(
            &run_list_selecting(super::super::LIST_W, &sessions, &cache, Some(id)).shapes,
        );
        assert_eq!(
            sel - plain,
            1,
            "选中只该多画背景一块,实际多了 {}",
            sel - plain
        );
    }

    /// 会话行不再画连接状态点(v0.1.28)。连带那块 12×12 的 hover 热区也没了 ——
    /// 点没了还留着浮层,等于在空白处埋一个看不见的提示。
    ///
    /// 代价是明确接受的:列表从此看不出哪台连上了,连接状态归 pane 标题条管。
    ///
    /// 自证会变红:把 `paint_status` 那次调用加回 `paint_row_body`。
    #[test]
    fn session_rows_no_longer_paint_a_connection_status_dot() {
        let sessions = vec![
            with_icon(rec(1, "dev-box", "192.0.2.10", &[])),
            rec(2, "prod-box", "192.0.2.11", &[]),
        ];
        let cache = cache_of(&sessions);
        let out = run_list_at(super::super::LIST_W, &sessions, &cache);
        assert_eq!(
            circle_count(&out.shapes),
            0,
            "左栏里不该再有圆形图元(状态点是这里唯一会画圆的东西)"
        );
    }

    /// 给一条会话挂上真图标(走生产代码那条归一化路径)。
    fn with_icon(mut r: SessionRecord) -> SessionRecord {
        let px: Vec<u8> = std::iter::repeat_n([7u8, 8, 9, 255], 32 * 32)
            .flatten()
            .collect();
        let img = ico::IconImage::from_rgba_data(32, 32, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        r.appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&raw).unwrap(),
            bg: None,
        });
        r
    }

    fn cache_of(sessions: &[SessionRecord]) -> crate::ui::badge::AppearanceCache {
        let mut c = crate::ui::badge::AppearanceCache::default();
        c.rebuild(sessions, &[]);
        c
    }

    /// 给 `with_icon` 的会话再叠一层节点色,`apply_to` 由调用方给。
    ///
    /// 图标底色这条路径复核前**从没有测试同时设过 icon 和 color**——`cache_with`
    /// 只设 color、`with_icon` 只设 icon,图标那次 `should_paint(appearance,
    /// ColorTarget::ListItem)` 调用永远拿到 `appearance.icon.is_some() == false`
    /// 或 `appearance.color.is_none()` 的组合,根本走不到「垫底色」分支。
    fn with_icon_and_color(
        r: SessionRecord,
        hex: &str,
        apply_to: Vec<mullion_store::ColorTarget>,
    ) -> SessionRecord {
        let mut r = with_icon(r);
        r.appearance.color = Some(mullion_store::ColorSpec {
            hex: hex.to_string(),
            apply_to,
        });
        r
    }

    /// 数一帧里「填了指定颜色、且宽高相等(方形)」的矩形个数。
    ///
    /// 图标底色和右侧 edge bar 用的是**同一个** `should_paint(appearance,
    /// ColorTarget::ListItem)` 调用结果,颜色完全一样,数总数分不清谁多画了
    /// 谁——但形状不一样:edge bar 是 `EDGE_BAR_W`(3px)宽、行高那么高的
    /// 竖条,图标底色是 `icon_px` x `icon_px` 的正方形。按「方形」过滤就把
    /// 两者拆开了。
    fn square_fill_count(shapes: &[egui::epaint::ClippedShape], color: egui::Color32) -> usize {
        fn walk(s: &egui::Shape, color: egui::Color32) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(|s| walk(s, color)).sum(),
                egui::Shape::Rect(r)
                    if r.fill == color && (r.rect.width() - r.rect.height()).abs() < 0.5 =>
                {
                    1
                }
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape, color)).sum()
    }

    /// F61/F62 复核挖出的真缺口:图标底色必须过 `ColorTarget::ListItem` 这道
    /// 闸门,而不是随手用了别的落点——`list_row_paints_an_edge_bar_...` 只
    /// 覆盖了 edge bar 那次 `should_paint` 调用,`paint_row_body` 里画图标的
    /// 那次调用此前完全没有测试盯着(复核实测:把它改成 `ColorTarget::
    /// PaneTitle`,544 个测试全绿无一变红)。
    ///
    /// 自证会变红:把 `paint_row_body` 里画图标那次 `should_paint` 调用的
    /// `ColorTarget::ListItem` 改成 `ColorTarget::PaneTitle`(edge bar 那次不动)。
    #[test]
    fn icon_backdrop_uses_the_list_item_target_not_pane_title() {
        use mullion_store::ColorTarget;
        let color = egui::Color32::from_rgb(0x1e, 0x88, 0xe5);

        let pane_title_only = vec![with_icon_and_color(
            rec(1, "dev-box", "192.0.2.10", &[]),
            "#1e88e5",
            vec![ColorTarget::PaneTitle],
        )];
        let cache_pt = cache_of(&pane_title_only);
        let baseline = square_fill_count(
            &run_list_at(super::super::LIST_W, &pane_title_only, &cache_pt).shapes,
            color,
        );
        assert_eq!(
            baseline, 0,
            "只勾了「pane 标题条」的会话,不该在列表行的图标下垫这个颜色的方块"
        );

        let list_item = vec![with_icon_and_color(
            rec(1, "dev-box", "192.0.2.10", &[]),
            "#1e88e5",
            vec![ColorTarget::ListItem],
        )];
        let cache_li = cache_of(&list_item);
        let with_bg = square_fill_count(
            &run_list_at(super::super::LIST_W, &list_item, &cache_li).shapes,
            color,
        );
        assert_eq!(
            with_bg, 1,
            "勾了「会话列表」的会话,图标下应该恰好垫一块这个颜色的方块"
        );
    }

    /// 三档必须**单调**:越拖越窄只能越走越简,不能来回跳。
    ///
    /// 顺带钉死两件事:拖到下限 `LIST_MIN_W` 落在 `Icons` 档(否则最窄那一档
    /// 根本拖不到,白做),默认宽 `LIST_W` 落在 `Full` 档(否则一打开会话
    /// 管理器就是残缺的样子)。
    #[test]
    fn narrowing_the_list_only_ever_simplifies_it() {
        use super::super::{LIST_MIN_W, LIST_W};
        let rank = |d| match d {
            Density::Icons => 0,
            Density::Compact => 1,
            Density::Full => 2,
        };
        let mut prev = rank(density_for(LIST_MIN_W));
        let mut w = LIST_MIN_W;
        while w <= 480.0 {
            let cur = rank(density_for(w));
            assert!(cur >= prev, "宽度涨到 {w} 反而退了一档");
            prev = cur;
            w += 1.0;
        }
        assert_eq!(
            density_for(LIST_MIN_W),
            Density::Icons,
            "拖到下限也进不了纯图标档的话,那一档等于不存在"
        );
        assert_eq!(
            density_for(LIST_W),
            Density::Full,
            "默认宽度必须是完整档 —— 一打开就是残缺样子,谁也不会去拖它"
        );
        // 三档都要真的能拖到:阈值之间至少各留一格。
        assert_eq!(density_for(ICONS_BELOW), Density::Compact);
        assert_eq!(density_for(COMPACT_BELOW), Density::Full);
    }

    /// 三档图标统一 32px。行高必须真的装得下它,而且上下留白要够 ——
    /// 只断言 `row_h >= icon_px` 是不够的:`Full` 行高退回 44 时 44 > 32 仍然
    /// 过,而那正是要防的(32px 图标在 44 行高里上下只剩 6px,挤得发闷)。
    ///
    /// `Compact` 的留白阈值故意更松(4px):它就是「省地方」的那一档,
    /// 单行文字 + 40 行高是它存在的意义。
    ///
    /// 自证会变红:把 `icon_px` 任意一档改回 16 或 `ico::LARGE`(第一段炸);
    /// 把 `row_h(Full)` 改回 44(第三段炸)。
    #[test]
    fn every_step_uses_the_32px_frame_and_the_row_fits_it() {
        use super::super::LIST_MIN_W;
        // `Icons` 档存在的前提:阈值必须严格大于左栏能拖到的下限,否则
        // `density_for` 永远落不到它。编译期钉死 —— 这两个数分处两个文件,
        // 靠人记住迟早出事。(写法与 `mod.rs` 里那条宽度联立断言同源。)
        const { assert!(LIST_MIN_W < ICONS_BELOW) };
        for d in [Density::Full, Density::Compact, Density::Icons] {
            assert_eq!(
                icon_px(d),
                crate::ui::ico::SMALL as f32,
                "{d:?} 档该用 32px 那一帧"
            );
        }
        assert!(
            LIST_MIN_W >= icon_px(Density::Icons),
            "左栏下限 {LIST_MIN_W} 横着装不下 {}px 图标",
            icon_px(Density::Icons)
        );
        for (d, min_pad) in [
            (Density::Full, 8.0f32),
            (Density::Icons, 8.0),
            (Density::Compact, 4.0),
        ] {
            let pad = (row_h(d) - icon_px(d)) / 2.0;
            assert!(
                pad >= min_pad,
                "{d:?} 档行高 {} 减掉 {}px 图标后上下各只剩 {pad}px",
                row_h(d),
                icon_px(d)
            );
        }
    }

    /// 状态点下线后图标左移贴边。三档共用同一个槽位中心,文字左界才不会
    /// 随档位跳 —— 那是 `text_x` 这个函数存在的全部理由。
    ///
    /// 自证会变红:把 `ICON_SLOT_X` 改回 38。
    #[test]
    fn the_icon_hugs_the_left_edge_now_that_the_status_dot_is_gone() {
        assert_eq!(
            ICON_SLOT_X - icon_px(Density::Full) / 2.0,
            8.0,
            "图标左边距应当是 8px,与行高上下留白同数"
        );
        assert_eq!(text_x(Density::Full), 48.0);
        assert_eq!(
            text_x(Density::Compact),
            text_x(Density::Full),
            "两个有文字的档必须共用同一条文字左界"
        );
    }

    /// `Full` 档两行文字要在 48 行高里上下居中。行高从 44 涨到 48 时最容易
    /// 漏改这两个基线常量,漏了的现象是两行字整体贴在行的上半部分。
    ///
    /// 这是个**代理断言**:真正的文字包围盒要排版才知道,这里用「名称上沿到
    /// 行顶」对「副标题下沿到行底」的差值当近似,±1px 以内算居中。
    ///
    /// 自证会变红:把 `NAME_TOP` 改回 7.0。
    #[test]
    fn the_two_text_lines_sit_vertically_centred_in_the_full_row() {
        let top_gap = NAME_TOP;
        let bottom_gap = row_h(Density::Full) - (SUB_TOP + SUB_FONT_PX);
        assert!(
            (top_gap - bottom_gap).abs() <= 1.0,
            "上留白 {top_gap} 与下留白 {bottom_gap} 差太多"
        );
    }

    /// 纯图标档只认图标:没设图标的会话整条藏掉,**并且当场说清藏了几条**。
    ///
    /// 少画几行而不吭声,用户看到的是「我的会话没了」—— 那比不做这个档更糟。
    ///
    /// 自证会变红:把 `show()` 里那个 `d == Density::Icons` 的过滤删掉
    /// (第一段炸);把末尾 `if hidden > 0` 那块删掉(第二段炸)。
    #[test]
    fn the_icon_only_step_hides_iconless_rows_but_says_how_many() {
        let sessions = vec![
            with_icon(rec(1, "有图标", "192.0.2.10", &[])),
            rec(2, "没图标", "192.0.2.11", &[]),
            rec(3, "也没有", "192.0.2.12", &[]),
        ];
        let cache = cache_of(&sessions);
        let out = run_list_at(super::super::LIST_MIN_W, &sessions, &cache);
        let texts = drawn_text(&out.shapes);
        assert!(
            !texts.iter().any(|s| s.contains("没图标")),
            "纯图标档不该画没设图标的行,实际画了:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("+2")),
            "藏了 2 条必须如实说一声,实际画的是:{texts:?}"
        );

        // 完整档下一条都不藏,也就没有那句提示。
        let wide = drawn_text(&run_list_at(super::super::LIST_W, &sessions, &cache).shapes);
        assert!(
            wide.iter().any(|s| s.contains("没图标")),
            "完整档下不该藏任何行"
        );
        assert!(
            !wide.iter().any(|s| s.contains("+2")),
            "完整档下什么都没藏,不该冒出「藏了几条」的提示"
        );
    }

    /// 窄档把文字换成图标,不是把文字换成空白:`Compact` 还留着名字、
    /// `Icons` 一个字都不留(名字改由 tooltip 兜底)。
    ///
    /// 自证会变红:把 `paint_row_body` 里 `d == Density::Icons` 那个提前
    /// return 删掉 —— 名字会重新画出来,第二段断言炸。
    #[test]
    fn each_step_drops_exactly_one_layer_of_text() {
        let sessions = vec![with_icon(rec(1, "dev-box", "192.0.2.10", &[]))];
        let cache = cache_of(&sessions);
        let has = |w: f32, needle: &str| {
            drawn_text(&run_list_at(w, &sessions, &cache).shapes)
                .iter()
                .any(|s| s.contains(needle))
        };
        // 完整档:名称 + user@host 都在。
        assert!(has(super::super::LIST_W, "dev-box"), "完整档要有名称");
        assert!(has(super::super::LIST_W, "192.0.2.10"), "完整档要有副标题");
        // 紧凑档:只剩名称。
        assert!(has(ICONS_BELOW, "dev-box"), "紧凑档要保住名称");
        assert!(!has(ICONS_BELOW, "192.0.2.10"), "紧凑档该把副标题让给图标");
        // 纯图标档:一个字都不留。
        assert!(
            !has(super::super::LIST_MIN_W, "dev-box"),
            "纯图标档不该有名称"
        );
    }
}
