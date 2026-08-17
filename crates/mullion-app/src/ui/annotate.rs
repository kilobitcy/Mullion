//! F100 标注模式:在 egui 外壳上悬停描边、点选编号,一键导出 Markdown。
//!
//! 用途是**压缩人与 Claude 之间的带宽**。用户看到「那个东西不对」,现在可以
//! `Ctrl+Shift+F` 进标注模式,把有问题的几处点出来,`Ctrl+Shift+E` 导出一段
//! Markdown 粘进 Claude Code —— 里面带着每一处的语义路径、屏幕矩形和**插桩点的
//! 源码位置**,Claude 不需要再猜「你说的那个挤在一起的东西是哪段代码画的」。
//!
//! 参考 `kilobitcy/snapmark`(web 端 UI 注释工具)。它的真实价值不是「截图标注」,
//! 是**把视觉位置映射回代码标识**。这里做的是 egui 侧的等价物。
//!
//! # 为什么注释不在应用内敲
//!
//! 只给编号,用户在 Claude Code 里口述「第 2 个太挤」。中文注释输入要过 winit +
//! 第三方 IME,那是 `CLAUDE.md`「你无法验证的东西」里的一条;而且在应用内做文本框
//! 会撞领域陷阱 T8(判给终端的键盘不得先喂 egui)。
//!
//! # 为什么身份靠 `#[track_caller]` 而不是 egui 的 callstack
//!
//! 原方案是「未标的回退 egui `callstack`」,**在 egui 0.30 上做不到**:
//! `mod callstack`(`lib.rs:445`)和存放捕获结果的 `mod pass_state`(`lib.rs:428`)
//! 都不是 `pub mod`,`Context` 上也没有访问器;`register_widget_info` 整个函数体被
//! `#[cfg(debug_assertions)]` 包住(`context.rs:1349`),release exe 里永不记录;
//! 能枚举全部 widget rect 的 `Context::debug_painting`(`context.rs:2170`)是私有的。
//!
//! `std::panic::Location::caller()` 更好:编译期常量、零运行时开销,给一行而不是
//! 整条栈,而且不需要 `callstack` feature、不需要 release `debug = 1`、**不受
//! `panic = "abort"` 影响** —— 原方案的三条前提一并消失。代价是必须插桩,但
//! **容器级**插桩就够,而「左栏 / 搜索框 / 会话行」正是人说话的粒度。
//!
//! 完整取舍见 `docs/superpowers/plans/2026-08-09-f100-annotate-mode.md`。

use std::panic::Location;

use mullion_term::keymap::{Key, Mods};

use crate::theme::{self, Theme};

/// 标注模式的四个快捷键。判定抽成纯函数(`hotkey`)是为了能单测「`Esc` 只在模式
/// 开着时被吃掉」—— 那一条错了就是「终端里再也按不出 `Esc`」,而这在无头环境
/// 里看不出来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    Toggle,
    Export,
    CycleDetail,
    Exit,
}

/// 这一次按键是不是标注模式的快捷键。`None` = 照旧往下走(编码给终端 / 喂 egui)。
///
/// `Ctrl+Shift+C/V` 已被 F18 占用,这里用 `F`(切换)/`E`(导出)/`D`(换档)。
/// **`Esc` 只在模式开着时才截**:否则终端里永远发不出 `0x1b`,vim / Claude Code
/// 直接残废。
pub fn hotkey(key: Key, mods: Mods, on: bool) -> Option<Hotkey> {
    if on && key == Key::Escape && !mods.ctrl && !mods.alt {
        return Some(Hotkey::Exit);
    }
    if !(mods.ctrl && mods.shift) {
        return None;
    }
    let Key::Char(c) = key else {
        return None;
    };
    match c.to_ascii_lowercase() {
        'f' => Some(Hotkey::Toggle),
        // 导出和换档在模式关着时没有意义,别悄悄吞掉这两个组合键。
        'e' if on => Some(Hotkey::Export),
        'd' if on => Some(Hotkey::CycleDetail),
        _ => None,
    }
}

/// 一处可点选的目标。每帧由散布在 UI 里的 `mark()` 重新登记。
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    /// 语义路径,用 `/` 分层,如 `会话管理器/左栏/会话行[节点 03]`。
    pub path: String,
    pub rect: egui::Rect,
    /// 插桩点的源码位置。`&'static Location` 是编译期常量,不是运行时抓的栈。
    pub src: &'static Location<'static>,
}

/// 已点选的一处。跟 `Spot` 分开是因为它要**跨帧活着**:`spots` 每帧清空重建
/// (下一帧那个 widget 可能压根不画了 —— 用户点完一处会话行就去切 Tab),
/// 所以选中的当下就把要用的东西拷下来,导出不依赖「本帧恰好还在」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub path: String,
    /// 存成字符串而不是 `Rect`:`Rect` 不是 `Eq`(浮点),而判「再点一次取消选中」
    /// 要比较相等。同时这也是导出时直接要用的形态。
    pub rect: String,
    pub src: String,
}

/// 导出的详细度。共识:三档,默认紧凑。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detail {
    /// 只列选中项,一行一条。
    #[default]
    Compact,
    /// 加上本帧一共登记了多少处,以及每条的完整层级。
    Normal,
    /// 连**没选中**的候选一起列 —— 用来问「这一片区域里都有些什么」。
    Full,
}

impl Detail {
    /// 屏上提示条里显示的档名。
    fn name(self) -> &'static str {
        match self {
            Detail::Compact => "紧凑",
            Detail::Normal => "标准",
            Detail::Full => "详细",
        }
    }

    fn next(self) -> Self {
        match self {
            Detail::Compact => Detail::Normal,
            Detail::Normal => Detail::Full,
            Detail::Full => Detail::Compact,
        }
    }
}

/// 导出时要写进开头那行的全局上下文。**不含逐 widget 的样式值**(共识第 3 条:
/// 那些 Claude 自己去读代码更准,写进来只会让人以为它是权威事实)。
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// 窗口逻辑尺寸。
    pub size: egui::Vec2,
    pub ppp: f32,
    /// 主题名。
    pub theme: String,
    /// 当下在看哪个界面,如 `会话管理器(编辑器「基础」页)`。
    pub screen: String,
    /// 补充事实,如 `左栏 280px/Full 档`。一条一个短句。
    pub extra: Vec<String>,
}

// ---------------------------------------------------------------------------
// 状态。挂 `egui::Context` 的 temp data,**不放 `UiState`**。
//
// 理由:插桩点散布在整个 UI 树的深处,那些地方只拿得到 `&Ui` / `&Painter`,
// 拿不到 `&mut UiState`(它的可变借用被外层闭包占着)。`badge.rs` 的纹理缓存
// 出于同一个理由放在同一个地方。
//
// 顺带也说明了为什么标注模式的开关不走 `UiState`:它是「UI 调试态」,不是应用
// 状态,混进 `UiState` 会让每个读 `UiState` 的地方都要多想一层。
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct State {
    on: bool,
    spots: Vec<Spot>,
    picked: Vec<Picked>,
    /// `Ctrl+Shift+E` 置位,`overlay()` 消费。用标志而不是让 `app.rs` 直接生成
    /// Markdown:生成要用本帧的 `spots` 与 `Env`,那些只有 UI 侧手上有。
    export_request: bool,
    /// 生成好、等着 `build_ui` 取走送剪贴板的那一份。
    export_ready: Option<String>,
    /// 导出详细度(`Ctrl+Shift+D` 循环)。默认紧凑。
    detail: Detail,
}

fn id() -> egui::Id {
    egui::Id::new("mullion_annotate")
}

/// `on` 单独存一份,让 `mark()` 在模式关着时只做一次 bool 读就早退 —— 不去
/// clone 整个 `State`。插桩点有几十个,每帧都全量 clone 是白烧 CPU。
fn on_id() -> egui::Id {
    egui::Id::new("mullion_annotate_on")
}

fn with_state<R>(ctx: &egui::Context, f: impl FnOnce(&mut State) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<State>(id())))
}

/// 标注模式开着吗。
pub fn is_on(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(on_id()).unwrap_or(false))
}

/// 切换标注模式(`Ctrl+Shift+F`)。返回切换后的状态。
///
/// 关掉时**清空已选中的**:留着的话下次开启会凭空冒出上次的编号,用户会以为
/// 自己点错了 —— 同 `list.rs` 里 `pending_delete` 那条坑的道理。
pub fn toggle(ctx: &egui::Context) -> bool {
    let on = !is_on(ctx);
    ctx.data_mut(|d| d.insert_temp(on_id(), on));
    with_state(ctx, |st| {
        st.on = on;
        if !on {
            st.picked.clear();
            st.spots.clear();
            st.export_request = false;
        }
    });
    on
}

/// 退出标注模式(`Esc`)。已经关着就什么都不做。
pub fn exit(ctx: &egui::Context) {
    if is_on(ctx) {
        toggle(ctx);
    }
}

/// 请求导出(`Ctrl+Shift+E`)。真正的生成在 `overlay()` 里 —— 它才拿得到本帧的
/// `spots` 和 `Env`。
pub fn request_export(ctx: &egui::Context) {
    with_state(ctx, |st| st.export_request = true);
}

/// 循环切换详细度(`Ctrl+Shift+D`),返回切换后的档。
///
/// 共识第 4 条要三档却没定切换手段;做成循环键而不是三个键,是因为这是个
/// 调试功能,不值得占三个键位,而屏上提示条一直写着当前档,不会切迷路。
pub fn cycle_detail(ctx: &egui::Context) -> Detail {
    with_state(ctx, |st| {
        st.detail = st.detail.next();
        st.detail
    })
}

/// 取走生成好的 Markdown(`build_ui` 调,交给 `app.rs` 送剪贴板)。
pub fn take_export(ctx: &egui::Context) -> Option<String> {
    with_state(ctx, |st| st.export_ready.take())
}

/// 读回本帧登记的语义路径。**给测试与 `examples/ui_shot.rs` 用** —— 插桩铺没铺、
/// 铺在哪儿,在无头环境里没有别的办法看见。
pub fn spot_paths(ctx: &egui::Context) -> Vec<String> {
    with_state(ctx, |st| st.spots.iter().map(|s| s.path.clone()).collect())
}

/// 读回本帧登记的矩形:**先找路径完全相等的**,没有再退回「含 `needle`
/// 的第一处」。
///
/// **给测试用** —— 无头环境里要往某个部件的中心注入点击,硬编码坐标一改
/// 布局就静默打空(点了个空地方,测试却因为别的原因绿着)。
///
/// **为什么不像 `ensure_picked` 那样只做子串匹配**:父路径往往是子路径的
/// 前缀(`标签栏/标签 2` vs `标签栏/标签 2/关闭`),而子控件通常先登记 ——
/// 只做子串匹配的话,查父控件会**静默**拿到子控件那一小块矩形。切片 J 的
/// Task 9 就踩了:想点标签标题,实际点在了 × 上。
///
/// **前提:标注模式必须开着**(`toggle`)。关着时 `mark` 直接 return,
/// 这里必然返回 `None`。
#[cfg(test)]
pub fn spot_rect(ctx: &egui::Context, needle: &str) -> Option<egui::Rect> {
    with_state(ctx, |st| {
        st.spots
            .iter()
            .find(|s| s.path == needle)
            .or_else(|| st.spots.iter().find(|s| s.path.contains(needle)))
            .map(|s| s.rect)
    })
}

/// 确保「路径里含 `needle` 的第一处」处于选中态,返回是否找到。
///
/// **给 `examples/ui_shot.rs` 用**:出图脚本要摆出「已选中两处」的样子,不该
/// 去猜某个控件落在哪个像素上再合成一次点击 —— 那种坐标一改布局就失效,而且
/// 图错了看不出是坐标错了还是 UI 错了。
///
/// 幂等:已经选中就什么都不做,所以可以在每帧的闭包里无脑调(不像 `pick`,
/// 那是「再点一次就取消」的用户语义)。
///
/// **必须在同一帧的 `overlay()` 之前调** —— `overlay` 末尾会清空 `spots`。
pub fn ensure_picked(ctx: &egui::Context, needle: &str) -> bool {
    with_state(ctx, |st| {
        let Some(sp) = st.spots.iter().find(|s| s.path.contains(needle)).cloned() else {
            return false;
        };
        let p = picked_of(&sp);
        if !st.picked.contains(&p) {
            st.picked.push(p);
        }
        true
    })
}

/// 登记一处可标注的目标。**模式关着时只有一次 bool 读的开销。**
///
/// `#[track_caller]` 让 `Location::caller()` 拿到的是**调用这个函数的那一行**,
/// 也就是插桩点本身。这是编译期常量,不是运行时抓栈。
#[track_caller]
pub fn mark(ctx: &egui::Context, path: impl Into<String>, rect: egui::Rect) {
    if !is_on(ctx) {
        return;
    }
    // 退化矩形(还没布局完的 `Rect::NOTHING`、或者被裁成零面积)登记了也点不到,
    // 只会让 hit test 里多几个永远命中不了的候选。
    if !rect.is_positive() {
        return;
    }
    let spot = Spot {
        path: path.into(),
        rect,
        src: Location::caller(),
    };
    with_state(ctx, |st| st.spots.push(spot));
}

/// 源码位置渲染成 `路径:行号`。**保留完整相对路径**(不只留文件名):这段文本是
/// 给 Claude 读的,全路径能直接拿去 `Read`,`list.rs:349` 还得先搜一遍。
fn src_of(loc: &Location<'static>) -> String {
    format!("{}:{}", loc.file(), loc.line())
}

/// 矩形渲染成 `(左,上)-(右,下)`,取整到整数点。半像素小数对人和 Claude 都没有
/// 信息量,只会让每行变长。
fn rect_of(r: egui::Rect) -> String {
    format!(
        "({},{})-({},{})",
        r.left().round() as i32,
        r.top().round() as i32,
        r.right().round() as i32,
        r.bottom().round() as i32
    )
}

/// 指针底下**最具体**的那一处。
///
/// 「最具体」= 面积最小。父容器必然包含子控件,所以面积最小的那个就是用户真正
/// 指着的东西 —— 不这么排的话,点哪儿都会命中最外层那个铺满窗口的容器。
pub fn hit(spots: &[Spot], pos: egui::Pos2) -> Option<&Spot> {
    spots
        .iter()
        .filter(|s| s.rect.contains(pos))
        .min_by(|a, b| {
            let (aa, ba) = (a.rect.area(), b.rect.area());
            aa.partial_cmp(&ba).unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn picked_of(s: &Spot) -> Picked {
    Picked {
        path: s.path.clone(),
        rect: rect_of(s.rect),
        src: src_of(s.src),
    }
}

/// 点一处:没选过就追加(编号 = 追加后的下标 + 1),已经选过就移除。
///
/// 移除而不是忽略:用户点错了得有办法取消,而「再点一次」是最不需要教的做法。
/// 移除之后**后面的编号顺延** —— 这是 `Vec` 的自然行为,也正是屏上徽标会做的事。
pub fn pick(picked: &mut Vec<Picked>, spot: &Spot) {
    let p = picked_of(spot);
    match picked.iter().position(|x| *x == p) {
        Some(i) => {
            picked.remove(i);
        }
        None => picked.push(p),
    }
}

/// 生成导出的 Markdown。**纯函数** —— 这是本模块唯一必须字字对的部分,所以它
/// 不碰 egui 状态,好单测。
pub fn markdown(env: &Env, picked: &[Picked], spots: &[Spot], detail: Detail) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "## Mullion UI 标注 · v{}\n",
        env!("CARGO_PKG_VERSION")
    ));

    let mut facts = vec![
        format!(
            "窗口 {}x{} @{}",
            env.size.x.round() as i32,
            env.size.y.round() as i32,
            env.ppp
        ),
        env.theme.clone(),
    ];
    if !env.screen.is_empty() {
        facts.push(env.screen.clone());
    }
    facts.extend(env.extra.iter().cloned());
    s.push_str(&facts.join(" · "));
    s.push('\n');

    if picked.is_empty() {
        // 一个人按了导出却什么都没选,大概是不知道要先点。别给他一段空白。
        s.push_str("\n(没有选中任何位置。进标注模式后先点几处要说的地方,再导出。)\n");
    } else {
        s.push('\n');
        for (i, p) in picked.iter().enumerate() {
            s.push_str(&format!("{}. {} — {} — {}\n", i + 1, p.path, p.rect, p.src));
        }
    }

    if detail == Detail::Normal || detail == Detail::Full {
        s.push_str(&format!("\n本帧共登记 {} 处可标注位置。\n", spots.len()));
    }
    if detail == Detail::Full {
        s.push_str("\n<details><summary>全部候选</summary>\n\n");
        for sp in spots {
            s.push_str(&format!(
                "- {} — {} — {}\n",
                sp.path,
                rect_of(sp.rect),
                src_of(sp.src)
            ));
        }
        s.push_str("\n</details>\n");
    }
    s
}

/// 画 overlay 并处理点选。**必须在 `build_ui` 的最末调**:它要看的是本帧
/// 所有 `mark()` 登记完之后的 `spots`。
///
/// 顺带把 `spots` 清空,下一帧从头登记。
pub fn overlay(ctx: &egui::Context, t: &Theme, env: &Env) {
    if !is_on(ctx) {
        return;
    }
    let (spots, mut picked, export_request, detail) = with_state(ctx, |st| {
        (
            st.spots.clone(),
            st.picked.clone(),
            std::mem::take(&mut st.export_request),
            st.detail,
        )
    });

    // 铺一层吃掉全部指针输入的顶层 `Area`。**必须吃掉** —— 否则点会话行就真的
    // 切了会话、点「删除」就真的弹了确认框,标注一圈下来应用状态全变了。
    //
    // egui 的命中判定按图层从上往下走,`Order::Foreground` 在 `Order::Middle`
    // (`Window` 所在层)之上,所以这一层注册的可点击矩形会截住下面所有 widget 的
    // 点击。放在 `build_ui` 最末画,同层内也在 toast 之后,不会被谁盖住。
    let screen = ctx.screen_rect();
    egui::Area::new(id().with("overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true)
        // `Area` 默认 `fade_in(true)`,头几帧整层 opacity 接近 0 —— 描边、徽标、
        // 提示条会一起淡到看不见。淡入对一个调试工具没有价值,而按下
        // `Ctrl+Shift+F` 之后「立刻看清自己进了标注模式」正是它要给的反馈。
        // (`ui_shot` 的 annotate 场景第一版就是被这个默认值淡掉的。)
        .fade_in(false)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen, egui::Sense::click());
            let p = ui.painter();

            // 已选中的:描边 + 编号徽标。先画,让悬停描边盖在上面。
            for (i, pk) in picked.iter().enumerate() {
                if let Some(sp) = spots.iter().find(|s| picked_of(s) == *pk) {
                    p.rect_stroke(
                        sp.rect,
                        egui::Rounding::same(3.0),
                        egui::Stroke::new(2.0, theme::c32(t.accent)),
                    );
                    badge(p, t, sp.rect, i + 1);
                }
            }

            let hovered = resp.hover_pos().and_then(|pos| hit(&spots, pos));
            if let Some(sp) = hovered {
                p.rect_stroke(
                    sp.rect,
                    egui::Rounding::same(3.0),
                    egui::Stroke::new(2.0, theme::c32(t.info)),
                );
                // 路径贴在描边框上方(顶到屏幕上沿时翻到下方)—— 不跟着鼠标飘,
                // 用户要照着念这行字,飘着的东西念不了。
                let above = sp.rect.top() - 6.0;
                let (anchor, y) = if above < screen.top() + 18.0 {
                    (egui::Align2::LEFT_TOP, sp.rect.bottom() + 6.0)
                } else {
                    (egui::Align2::LEFT_BOTTOM, above)
                };
                label(p, t, egui::pos2(sp.rect.left(), y), anchor, &sp.path);
            }

            if resp.clicked() {
                if let Some(sp) = resp.interact_pointer_pos().and_then(|pos| hit(&spots, pos)) {
                    pick(&mut picked, sp);
                }
            }

            // 提示条固定在屏幕左下角:标注模式是个「模态」,不写出来用户会忘了
            // 自己还在里面(点什么都没反应,只会以为应用卡了)。
            let tip = format!(
                "标注模式:点选位置 · 已选 {} 处 · Ctrl+Shift+E 导出({}) · Ctrl+Shift+D 换档 · Esc 退出",
                picked.len(),
                detail.name()
            );
            // **不贴屏幕最底边**:那儿是状态栏,而状态栏自己也是要标的对象 ——
            // 提示条压在上面,用户就永远标不到它。往上抬一截,落在中央终端区上;
            // 终端网格没有 widget、本来就不可标注(共识第 1 条),挡住它无代价。
            label(
                p,
                t,
                egui::pos2(screen.left() + 8.0, screen.bottom() - 56.0),
                egui::Align2::LEFT_BOTTOM,
                &tip,
            );
        });

    let ready = export_request.then(|| markdown(env, &picked, &spots, detail));
    with_state(ctx, |st| {
        st.picked = picked;
        st.spots.clear();
        if let Some(md) = ready {
            st.export_ready = Some(md);
        }
    });
}

/// 编号徽标:实心圆 + 白字,钉在矩形左上角外侧。
fn badge(p: &egui::Painter, t: &Theme, rect: egui::Rect, n: usize) {
    let c = egui::pos2(rect.left(), rect.top());
    p.circle_filled(c, 9.0, theme::c32(t.accent));
    p.text(
        c,
        egui::Align2::CENTER_CENTER,
        n.to_string(),
        egui::FontId::proportional(11.0),
        theme::c32(t.accent_fg),
    );
}

/// 带底的一行小字。overlay 的文字全都压在真实 UI 上面,不铺底会跟底下的内容
/// 糊成一团、谁也读不出来。
fn label(p: &egui::Painter, t: &Theme, pos: egui::Pos2, anchor: egui::Align2, text: &str) {
    let galley = p.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.0),
        theme::c32(t.fg),
    );
    let pad = egui::vec2(5.0, 2.0);
    let rect = anchor.anchor_size(pos, galley.size() + pad * 2.0);
    p.rect_filled(rect, egui::Rounding::same(3.0), theme::c32(t.sunken_bg));
    p.galley(rect.min + pad, galley, theme::c32(t.fg));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(path: &str, rect: egui::Rect) -> Spot {
        Spot {
            path: path.into(),
            rect,
            src: Location::caller(),
        }
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    fn ctrl_shift() -> Mods {
        Mods {
            ctrl: true,
            shift: true,
            alt: false,
            sup: false,
        }
    }

    /// **`Esc` 只在标注模式开着时被截住。** 截错了就是终端里永远发不出 `0x1b`,
    /// vim / Claude Code 直接残废 —— 而这个后果在无头环境里看不出来,只能靠这条。
    ///
    /// 自证会变红:把 `hotkey` 里 `on && key == Key::Escape` 的 `on &&` 去掉。
    #[test]
    fn escape_is_only_swallowed_while_the_mode_is_on_so_the_terminal_keeps_getting_it() {
        let bare = Mods::default();
        assert_eq!(hotkey(Key::Escape, bare, true), Some(Hotkey::Exit));
        assert_eq!(
            hotkey(Key::Escape, bare, false),
            None,
            "模式关着时 Esc 必须照旧转发给终端"
        );
    }

    /// F18 的 `Ctrl+Shift+C/V` 不能被标注模式抢走 —— 抢走了就是「标注模式下没法
    /// 复制粘贴」,而且是静默的。
    ///
    /// 自证会变红:把 `hotkey` 的 match 里加一条 `'c' => Some(Hotkey::Toggle)`。
    #[test]
    fn f18_copy_paste_keys_are_left_alone() {
        for c in ['c', 'v'] {
            for on in [false, true] {
                assert_eq!(
                    hotkey(Key::Char(c), ctrl_shift(), on),
                    None,
                    "Ctrl+Shift+{c} 归 F18(on={on})"
                );
            }
        }
    }

    /// 导出/换档在模式关着时不该吞键;切换键两种状态下都要认。
    ///
    /// 自证会变红:把 `'e' if on` 的 guard 去掉。
    #[test]
    fn export_and_detail_keys_do_nothing_while_the_mode_is_off() {
        assert_eq!(
            hotkey(Key::Char('f'), ctrl_shift(), false),
            Some(Hotkey::Toggle)
        );
        assert_eq!(
            hotkey(Key::Char('f'), ctrl_shift(), true),
            Some(Hotkey::Toggle)
        );
        assert_eq!(hotkey(Key::Char('e'), ctrl_shift(), false), None);
        assert_eq!(hotkey(Key::Char('d'), ctrl_shift(), false), None);
        assert_eq!(
            hotkey(Key::Char('e'), ctrl_shift(), true),
            Some(Hotkey::Export)
        );
        assert_eq!(
            hotkey(Key::Char('d'), ctrl_shift(), true),
            Some(Hotkey::CycleDetail)
        );
    }

    /// 少一个修饰键就不该认:裸 `f` 在终端里是要真的发出去的。
    #[test]
    fn plain_letters_are_never_hotkeys() {
        let only_ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(hotkey(Key::Char('f'), Mods::default(), true), None);
        assert_eq!(hotkey(Key::Char('f'), only_ctrl, true), None);
    }

    /// 三档循环必须回到起点,否则换过一圈就再也回不到默认的紧凑档。
    #[test]
    fn detail_cycles_back_to_compact() {
        let d = Detail::default();
        assert_eq!(d, Detail::Compact);
        assert_eq!(d.next(), Detail::Normal);
        assert_eq!(d.next().next(), Detail::Full);
        assert_eq!(d.next().next().next(), Detail::Compact);
    }

    /// 命中必须取**最具体**的那一处。父容器总是包着子控件,按登记顺序或者按
    /// 「第一个命中的」取都会永远命中最外层那个铺满窗口的容器,标注模式直接废掉。
    ///
    /// 自证会变红:把 `hit` 里的 `min_by` 改成 `.next()`(取第一个命中的)。
    #[test]
    fn hit_picks_the_smallest_containing_spot_not_the_outermost_container() {
        let spots = vec![
            spot("窗口", r(0.0, 0.0, 800.0, 600.0)),
            spot("窗口/左栏", r(0.0, 0.0, 280.0, 600.0)),
            spot("窗口/左栏/会话行", r(8.0, 100.0, 264.0, 44.0)),
        ];
        let got = hit(&spots, egui::pos2(100.0, 120.0)).map(|s| s.path.as_str());
        assert_eq!(got, Some("窗口/左栏/会话行"));

        // 落在会话行外面、左栏里面 → 退一层。
        let got = hit(&spots, egui::pos2(100.0, 300.0)).map(|s| s.path.as_str());
        assert_eq!(got, Some("窗口/左栏"));

        // 什么都没命中时不该硬凑一个出来。
        assert!(hit(&spots, egui::pos2(900.0, 900.0)).is_none());
    }

    /// 再点一次同一处 = 取消选中,而且**后面的编号要顺延**。用户点错了必须有
    /// 办法取消;编号不顺延的话屏上会出现 1、3 这种断号,他念给 Claude 的
    /// 「第 2 个」跟导出文本里的第 2 条就不是一回事了。
    ///
    /// 自证会变红:把 `pick` 里 `Some(i) => picked.remove(i)` 改成 `Some(_) => {}`。
    #[test]
    fn clicking_an_already_picked_spot_removes_it_and_renumbers_the_rest() {
        let a = spot("甲", r(0.0, 0.0, 10.0, 10.0));
        let b = spot("乙", r(20.0, 0.0, 10.0, 10.0));
        let c = spot("丙", r(40.0, 0.0, 10.0, 10.0));
        let mut picked = Vec::new();
        pick(&mut picked, &a);
        pick(&mut picked, &b);
        pick(&mut picked, &c);
        assert_eq!(picked.len(), 3);

        pick(&mut picked, &b); // 取消中间那个
        let paths: Vec<&str> = picked.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["甲", "丙"],
            "取消中间一处后,剩下的必须顺延成 1、2 两条"
        );

        pick(&mut picked, &b); // 再点回来 → 追加到末尾
        let paths: Vec<&str> = picked.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, vec!["甲", "丙", "乙"]);
    }

    /// 同一个插桩点画出来的**不同**行必须能分别选中 —— 会话列表每一行都走同一次
    /// `mark()` 调用,`src` 完全一样,只有 `path` 和 `rect` 不同。只按 `src` 判重
    /// 会导致「选了第一行,第二行就点不动了」。
    ///
    /// 自证会变红:把 `Picked` 的判等改成只比 `src`。
    #[test]
    fn two_rows_from_the_same_instrumentation_site_are_independently_selectable() {
        let row1 = spot("列表/会话行[甲]", r(0.0, 0.0, 100.0, 44.0));
        let row2 = spot("列表/会话行[乙]", r(0.0, 44.0, 100.0, 44.0));
        // 同一个 helper 造的,`src` 必然相同 —— 这正是要防的场景。
        assert_eq!(src_of(row1.src), src_of(row2.src));

        let mut picked = Vec::new();
        pick(&mut picked, &row1);
        pick(&mut picked, &row2);
        assert_eq!(picked.len(), 2, "同一插桩点的两行必须能各自选中");
    }

    fn env() -> Env {
        Env {
            size: egui::vec2(1280.0, 800.0),
            ppp: 1.0,
            theme: "mullion-dark".into(),
            screen: "会话管理器".into(),
            extra: vec!["左栏 280px/Full 档".into()],
        }
    }

    /// 紧凑档:一条一行,带路径、矩形、源码位置。**源码位置是整个功能的价值所在**
    /// —— 没有它,这段文本就退化成「有个东西在 (45,118) 不好看」,Claude 还得
    /// 自己猜是哪段代码画的。
    ///
    /// 自证会变红:把 `markdown` 里那行 format 的 `{}` — `p.src` 去掉。
    #[test]
    fn compact_export_carries_path_rect_and_source_location_for_each_pick() {
        let s = spot("会话管理器/左栏/会话行", r(45.0, 118.0, 262.0, 44.0));
        let mut picked = Vec::new();
        pick(&mut picked, &s);
        let md = markdown(&env(), &picked, std::slice::from_ref(&s), Detail::Compact);

        assert!(
            md.contains("1. 会话管理器/左栏/会话行"),
            "缺编号或路径:{md}"
        );
        assert!(md.contains("(45,118)-(307,162)"), "缺屏幕矩形:{md}");
        assert!(
            md.contains(&src_of(s.src)),
            "缺插桩点源码位置 —— 没有它 Claude 还得自己猜是哪段代码:{md}"
        );
        // 全局上下文那一行:窗口尺寸/缩放/主题/界面/密度档。
        assert!(md.contains("窗口 1280x800 @1"), "缺窗口尺寸:{md}");
        assert!(md.contains("mullion-dark"), "缺主题名:{md}");
        assert!(md.contains("左栏 280px/Full 档"), "缺补充事实:{md}");
        // 紧凑档不该把没选中的候选也倒出来。
        assert!(!md.contains("全部候选"), "紧凑档不该列全部候选:{md}");
    }

    /// 详细档才列未选中的候选。三档必须真的不一样 —— 否则「三档」是假的。
    #[test]
    fn the_three_detail_levels_actually_differ() {
        let a = spot("甲", r(0.0, 0.0, 10.0, 10.0));
        let b = spot("乙", r(20.0, 0.0, 10.0, 10.0));
        let mut picked = Vec::new();
        pick(&mut picked, &a);
        let spots = vec![a, b];

        let compact = markdown(&env(), &picked, &spots, Detail::Compact);
        let normal = markdown(&env(), &picked, &spots, Detail::Normal);
        let full = markdown(&env(), &picked, &spots, Detail::Full);

        assert!(!compact.contains("本帧共登记"));
        assert!(normal.contains("本帧共登记 2 处"));
        assert!(!normal.contains("全部候选"));
        assert!(full.contains("全部候选"));
        assert!(full.contains("乙"), "详细档要列出没选中的候选");
        assert_ne!(compact, normal);
        assert_ne!(normal, full);
    }

    /// 什么都没选就按导出,得给一句人话,不能吐一段只有标题的空白。
    #[test]
    fn exporting_with_nothing_picked_explains_what_to_do() {
        let md = markdown(&env(), &[], &[], Detail::Compact);
        assert!(md.contains("没有选中任何位置"), "{md}");
    }

    /// 模式关着时 `mark()` 必须一处都不登记 —— 它散在整个 UI 树里,每帧都要跑,
    /// 关着还干活就是白烧 CPU(N3 红线的邻居)。
    ///
    /// 自证会变红:把 `mark()` 开头那个 `if !is_on(ctx) { return; }` 删掉。
    #[test]
    fn mark_is_a_no_op_while_the_mode_is_off() {
        let ctx = egui::Context::default();
        mark(&ctx, "甲", r(0.0, 0.0, 10.0, 10.0));
        let n = with_state(&ctx, |st| st.spots.len());
        assert_eq!(n, 0, "标注模式关着时不该登记任何候选");

        toggle(&ctx);
        mark(&ctx, "甲", r(0.0, 0.0, 10.0, 10.0));
        let n = with_state(&ctx, |st| st.spots.len());
        assert_eq!(n, 1, "开着时必须登记");
    }

    /// 退化矩形登记了也永远点不到,只会在 hit test 里当噪音。
    /// (`Rect::NOTHING` 是 egui 给还没布局完的 `Ui` 的初始值,真会传进来。)
    #[test]
    fn degenerate_rects_are_not_registered() {
        let ctx = egui::Context::default();
        toggle(&ctx);
        mark(&ctx, "空", egui::Rect::NOTHING);
        mark(&ctx, "零宽", r(10.0, 10.0, 0.0, 40.0));
        let n = with_state(&ctx, |st| st.spots.len());
        assert_eq!(n, 0, "零面积/负矩形不该进候选表");
    }

    /// 关掉标注模式必须清掉已选中的。留着的话下次开启会凭空冒出上次的编号,
    /// 用户会以为自己点错了 —— 同 `list.rs` 里 `pending_delete` 悬空那条坑。
    ///
    /// 自证会变红:把 `toggle` 里 `if !on { st.picked.clear(); … }` 删掉。
    #[test]
    fn leaving_the_mode_clears_the_picks_so_they_dont_reappear_next_time() {
        let ctx = egui::Context::default();
        toggle(&ctx);
        let s = spot("甲", r(0.0, 0.0, 10.0, 10.0));
        with_state(&ctx, |st| pick(&mut st.picked, &s));
        assert_eq!(with_state(&ctx, |st| st.picked.len()), 1);

        exit(&ctx);
        assert!(!is_on(&ctx));
        assert_eq!(
            with_state(&ctx, |st| st.picked.len()),
            0,
            "退出标注模式必须清空选中项,否则下次进来会凭空带着上次的编号"
        );
    }

    /// `Esc` 在标注模式**关着**的时候不该有任何副作用 —— 它得原样转发给终端
    /// (Claude Code 的 TUI 全靠 Esc)。这里测的是 `exit()` 这一半:关着时调它
    /// 不能把模式反过来打开。
    ///
    /// 自证会变红:把 `exit()` 改成无条件 `toggle(ctx)`。
    #[test]
    fn exit_while_already_off_does_not_turn_the_mode_on() {
        let ctx = egui::Context::default();
        exit(&ctx);
        assert!(!is_on(&ctx), "Esc 在标注模式关着时不该把它打开");
    }

    /// 按语义路径取回本帧登记的矩形。测试要往某个部件的中心注入点击 ——
    /// 硬编码坐标一改布局就静默打空,所以必须问 `annotate` 要。
    ///
    /// 顺带钉住那条最容易踩的前提:**标注模式关着时 `mark` 直接 return**,
    /// 不开模式就一处也找不到。
    ///
    /// 自证会变红:把 `spot_rect` 改成恒返回 `None`。
    #[test]
    fn spot_rect_finds_a_marked_widget_but_only_when_the_mode_is_on() {
        let ctx = egui::Context::default();
        let r = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 30.0));

        // 模式关着 —— 登记不进去。
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            mark(ctx, "甲/乙", r);
        });
        assert_eq!(
            spot_rect(&ctx, "甲/乙"),
            None,
            "标注模式关着时 mark 应该直接 return"
        );

        // 打开模式再登记。
        toggle(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            mark(ctx, "甲/乙", r);
        });
        assert_eq!(spot_rect(&ctx, "甲/乙"), Some(r));
        // 找不到完全相等的才退回子串匹配。
        assert_eq!(spot_rect(&ctx, "乙"), Some(r));
        assert_eq!(spot_rect(&ctx, "丙"), None);
    }

    /// 父路径是子路径的前缀、且子控件**先**登记时,查父控件必须拿到父的
    /// 矩形,不能静默拿到子的那一小块。
    ///
    /// 守的是切片 J Task 9 踩到的坑:`标签栏/标签 2/关闭`(× 按钮)先登记,
    /// `标签栏/标签 2`(整块标签)后登记,只做子串匹配的话「点标签标题」的
    /// 测试实际点在了 × 上,红得莫名其妙。
    ///
    /// 自证会变红:把 `spot_rect` 里的精确匹配那一支去掉,只留 `contains`。
    #[test]
    fn spot_rect_prefers_an_exact_path_over_a_child_that_was_marked_first() {
        let ctx = egui::Context::default();
        let child = egui::Rect::from_min_size(egui::pos2(90.0, 5.0), egui::vec2(14.0, 14.0));
        let parent = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 24.0));

        toggle(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            // 子先、父后 —— 跟 `one_tab` 里的登记顺序一致。
            mark(ctx, "标签栏/标签 2/关闭", child);
            mark(ctx, "标签栏/标签 2", parent);
        });

        assert_eq!(
            spot_rect(&ctx, "标签栏/标签 2"),
            Some(parent),
            "查父控件拿到了子控件的矩形 —— 往这块中心注入点击会点到 × 上"
        );
        assert_eq!(spot_rect(&ctx, "标签栏/标签 2/关闭"), Some(child));
    }
}
