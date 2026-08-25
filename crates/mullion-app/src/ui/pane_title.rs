//! pane 标题条(F83)。每个 pane 顶部 32px:序号 + 主机名 + 连接状态点 + 关闭按钮。
//!
//! 用 egui `Area::fixed_pos` 按绝对像素定位,只覆盖标题条那 32px,**不能**盖住
//! 整个 pane —— egui 在它覆盖的区域会吃掉指针事件(T8 的指针路由是"先喂 egui
//! 后判"),盖大了终端就再也划不了选。

use mullion_core::layout::PaneId;

use crate::shell::workspace::{PaneGeom, PaneStatus};
use crate::theme::{self, Theme};

/// 一个标题条要显示的东西。
pub struct TitleView<'a> {
    pub geom: PaneGeom,
    /// 该 pane 在几何顺序中的序号,从 1 起。
    pub index: usize,
    /// 主机标签(会话名或 user@host)。尚未连上时给 `None`。
    pub host: Option<&'a str>,
    pub status: PaneStatus,
    pub focused: bool,
    /// F61/F62:这个 pane 所属会话的已解析外观。`None` = 没有对应会话记录
    /// (快速连接、或 store 不可用)。**必须来自 `badge::AppearanceCache`**,
    /// 不许在这里现解析(陷阱 T3)。
    pub appearance: Option<&'a crate::ui::badge::Appearance>,
    /// ⑥:远端当前目录的最后一级(已由 `dir_leaf` 取好)。`None` = 不知道。
    /// 拼进左区那一整串 `序号 · 节点名 · 目录名 · tmux名`(`title_text`),
    /// 不再是独立右区。
    ///
    /// **拿 `String` 而不是 `&str`**:源头 `PaneState.cwd` 是字节,取最后一级
    /// 要经过 `to_string_lossy`,借不出一个活到本帧结束的 `&str`。每 pane
    /// 每帧一次短字符串分配,相对每帧几千个 glyph 可以忽略。
    pub cwd_leaf: Option<String>,
    /// ⑥:tmux 会话名。`None` = 不在 tmux 里 / 远端没开 `set-titles`。
    /// 拼进左区那一整串 `序号 · 节点名 · 目录名 · tmux名`(`title_text`),
    /// 不再是独立右区。
    pub tmux: Option<&'a str>,
    /// F163/D4:挂在这块 pane 上的一句说明(attach 失败 / 会话已删 / 连不上)。
    /// 来自 `PaneState::notice`。**不弹窗** —— 多块 pane 同时失败会连弹好几次。
    pub notice: Option<&'a str>,
}

/// ④:焦点分屏标题条上那层 accent 的不透明度。0.14 —— 够看出「这块亮一点」,
/// 又不至于把主机名的对比度压下去。
const FOCUS_TINT: f32 = 0.14;

/// ⑤:图标边长(逻辑点)。
///
/// 目标是 16 点(设计稿尺寸,再大就压过主机名);上限之外还要有下限:
/// 极小 pane 上 `layout_geometry` 会用 `title_bar_px(ppp).min(px.h)` 把标题条
/// 压扁,那时按内高缩到 10 点为止 —— 再小就是一坨糊,不如让它糊得整齐些。
/// 减 2 点是给图标留呼吸空间,不让它顶到边距线上。
pub fn icon_side(inner_h: f32) -> f32 {
    (inner_h - 2.0).clamp(10.0, 16.0)
}

/// 标题条上的文字。抽成纯函数是因为格式会被人反复调,而它是唯一能自动验的部分。
///
/// 格式:`序号 · 节点名 · 最后一级目录名 · tmux 会话名`。后两段来自 F123 的
/// 远端状态,**拿不到就连分隔符一起消失**(留一个孤零零的 `·` 比不显示更糟)。
///
/// 断开时只留 `序号 · 节点名 (已断开)`:目录名和 tmux 名此刻是陈旧值,
/// 远端早就不说话了,摆着是误导。
///
/// `notice`(F163/D4):挂在这块 pane 上的一句说明,拼在最后。**所有分支都要
/// 拼**——包括 `host` 为 `None` 那条 early return:占位 pane(D3 的会话已删 /
/// D6 的拨号降级)恰恰没有自己的 `host`,而那句说明是它唯一能显示的信息。
pub fn title_text(
    index: usize,
    host: Option<&str>,
    dir: Option<&str>,
    tmux: Option<&str>,
    status: PaneStatus,
    notice: Option<&str>,
) -> String {
    let tail = |mut s: String| {
        if let Some(n) = notice {
            s.push_str(" · ");
            s.push_str(n);
        }
        s
    };
    let Some(h) = host else {
        // `(None, ..)` 现在可达:占位 pane(`App::place_dead_pane`)的
        // `host_pending == true`,构造点据此不把 `hosts[host_ix]`(别人那台
        // 机器)当成自己的名字传进来,`host` 就是 `None`。这类 pane 没有
        // 自己的主机名,`notice` 是标题条上唯一有用的信息,必须拼上。
        return tail(format!("{index} · 连接中…"));
    };
    if status == PaneStatus::Disconnected {
        return tail(format!("{index} · {h} (已断开)"));
    }
    let mut parts = vec![index.to_string(), h.to_string()];
    parts.extend(dir.map(str::to_string));
    parts.extend(tmux.map(str::to_string));
    tail(parts.join(" · "))
}

/// 画一批标题条,返回被点了 × 的 pane(每帧至多一个)。
///
/// 用 `Area` 而非 `Panel`:标题条要跟着 pane 的绝对像素走,`Panel` 只会
/// 从窗口边缘往里堆。`fixed_pos` 收 point,所以像素要先除 `pixels_per_point`。
///
/// **两处越界坑**(headless 实测坐实过,别再踩,详见函数体内注释):
/// 1. `egui::Frame` 的占用尺寸(进而 `Area` 的可交互矩形,即
///    `ctx.memory(|m| m.area_rect(id))`)取的是 `content_ui.min_rect() + margin`——
///    只要内容(哪怕只有一个"●")的自然尺寸超过预留空间,`min_rect` 就会带着
///    margin 一起把 `Area` 的实际矩形撑到 `title_px` 之外:横向侵入右边邻居 pane、
///    纵向盖住终端第一行,吃掉本该属于终端的指针事件(T8 变体)。极端情形(高
///    DPI + 窄标题条 + 长主机名)哪怕加了下面第 2 条的截断,单个状态点在被压得
///    很矮的行高下仍可能比预留高度还高一丝——**不能**依赖"内容自己不超",必须
///    在几何上钉死。
/// 2. `ui.set_min_size` 只设下限、不设上限,`Label` 不截断的话 `horizontal` 布局
///    会把行撑宽,顶出 `×` 按钮。
///
/// 因此这里不用 `Frame::show`(它会把内容的 `min_rect` 折回父 ui,内容一旦超出
/// 预算,父 ui 跟着超),而是手动摆:背景用 `painter` 按 `full` 直接画死;内容摆进
/// 一个**不参与父 ui 尺寸折算**的 `new_child`(`set_clip_rect` 保证画多了也只是被
/// 裁掉,不会向外泄漏);最后显式 `allocate_rect(full, ..)`——`Area` 的占用尺寸永远
/// 等于 `full`,不多不少,内容长成什么样都不影响这个几何承诺。
/// `Area` 的 id,`show` 和测试都从这里取,别在两处各写一遍字面量。
fn area_id(id: PaneId) -> egui::Id {
    egui::Id::new(("pane_title", id.0))
}

/// ⑥:目录路径的最后一级,给标题条显示用。
///
/// `/` 与 `~` 原样返回(它们本身就是「最后一级」);尾部斜杠忽略;空路径
/// 给 `None`(别在标题条上摆一个空标签)。
///
/// 非 UTF-8 走 `to_string_lossy`:显示层宁可出一个 `�` 也要把目录名摆出来 ——
/// 整段消失的话用户完全不知道自己在哪。**② 那边不吃这个宽容度**,它只接受
/// 绝对路径的原始字节。
pub fn dir_leaf(cwd: &[u8]) -> Option<String> {
    let trimmed = match cwd {
        [] => return None,
        [b'/'] => return Some("/".to_string()),
        // `strip_suffix` 只剥一层:`/tmp//` 剥一次还剩 `/tmp/`,`rposition`
        // 照样定位到那个新暴露出来的尾部 `/`,`leaf` 就成了空串,整段目录名
        // 消失。这里用 `while` 剥掉**全部**尾部斜杠,再走同一套取最后一级的
        // 逻辑,退化成显示 `"tmp"` 而不是什么都不显示。
        _ => {
            let mut t = cwd;
            while let Some(s) = t.strip_suffix(b"/") {
                t = s;
            }
            t
        }
    };
    let leaf = match trimmed.iter().rposition(|&b| b == b'/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    };
    if leaf.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(leaf).into_owned())
}

/// 两个按钮的 id。**显式给**而不是让 egui 按布局位置自动生成:测试要
/// `ctx.read_response(id)` 取回矩形才点得中它们,而自动 id 依赖布局顺序,
/// 一改布局就失效。
fn close_id(id: PaneId) -> egui::Id {
    egui::Id::new(("pane_title_close", id.0))
}
fn rehost_id(id: PaneId) -> egui::Id {
    egui::Id::new(("pane_title_rehost", id.0))
}

/// 标题条小按钮的字号,也是自绘图标的边长。
const BUTTON_SIZE: f32 = 13.0;

/// 标题条小按钮上画什么。
#[derive(Clone, Copy)]
enum Mark {
    /// 一个字形。**只用于码位一定存在的字符** —— `×` 是 U+00D7(Latin-1
    /// Supplement),任何拉丁字体都有。
    Glyph(&'static str),
    /// 自绘的「换节点」双向箭头。
    ///
    /// 原来写的是 `⇆`(U+21C6),实机上是个 tofu 方框 □:egui 内嵌的
    /// Ubuntu-Light 没有这个码位,`install_cjk_font` 回退到的微软雅黑也没有
    /// (U+21C6 不在 GBK 里)。换一个"更常见的箭头字符"治不了本 —— F21 允许
    /// 用户换显示字体,换一次就可能再缺一次,而字形缺失这类事**只有人眼能
    /// 发现**(无头环境里 galley 照样有尺寸,测不出来)。自绘完全不依赖字体。
    SwapArrows,
}

/// 标题条上的一个小按钮。
///
/// **手动 `allocate` + `interact`,不用 `ui.small_button`**:后者的 id 由布局
/// 顺序自动生成,测试没法 `ctx.read_response(id)` 取回矩形去点它 ——「点了有没有
/// 反应」「两个挨着的按钮串没串」就永远测不到,而「想换节点结果把 pane 关了」
/// 是一次不可撤销的误操作。
///
/// 字形的尺寸按实测宽度算,不写死:`×` 在不同字体下宽度不同,写死会让它要么被
/// 裁一半、要么留一大块空白。自绘图标没有这个问题,给个正方形边长即可。
fn small_action_button(ui: &mut egui::Ui, id: egui::Id, mark: Mark, t: &Theme) -> egui::Response {
    let galley = match mark {
        Mark::Glyph(g) => Some(ui.painter().layout_no_wrap(
            g.to_string(),
            egui::FontId::proportional(BUTTON_SIZE),
            theme::c32(t.fg_muted),
        )),
        Mark::SwapArrows => None,
    };
    let content = galley
        .as_ref()
        .map_or(egui::Vec2::splat(BUTTON_SIZE), |g| g.size());
    let size = content + egui::vec2(8.0, 2.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let resp = ui.interact(rect, id, egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, theme::c32(t.panel_head));
    }
    let color = theme::c32(if resp.hovered() {
        t.fg_strong
    } else {
        t.fg_muted
    });
    match galley {
        Some(g) => {
            let at = rect.center() - g.size() / 2.0;
            ui.painter().galley(at, g, color);
        }
        None => paint_swap_arrows(ui.painter(), rect, color),
    }
    resp
}

/// 画「⇆」:上排箭头指右,下排箭头指左。纯线段,零字体依赖。
///
/// 尺寸全部由 `rect` 推,不写死像素 —— DPI 变了(`ScaleFactorChanged`)按钮矩形
/// 跟着变,图标必须一起变,否则高 DPI 下会缩成一小撮。
fn paint_swap_arrows(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let half_w = BUTTON_SIZE * 0.42;
    let gap = BUTTON_SIZE * 0.18; // 两排的半间距
    let head = BUTTON_SIZE * 0.20; // 箭头斜线的边长
    let stroke = egui::Stroke::new((BUTTON_SIZE * 0.10).max(1.0), color);
    let (l, r) = (c.x - half_w, c.x + half_w);
    for (y, tip, back) in [
        (c.y - gap, r, r - head), // 上排:箭头在右
        (c.y + gap, l, l + head), // 下排:箭头在左
    ] {
        painter.line_segment([egui::pos2(l, y), egui::pos2(r, y)], stroke);
        painter.line_segment([egui::pos2(tip, y), egui::pos2(back, y - head)], stroke);
        painter.line_segment([egui::pos2(tip, y), egui::pos2(back, y + head)], stroke);
    }
}

/// 一帧里标题条上发生的事。每项每帧至多一个(多块 pane 同时被点是不可能的)。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TitleAction {
    /// 点了 ×。
    pub close: Option<PaneId>,
    /// 点了「换节点」——只是**请求**,选哪个节点由 App 弹窗决定。
    pub rehost: Option<PaneId>,
}

pub fn show(ctx: &egui::Context, t: &Theme, views: &[TitleView<'_>]) -> TitleAction {
    let ppp = ctx.pixels_per_point();
    let mut action = TitleAction::default();
    for v in views {
        let tp = v.geom.title_px;
        // F100:pane 三处插桩。终端网格是 GPU 自绘的,accesskit 那条腿看不见它,
        // 而「这块终端」恰恰是最常要指的东西。**放在下面那个 `continue` 之前** ——
        // 标题条关掉时(F83)pane 和终端区仍然要能标。
        let px = |r: crate::shell::workspace::PxRect| {
            egui::Rect::from_min_size(
                egui::pos2(r.x as f32 / ppp, r.y as f32 / ppp),
                egui::vec2(r.w as f32 / ppp, r.h as f32 / ppp),
            )
        };
        crate::ui::annotate::mark(ctx, format!("分屏 {}", v.index), px(v.geom.px));
        crate::ui::annotate::mark(ctx, format!("分屏 {}/终端区", v.index), px(v.geom.term_px));
        if tp.h != 0 {
            crate::ui::annotate::mark(ctx, format!("分屏 {}/标题条", v.index), px(tp));
        }
        if tp.h == 0 {
            continue; // 标题条关掉了(F83 开关)
        }
        let id = area_id(v.geom.id);
        let pos = egui::pos2(tp.x as f32 / ppp, tp.y as f32 / ppp);
        let size = egui::vec2(tp.w as f32 / ppp, tp.h as f32 / ppp);
        let full = egui::Rect::from_min_size(pos, size);
        egui::Area::new(id)
            .fixed_pos(pos)
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.painter()
                    .rect(full, 0.0, theme::c32(t.panel_bg), theme::stroke(t));
                // ④:焦点分屏的标题条上叠一层薄 accent。底色统一走 `panel_bg`
                // 再叠色,而不是焦点时换成 `panel_head` —— `panel_head` 是
                // 「面板表头」那一档灰,跟 F62 的会话语义色、跟未聚焦态都太近,
                // 分屏一多就分不出来。`gamma_multiply` 把不透明的 accent 变成
                // 半透明,叠出来是一层薄色而不是一块纯 accent 底。
                if v.focused {
                    ui.painter().rect_filled(
                        full,
                        0.0,
                        theme::c32(t.accent).gamma_multiply(FOCUS_TINT),
                    );
                }
                // F62:语义色竖条走左边缘。**用 painter 直接画在 `full` 里**,
                // 不新增任何 widget、不参与布局计算 —— 这是绕开本文件顶部
                // 那两个越界坑的做法(`Frame` 的 min_rect+margin 撑破 Area、
                // `set_min_size` 只设下限)。守护:
                // `area_rect_stays_exact_even_with_appearance_bar_and_icon`。
                let bar_color = v.appearance.and_then(|a| {
                    crate::ui::badge::should_paint(a, mullion_store::ColorTarget::PaneTitle)
                });
                if let Some(c) = bar_color {
                    crate::ui::badge::paint_edge_bar(
                        ui.painter(),
                        full,
                        crate::ui::badge::Side::Left,
                        c,
                    );
                }
                let inner = full.shrink2(egui::vec2(8.0, 4.0));
                let mut content = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                content.set_clip_rect(inner);
                content.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if small_action_button(ui, close_id(v.geom.id), Mark::Glyph("×"), t)
                        .on_hover_text("关闭此分屏")
                        .clicked()
                    {
                        action.close = Some(v.geom.id);
                    }
                    // 用户要的入口:分屏之后在这里把这块 pane 换到别的节点。
                    // 放在 × 左边而不是做成"点主机名":主机名会被 `.truncate()`
                    // 截断,长主机名时点击靶子会缩到几个像素宽。
                    if small_action_button(ui, rehost_id(v.geom.id), Mark::SwapArrows, t)
                        .on_hover_text("把这块分屏换到别的节点")
                        .clicked()
                    {
                        action.rehost = Some(v.geom.id);
                    }
                    // 剩下的空间(已扣掉 × / 换节点按钮)左对齐摆状态点 + 一整串标题文字;
                    // 排版用的 available_width 到这里已经是扣掉 × 之后的余量。
                    // × 不被顶出条外,靠的是 right_to_left 先占位 + 外层 set_clip_rect
                    // 裁剪(clip 同时裁交互,见 egui `Ui::interact`,egui-0.30 `ui.rs:1057`
                    // 的 `interact_rect: self.clip_rect().intersect(rect)`);
                    // Area 的外部几何已被
                    // `allocate_rect(full, ..)` 硬钉死,与内容截不截断完全解耦。
                    // `.truncate()` 现在唯一的作用是视觉观感(省略号 vs 硬裁切),
                    // 不是防止 × 被顶出条外的兜底。这个观感由
                    // `a_title_too_long_for_the_bar_gets_an_ellipsis_not_a_hard_cut`
                    // 钉着(删掉 `.truncate()` 它会红)。
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // F61:图标画在状态点之前。`content` 是个 `new_child` +
                        // `set_clip_rect`,画多了只会被裁掉,不会把 `Area` 撑大。
                        if let Some(icon) = v.appearance.and_then(|a| a.icon.as_ref()) {
                            // ⑤:边长按内容区高度算,不写死 —— 见 `icon_side`。
                            let side = icon_side(inner.height());
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                            crate::ui::badge::paint_icon(
                                ui.painter(),
                                r,
                                icon,
                                v.appearance.and_then(|a| {
                                    crate::ui::badge::should_paint(
                                        a,
                                        mullion_store::ColorTarget::PaneTitle,
                                    )
                                }),
                            );
                        }
                        let dot = match v.status {
                            PaneStatus::Live => t.ok,
                            // F128:重连中 = 黄色(还有救),与 Disconnected 的灰
                            // 明确区分:用户看一眼就知道要不要自己动手。
                            PaneStatus::Reconnecting => t.warn,
                            PaneStatus::Disconnected => t.fg_dim,
                        };
                        ui.colored_label(theme::c32(dot), "●");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(title_text(
                                    v.index,
                                    v.host,
                                    v.cwd_leaf.as_deref(),
                                    v.tmux,
                                    v.status,
                                    v.notice,
                                ))
                                .color(theme::c32(if v.focused {
                                    t.fg_strong
                                } else {
                                    t.fg_muted
                                })),
                            )
                            .truncate(),
                        );
                    });
                });
                // 不把 content 的占用尺寸折回 ui——见函数文档注释第 1 条。
                ui.allocate_rect(full, egui::Sense::hover());
            });
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⑥/F123:一整串按「序号 · 节点名 · 目录名 · tmux 名」拼。
    ///
    /// 序号保留:分屏一多要靠它认哪块是哪块。
    #[test]
    fn title_puts_host_then_directory_then_tmux_on_one_line() {
        assert_eq!(
            title_text(
                2,
                Some("build-01"),
                Some("Mullion"),
                Some("main"),
                PaneStatus::Live,
                None
            ),
            "2 · build-01 · Mullion · main"
        );
    }

    /// 缺的段**连分隔符一起消失** —— 留一个孤零零的 `·` 比不显示更糟。
    ///
    /// 自证会变红:把实现改成无条件 `format!("{index} · {host} · {dir} · {tmux}")`
    /// 并把 `None` 渲染成空串。
    #[test]
    fn missing_pieces_take_their_separator_with_them() {
        assert_eq!(
            title_text(
                3,
                Some("build-01"),
                Some("Mullion"),
                None,
                PaneStatus::Live,
                None
            ),
            "3 · build-01 · Mullion"
        );
        assert_eq!(
            title_text(
                4,
                Some("build-01"),
                None,
                Some("main"),
                PaneStatus::Live,
                None
            ),
            "4 · build-01 · main"
        );
        assert_eq!(
            title_text(5, Some("build-01"), None, None, PaneStatus::Live, None),
            "5 · build-01"
        );
    }

    /// §6.3:断开的 pane 内容留着可滚可复制,但状态必须写在脸上 ——
    /// 不然用户会对着一块不响应的终端反复敲键。
    ///
    /// **断开时不带目录和 tmux 名**:那两个此刻是陈旧值(远端早就不说话了),
    /// 摆着是误导。
    ///
    /// 自证会变红:把 `Disconnected` 分支也拼上 dir/tmux。
    #[test]
    fn a_disconnected_pane_says_so_and_drops_the_stale_context() {
        let s = title_text(
            1,
            Some("build-01"),
            Some("Mullion"),
            Some("main"),
            PaneStatus::Disconnected,
            None,
        );
        assert_eq!(s, "1 · build-01 (已断开)");
    }

    /// 预分配了叶子位但 channel 还没开好的空窗期(见 Workspace::apply_preset)。
    #[test]
    fn pane_without_a_host_yet_says_connecting() {
        assert_eq!(
            title_text(
                3,
                None,
                Some("Mullion"),
                Some("main"),
                PaneStatus::Live,
                None
            ),
            "3 · 连接中…"
        );
    }

    /// F163/D4:attach 失败的说明挂在**这块 pane 的标题条**上,不弹窗 ——
    /// 多块 pane 同时失败时会连弹好几次。
    ///
    /// 自证会变红:把 `title_text` 的 `notice` 参数忽略掉。
    #[test]
    fn a_pane_notice_shows_up_on_its_own_title_bar() {
        let got = title_text(
            2,
            Some("prod"),
            Some("srv"),
            None,
            PaneStatus::Live,
            Some("当初的会话 web01 已不存在"),
        );
        assert!(got.contains("web01 已不存在"), "{got}");
        assert!(got.contains("prod"), "说明不该把原来那串顶掉:{got}");
    }

    /// 断开态 pane 的说明同样要出得来(D3 占位 / D6 降级都是断开态)——
    /// 断开分支原本是一条 early return,漏了它就整条看不见。
    #[test]
    fn a_disconnected_pane_still_shows_its_notice() {
        let got = title_text(
            1,
            Some("prod"),
            None,
            None,
            PaneStatus::Disconnected,
            Some("会话已被删除,无法自动恢复"),
        );
        assert!(got.contains("会话已被删除"), "{got}");
    }

    /// D3/D6:占位 pane 没有自己的 `HostConn`(`host_ix` 是哑值 0),标题条
    /// 不许显示成**别人那台机器**的名字 —— 那台其实连得好好的,用户会以为
    /// 它断了。没有主机名时,那句说明是标题条上唯一有用的信息,必须出得来。
    ///
    /// 自证会变红:把 `title_text` 里 `host: None` 那条 early return 的
    /// `tail(...)` 去掉。
    #[test]
    fn a_pane_with_no_host_of_its_own_still_shows_why() {
        let got = title_text(
            2,
            None,
            None,
            None,
            PaneStatus::Disconnected,
            Some("会话已被删除,无法自动恢复"),
        );
        assert!(got.contains("会话已被删除"), "{got}");
        assert!(!got.contains("prod"), "{got}");
    }

    /// 800×600 物理像素、开着标题条的单 pane 几何。
    ///
    /// ⑤:标题条高度现在随 DPI 走,所以要传 ppp —— 传死 32 的话高 DPI 的
    /// 用例量的就不是真实几何。
    fn geom_800x600_title32(id: u32, ppp: f32) -> PaneGeom {
        use crate::shell::workspace::{title_bar_px, PxRect};
        PaneGeom {
            id: PaneId(id),
            px: PxRect {
                x: 0,
                y: 100,
                w: 800,
                h: 600,
            },
            title_px: PxRect {
                x: 0,
                y: 100,
                w: 800,
                h: title_bar_px(ppp),
            },
            term_px: PxRect {
                x: 0,
                y: 132,
                w: 800,
                h: 568,
            },
            grid: (80, 28),
        }
    }

    /// pane 整块 / 标题条 / 终端区都要能在标注模式(F100)里选中。终端网格是
    /// GPU 自绘的,不是 egui widget,accesskit 那条腿覆盖不到它 —— 只能手工插桩。
    ///
    /// **标题条关掉时(F83)仍要能标 pane 与终端区**:`show` 里那句
    /// `if tp.h == 0 { continue }` 在插桩之后,不能把它们一起跳过。
    ///
    /// 自证会变红:把 `show` 里三句 `annotate::mark` 中任意一句注释掉。
    #[test]
    fn panes_are_markable_even_when_the_title_bar_is_hidden() {
        for title_h in [0, 32] {
            let ctx = egui::Context::default();
            crate::ui::annotate::toggle(&ctx);
            let mut view = TitleView {
                geom: geom_800x600_title32(1, 1.0),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
                cwd_leaf: None,
                tmux: None,
                notice: None,
            };
            view.geom.title_px.h = title_h;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(
                    ctx,
                    &crate::theme::MULLION_DARK,
                    std::slice::from_ref(&view),
                );
            });
            let paths = crate::ui::annotate::spot_paths(&ctx);
            assert!(
                paths.contains(&"分屏 1".to_string()),
                "title_h={title_h}: {paths:?}"
            );
            assert!(
                paths.contains(&"分屏 1/终端区".to_string()),
                "title_h={title_h}: {paths:?}"
            );
            assert_eq!(
                paths.contains(&"分屏 1/标题条".to_string()),
                title_h != 0,
                "title_h={title_h}: 标题条关掉时不该登记一个零高的标题条"
            );
        }
    }

    /// 真正驱动 egui 跑一帧,用 `Memory::area_rect` 取回 `show()` 画出来的
    /// `Area` 的**实际**占用矩形——这是本函数唯一能自动验证「标题条到底占了
    /// 多大地方」的手段。egui 面板层是逻辑点,`title_px` 是物理像素,断言前
    /// 先按 `ppp` 换算。
    ///
    /// 这条测试同时守两件事:
    /// 1. 不多占(Critical 1):`Frame` 的 `inner_margin` 双向外扩 + 长文本不截断,
    ///    实测过会把 `Area` 撑出 `title_px` 之外,盖住终端首行或侵入邻居 pane。
    /// 2. 用的是 `title_px` 不是 `px`(Critical 2):`px.h`=600 与 `title_px.h`=32
    ///    差一个数量级,`show()` 里如果手滑把 `v.geom.title_px` 换成
    ///    `v.geom.px`,这里的期望值会被打得面目全非,测试必红。
    ///
    /// ppp 覆盖 100% / 125% / 150%——Windows 最常见的三档缩放。
    #[test]
    fn area_rect_matches_title_px_exactly_across_dpi_scales() {
        use crate::shell::workspace::title_bar_px;
        for ppp in [1.0f32, 1.25, 1.5] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let views = [TitleView {
                geom: geom_800x600_title32(1, ppp),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
                cwd_leaf: None,
                tmux: None,
                notice: None,
            }];
            let _ = ctx.run(Default::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, &views);
            });
            let rect = ctx
                .memory(|m| m.area_rect(area_id(PaneId(1))))
                .unwrap_or_else(|| panic!("ppp={ppp}: 标题条没画出任何 Area"));
            let want_w = 800.0 / ppp;
            let want_h = title_bar_px(ppp) as f32 / ppp;
            assert!(
                (rect.width() - want_w).abs() < 0.5,
                "ppp={ppp}: Area 宽 {} 应约等于 title_px 换算值 {want_w},差太多说明撑出了标题条",
                rect.width()
            );
            assert!(
                (rect.height() - want_h).abs() < 0.5,
                "ppp={ppp}: Area 高 {} 应约等于 title_px 换算值 {want_h},差太多说明撑出了标题条",
                rect.height()
            );
        }
    }

    /// 极端情形:标题条本身很窄、主机名又很长。不截断的话 `×` 会被文字顶出
    /// `title_px` 右边界,侵入邻居 pane 的地盘。
    #[test]
    fn long_host_name_does_not_push_area_past_title_px() {
        use crate::shell::workspace::{title_bar_px, PxRect};
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let geom = PaneGeom {
            id: PaneId(2),
            px: PxRect {
                x: 0,
                y: 0,
                w: 160,
                h: 600,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 160,
                h: title_bar_px(1.0),
            },
            term_px: PxRect {
                x: 0,
                y: 32,
                w: 160,
                h: 568,
            },
            grid: (16, 28),
        };
        let views = [TitleView {
            geom,
            index: 1,
            host: Some("this-is-a-ridiculously-long-hostname-that-will-never-fit.example.com"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
            cwd_leaf: None,
            tmux: None,
            notice: None,
        }];
        let _ = ctx.run(Default::default(), |ctx| {
            show(ctx, &crate::theme::MULLION_DARK, &views);
        });
        let rect = ctx
            .memory(|m| m.area_rect(area_id(PaneId(2))))
            .expect("标题条没画出任何 Area");
        assert!(
            (rect.width() - 160.0).abs() < 0.5,
            "长主机名把 Area 撑宽到 {},应截断在 160 逻辑点以内",
            rect.width()
        );
    }

    /// 在指定的**逻辑点**位置完整点一下(移入 → 按下 → 抬起)。
    ///
    /// 三个事件必须在同一帧里发全:egui 的 `clicked()` 判据是「本帧收到了
    /// 抬起、且按下时指针也在这个 widget 上」,只发按下或只发抬起都点不出来。
    fn click_at(pos: egui::Pos2) -> egui::RawInput {
        let modifiers = egui::Modifiers::default();
        egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                },
            ],
            ..Default::default()
        }
    }

    /// 跑一帧拿按钮位置、再跑一帧点它,返回 `show` 的动作。
    fn click_button(which: egui::Id) -> TitleAction {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let views = [TitleView {
            geom: geom_800x600_title32(1, 1.0),
            index: 1,
            host: Some("dev@build-01"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
            cwd_leaf: None,
            tmux: None,
            notice: None,
        }];
        // **必须显式把时间推过 `Area` 的 fade_in**。默认 `RawInput` 的
        // `time` 是 `None`,egui 会拿墙钟凑,两帧之间可能只过了几微秒 ——
        // 淡入没走完时 `Area` 的内容不可交互,点下去毫无反应,而现象是
        // 「测试红,但代码看着没错」。跟 `icon_backdrop_uses_the_pane_title_target`
        // 同一个坑、同一个解法。
        let frame = |ctx: &egui::Context, t: f64| {
            ctx.run(
                egui::RawInput {
                    time: Some(t),
                    ..Default::default()
                },
                |ctx| {
                    show(ctx, &crate::theme::MULLION_DARK, &views);
                },
            )
        };
        // 两帧预热:第一帧让按钮登记进 widget 表,第二帧把淡入走完。
        let _ = frame(&ctx, 0.0);
        let _ = frame(&ctx, 1.0);
        let rect = ctx
            .read_response(which)
            .unwrap_or_else(|| panic!("标题条上找不到 {which:?} 这个按钮"))
            .rect;
        let mut out = TitleAction::default();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(2.0),
                ..click_at(rect.center())
            },
            |ctx| {
                out = show(ctx, &crate::theme::MULLION_DARK, &views);
            },
        );
        out
    }

    /// 用户要的入口:分屏之后在 **pane 标题条上**把这块换到别的节点。
    ///
    /// 自证会变红:把 `show` 里换节点按钮的 `rehost = Some(..)` 删掉。
    #[test]
    fn clicking_the_rehost_button_asks_to_change_this_panes_node() {
        let a = click_button(rehost_id(PaneId(1)));
        assert_eq!(a.rehost, Some(PaneId(1)), "点「换节点」应报告这块 pane");
        assert_eq!(a.close, None, "点「换节点」不该顺带把 pane 关了");
    }

    /// 标题条上的按钮**不许**用超出 Latin-1 的字符画。
    ///
    /// 原来的换节点按钮写的是 `⇆`(U+21C6),实机上是个 tofu 方框 □ —— egui 内嵌
    /// 的 Ubuntu-Light 没这个码位,`install_cjk_font` 回退到的微软雅黑也没有。
    /// 这类事**只有人眼能发现**:字形缺失时 `layout_no_wrap` 照样返回一个正常
    /// 尺寸的 galley,任何几何断言都是绿的。所以守护只能落在"用了什么字符"上。
    ///
    /// 阈值取 0x100(Latin-1 Supplement 及以下):`×` U+00D7 在这里面,任何拉丁
    /// 字体都有它;想画别的就走 `Mark` 的自绘分支,那条路零字体依赖。
    /// F21 允许用户换显示字体,换一次就可能再缺一次 —— 这也是不能靠"挑一个更
    /// 常见的箭头字符"了事的原因。
    ///
    /// 锚点拆开拼 **且** 先剥掉注释行:两层都要。拼是防扫到测试自己这行代码;
    /// 剥注释是防扫到任何一处**文档里写全的字面量** —— 本项目在源码级守护上
    /// 反复栽在这上面(注释里写全字面量是第五类恒绿模式的第二种形态),
    /// 这条测试第一版就因为文档里举了反例而假红。所以文档里也不写全 `⇆`
    /// 那个实参,只说码位。
    ///
    /// 自证会变红:把换节点按钮改回 `Mark::Glyph` 加 U+21C6 那个字符。
    #[test]
    fn title_bar_buttons_never_use_a_glyph_the_font_might_not_have() {
        let raw = include_str!("pane_title.rs");
        let src: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let needle = concat!("Mark::Gly", "ph(\"");
        let mut seen = 0;
        for tail in src.split(needle).skip(1) {
            let arg = &tail[..tail.find('"').expect("Mark::Glyph 的实参没有闭引号")];
            seen += 1;
            for ch in arg.chars() {
                assert!(
                    (ch as u32) < 0x100,
                    "标题条按钮用了 {ch:?}(U+{:04X})—— 超出 Latin-1,\
                     实机上很可能画成 tofu 方框。改走 Mark 的自绘分支",
                    ch as u32
                );
            }
        }
        assert!(
            seen > 0,
            "一个 Mark::Glyph 都没扫到 —— 锚点失效,这条测试恒绿了"
        );
    }

    /// 反面:两个按钮挨着,串了的话用户想换节点却把 pane 关掉了 —— 一次
    /// 不可撤销的误操作。
    ///
    /// 自证会变红:把 `show` 里两个按钮的赋值对调。
    #[test]
    fn clicking_close_still_closes_and_does_not_ask_to_rehost() {
        let a = click_button(close_id(PaneId(1)));
        assert_eq!(a.close, Some(PaneId(1)));
        assert_eq!(a.rehost, None, "点 × 不该弹换节点");
    }

    /// F83 标题条开关关闭(`title_px.h == 0`)时,这个 pane 不该画出任何 `Area`——
    /// 删掉 `show()` 里 `if tp.h == 0 { continue }` 这段此前仍然全绿,是测试盲区。
    #[test]
    fn title_bar_off_draws_no_area() {
        use crate::shell::workspace::PxRect;
        let ctx = egui::Context::default();
        let geom = PaneGeom {
            id: PaneId(3),
            px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            grid: (80, 30),
        };
        let views = [TitleView {
            geom,
            index: 1,
            host: Some("h"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
            cwd_leaf: None,
            tmux: None,
            notice: None,
        }];
        let _ = ctx.run(Default::default(), |ctx| {
            show(ctx, &crate::theme::MULLION_DARK, &views);
        });
        assert!(
            ctx.memory(|m| m.area_rect(area_id(PaneId(3)))).is_none(),
            "title_px.h == 0(标题条关闭)时不该为这个 pane 画 Area"
        );
    }

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

    fn appearance_with(targets: Vec<mullion_store::ColorTarget>) -> crate::ui::badge::Appearance {
        crate::ui::badge::Appearance {
            icon: None,
            color: Some(mullion_store::ColorSpec {
                hex: "#e06767".into(),
                apply_to: targets,
            }),
        }
    }

    fn run_title(appearance: Option<&crate::ui::badge::Appearance>) -> usize {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let views = [TitleView {
            geom: geom_800x600_title32(1, 1.0),
            index: 1,
            host: Some("dev@build-01"),
            status: PaneStatus::Live,
            focused: true,
            appearance,
            cwd_leaf: None,
            tmux: None,
            notice: None,
        }];
        // 跑两帧:`Area` 默认 `fade_in`,第一帧 opacity 是 0,画的图形会被
        // painter 记成 `Shape::Noop`(egui-0.30 `painter.rs::Painter::add`),
        // 数不出来。跟 `ui/mod.rs::rendered_text` 同一个理由、同一个套路。
        let _ = ctx.run(Default::default(), |ctx| {
            show(ctx, &crate::theme::MULLION_DARK, &views);
        });
        let out = ctx.run(Default::default(), |ctx| {
            show(ctx, &crate::theme::MULLION_DARK, &views);
        });
        count_shapes(&out.shapes)
    }

    /// F62:勾了「pane 标题条」的会话,标题条左边缘要多一条竖条。
    #[test]
    fn pane_title_paints_an_edge_bar_when_apply_to_includes_pane_title() {
        use mullion_store::ColorTarget;
        let none = run_title(None);
        let with = run_title(Some(&appearance_with(vec![ColorTarget::PaneTitle])));
        assert!(
            with > none,
            "勾了「pane 标题条」的会话应该多画一条竖条(无 {none} 个图形,有 {with} 个)"
        );
    }

    /// 没勾就不画。
    #[test]
    fn pane_title_paints_nothing_when_apply_to_excludes_pane_title() {
        use mullion_store::ColorTarget;
        let none = run_title(None);
        let other = run_title(Some(&appearance_with(vec![ColorTarget::ListItem])));
        assert_eq!(other, none, "只勾了会话列表的会话不该在 pane 标题条上画");
    }

    /// 造一张真能解出来的 .ico(走的是生产代码那条归一化路径)。
    fn real_ico() -> String {
        let px: Vec<u8> = std::iter::repeat_n([7u8, 8, 9, 255], 32 * 32)
            .flatten()
            .collect();
        let img = ico::IconImage::from_rgba_data(32, 32, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        crate::ui::ico::import(&raw).unwrap()
    }

    /// **本任务最关键的回归**:加了竖条和图标之后,`Area` 的几何承诺不能变。
    ///
    /// 本文件顶部注释警告过两个越界坑(`Frame` 的 `min_rect + margin` 撑破
    /// `Area`、`set_min_size` 只设下限)。竖条用 painter 直接画在已经
    /// `allocate_rect(full, ..)` 的矩形里、不新增任何 widget,就是为了绕开
    /// 它们 —— 这条测试钉死这个前提在有外观的情况下依然成立。
    #[test]
    fn area_rect_stays_exact_even_with_appearance_bar_and_icon() {
        use crate::shell::workspace::title_bar_px;
        use mullion_store::{ColorTarget, IconKind, IconSpec};
        let a = crate::ui::badge::Appearance {
            // 必须用**真能画出来**的图标。用 `IconKind::Builtin`/`Emoji` 的话
            // `paint_icon` 直接走降级不画(内置形状 v0.1.24 已撤、emoji
            // v0.1.26 已撤),这条「加了图标几何也不变」的断言就变成了空跑。
            icon: Some(IconSpec {
                kind: IconKind::Ico,
                value: real_ico(),
                bg: None,
            }),
            color: Some(mullion_store::ColorSpec {
                hex: "#e06767".into(),
                apply_to: vec![ColorTarget::PaneTitle],
            }),
        };
        for ppp in [1.0f32, 1.25, 1.5] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let views = [TitleView {
                geom: geom_800x600_title32(1, ppp),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: Some(&a),
                cwd_leaf: None,
                tmux: None,
                notice: None,
            }];
            let _ = ctx.run(Default::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, &views);
            });
            let rect = ctx
                .memory(|m| m.area_rect(area_id(PaneId(1))))
                .unwrap_or_else(|| panic!("ppp={ppp}: 标题条没画出任何 Area"));
            assert!(
                (rect.width() - 800.0 / ppp).abs() < 0.5,
                "ppp={ppp}: 加了外观后 Area 宽 {} 撑出了 title_px",
                rect.width()
            );
            assert!(
                (rect.height() - title_bar_px(ppp) as f32 / ppp).abs() < 0.5,
                "ppp={ppp}: 加了外观后 Area 高 {} 撑出了 title_px",
                rect.height()
            );
        }
    }

    /// F61/F62 复核挖出的真缺口,与 `list.rs` 的
    /// `icon_backdrop_uses_the_list_item_target_not_pane_title` 互为镜像:
    /// 标题条画图标底色时必须钉死用的是 `ColorTarget::PaneTitle`,不是随手
    /// 传了别的落点。`area_rect_stays_exact_even_with_appearance_bar_and_icon`
    /// 虽然同时设了 icon 和 color,但只查 `Area` 几何,查不出底色到底有没有
    /// 垫、垫没垫对颜色——复核之前这条调用完全没有安全网。
    ///
    /// 用「填色 + 方形」而不是数图形总数区分图标底色和边缘竖条:两者用的是
    /// 同一次 `should_paint` 系调用结果,颜色一样,但边缘竖条是 `EDGE_BAR_W`
    /// 宽、标题条高的细长条,图标底色是 14x14 的正方形。
    ///
    /// 自证会变红:把画图标那次 `should_paint` 调用的 `ColorTarget::PaneTitle`
    /// 改成 `ColorTarget::ListItem`(边缘竖条那次不动)。
    #[test]
    fn icon_backdrop_uses_the_pane_title_target_not_list_item() {
        use mullion_store::{ColorTarget, IconKind, IconSpec};
        let color = egui::Color32::from_rgb(0x1e, 0x88, 0xe5);

        fn appearance_with_icon(targets: Vec<ColorTarget>) -> crate::ui::badge::Appearance {
            crate::ui::badge::Appearance {
                icon: Some(IconSpec {
                    kind: IconKind::Ico,
                    value: real_ico(),
                    bg: None,
                }),
                color: Some(mullion_store::ColorSpec {
                    hex: "#1e88e5".into(),
                    apply_to: targets,
                }),
            }
        }

        fn run_title_shapes(appearance: Option<&crate::ui::badge::Appearance>) -> egui::FullOutput {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(1.0);
            let views = [TitleView {
                geom: geom_800x600_title32(1, 1.0),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance,
                cwd_leaf: None,
                tmux: None,
                notice: None,
            }];
            // 显式推进时间,而不是像 `run_title` 那样跑两帧靠墙钟走时间:
            // 这条测试要比较**颜色**,`fade_in` 半路上的不透明度会把 RGB
            // 也跟着缩放(premultiplied),两帧之间墙钟走了多久是不确定的,
            // 缩放比例就对不上、颜色比不出来。显式把第二帧的时间戳推到远
            // 超过 `animation_time`(默认 1/12s)之后,拿到的就是稳定的
            // 满不透明度颜色。
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(0.0),
                    ..Default::default()
                },
                |ctx| {
                    show(ctx, &crate::theme::MULLION_DARK, &views);
                },
            );
            ctx.run(
                egui::RawInput {
                    time: Some(1.0),
                    ..Default::default()
                },
                |ctx| {
                    show(ctx, &crate::theme::MULLION_DARK, &views);
                },
            )
        }

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

        let list_item_only = appearance_with_icon(vec![ColorTarget::ListItem]);
        let baseline = square_fill_count(&run_title_shapes(Some(&list_item_only)).shapes, color);
        assert_eq!(
            baseline, 0,
            "只勾了「会话列表」的会话,不该在标题条的图标下垫这个颜色的方块"
        );

        let pane_title_only = appearance_with_icon(vec![ColorTarget::PaneTitle]);
        let with_bg = square_fill_count(&run_title_shapes(Some(&pane_title_only)).shapes, color);
        assert_eq!(
            with_bg, 1,
            "勾了「pane 标题条」的会话,图标下应该恰好垫一块这个颜色的方块"
        );
    }

    /// ⑤:图标必须**整个**画在标题条内容区里,任何 DPI 下都不许被裁。
    ///
    /// `paint_icon` 走 `painter.image`,在形状列表里是唯一的 `Shape::Mesh`
    /// (文字是 `Shape::Text`,底色/竖条是 `Shape::Rect`)—— 拿它的
    /// `visual_bounding_rect()` 跟 `inner`(= `full.shrink2(8, 4)`)比。
    ///
    /// 曾经的 bug:标题条高度是 32 **物理**像素常量,而边距/图标是逻辑点,
    /// 150% 缩放下内高只有 13.33 点,装不下 14 点的图标,底部被
    /// `set_clip_rect(inner)` 裁掉。
    ///
    /// 自证会变红:把 Task 3a 的 `title_bar_px` 改回 `|_| 32`(高 DPI 下标题条
    /// 只剩 21.33 点,内高 13.33 点,装不下图标)。
    ///
    /// **这条测试验的是 3a,验不到 `icon_side` 本身** —— `icon_side` 里那个
    /// `- 2.0` 已经保证结果恒小于内高,与 clamp 的两个边界无关;而 3a 修好之后
    /// `inner.height()` 在任何 ppp 下都恒为 24 点,压根走不到边界。`icon_side`
    /// 的边界行为由 `icon_side_stays_inside_the_content_box_until_the_bar_is_squashed`
    /// 单独钉。
    #[test]
    fn the_icon_fits_inside_the_title_bar_at_every_scale() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0] {
            let ap = crate::ui::badge::Appearance {
                icon: Some(mullion_store::IconSpec {
                    kind: mullion_store::IconKind::Ico,
                    value: real_ico(),
                    bg: None,
                }),
                ..Default::default()
            };
            let id = PaneId(7);
            let views = [TitleView {
                geom: geom_800x600_title32(id.0, ppp),
                index: 1,
                host: Some("h"),
                status: PaneStatus::Live,
                focused: false,
                appearance: Some(&ap),
                cwd_leaf: None,
                tmux: None,
                notice: None,
            }];

            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let t = crate::theme::MULLION_DARK;
            let mut shapes = Vec::new();
            for _ in 0..2 {
                let out = ctx.run(Default::default(), |ctx| {
                    show(ctx, &t, &views);
                });
                shapes = out.shapes;
            }

            let full = {
                let tp = views[0].geom.title_px;
                egui::Rect::from_min_size(
                    egui::pos2(tp.x as f32 / ppp, tp.y as f32 / ppp),
                    egui::vec2(tp.w as f32 / ppp, tp.h as f32 / ppp),
                )
            };
            let inner = full.shrink2(egui::vec2(8.0, 4.0));

            let mut seen = 0;
            for cs in &shapes {
                collect_meshes(&cs.shape, &mut |r: egui::Rect| {
                    seen += 1;
                    assert!(
                        inner.contains_rect(r),
                        "ppp={ppp}: 图标 {r:?} 没装进内容区 {inner:?}(内高 {:.2} 点)",
                        inner.height()
                    );
                });
            }
            assert_eq!(seen, 1, "ppp={ppp}: 该恰好画出一个图标 mesh,实际 {seen} 个");
        }
    }

    /// 装不下时给**省略号**,不是硬裁在半个字上。
    ///
    /// 合成一整串之后这条才有分量:以前主机名和右区各自分开、各自短,现在
    /// `序号 · 节点 · 目录 · tmux` 一条串起来,长度是四段之和,窄 pane 上
    /// 「装不下」从边角情况变成了常态。
    ///
    /// **不是在验「不换行」** —— 这个 `Layout` 下 `wrap_mode` 本来就是
    /// `Extend`,删掉 `.truncate()` 也不会换行(已实测,`show()` 里那段注释
    /// 也是这么写的)。删掉它真正的后果是文字被 `set_clip_rect(inner)` 硬裁
    /// 在半个字形上、没有任何「后面还有」的提示。
    ///
    /// 另外那条 `a_long_cwd_and_tmux_name_do_not_push_the_area_past_title_px`
    /// 验不到这个:它钉的是 `show()` 末尾 `allocate_rect(full, ..)`,`Area`
    /// 的尺寸跟 `content` 里截不截断完全解耦(它自己的文档也这么写)。
    ///
    /// 自证会变红:删掉 `show()` 里 `Label` 上的 `.truncate()`。
    #[test]
    fn a_title_too_long_for_the_bar_gets_an_ellipsis_not_a_hard_cut() {
        let id = PaneId(11);
        let views = [TitleView {
            geom: geom_800x600_title32(id.0, 1.0),
            index: 1,
            host: Some("dev@a-very-long-hostname-that-eats-the-whole-row-by-itself"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
            cwd_leaf: Some("a-directory-name-that-is-also-absurdly-long".to_string()),
            tmux: Some("a-tmux-session-name-that-is-long-too"),
            notice: None,
        }];

        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                show(ctx, &t, &views);
            });
            shapes = out.shapes;
        }

        let mut texts = Vec::new();
        for cs in &shapes {
            collect_galleys(&cs.shape, &mut |t: &str, elided| {
                texts.push((t.to_string(), elided));
            });
        }
        let title = texts
            .iter()
            .find(|(t, _)| t.contains("dev@"))
            .expect("标题文字一个 shape 都没画出来,测试失去意义");
        assert!(
            title.1,
            "标题装不下却没打省略号 —— 会被内容区硬裁在半个字上:{:?}",
            title.0
        );
    }

    /// 递归找 `Shape::Text`,给出每个 galley 的原文与「是否被省略」。
    fn collect_galleys(s: &egui::Shape, f: &mut impl FnMut(&str, bool)) {
        match s {
            egui::Shape::Text(t) => f(t.galley.text(), t.galley.elided),
            egui::Shape::Vec(v) => {
                for x in v {
                    collect_galleys(x, f);
                }
            }
            _ => {}
        }
    }

    /// 递归找 `Shape::Mesh`(`Vec` / `Callback` 之外的容器只有 `Vec`)。
    fn collect_meshes(s: &egui::Shape, f: &mut impl FnMut(egui::Rect)) {
        match s {
            egui::Shape::Mesh(_) => f(s.visual_bounding_rect()),
            egui::Shape::Vec(v) => {
                for x in v {
                    collect_meshes(x, f);
                }
            }
            _ => {}
        }
    }

    /// ⑤:`icon_side` 的三条契约。
    ///
    /// 上面那条 DPI 测试验不到这里(见它的注释),所以边界单独钉:
    /// - **正常标题条**(内高 24 点)下图标恒装得进内容区,且留着呼吸空间;
    /// - **上界 16**:再大就压过主机名 —— 这是观感约束,不是安全约束;
    /// - **下界 10**:极小 pane 上 `layout_geometry` 用
    ///   `title_bar_px(ppp).min(px.h)` 把标题条压扁,那时**故意**不再跟着缩,
    ///   宁可糊得整齐也不要缩成一坨。所以内高小于 12 点时图标确实会被
    ///   `set_clip_rect` 裁 —— 这是有意为之的取舍,不是 bug,钉在这里免得
    ///   将来有人当 bug「修」掉。
    ///
    /// 自证会变红:把 clamp 上限改成 `f32::MAX`(24.0/100.0 两条红);把下限
    /// 改成 0.0(6.0/0.0 两条红);把 `- 2.0` 去掉(14.0 那条红 —— **24.0 那条
    /// 抓不到这处删改**:24-2=22 与 24 都会被上界 16 吃掉,结果一样;14.0 落在
    /// 两个边界都够不着的区间,`icon_side(14.0)` 才会因为少了这 2 点从
    /// 12.0 变成 14.0)。
    #[test]
    fn icon_side_stays_inside_the_content_box_until_the_bar_is_squashed() {
        // 正常标题条:32 点高、上下各 4 点边距 → 内高 24 点,超出上界。
        assert!(icon_side(24.0) < 24.0, "正常内高下图标该装得进去");
        assert_eq!(icon_side(24.0), 16.0, "正常内高下取上界 16");
        // 两个边界都够不着的中间地带:直接体现 `- 2.0` 这个呼吸空间。
        assert_eq!(icon_side(14.0), 12.0, "两个边界都够不着时应是内高减 2");
        // 上界:标题条再高图标也不跟着长。
        assert_eq!(icon_side(100.0), 16.0);
        // 下界:压扁到这个程度就不再缩了,图标反而比内高大 —— 有意为之。
        assert_eq!(icon_side(6.0), 10.0);
        assert_eq!(icon_side(0.0), 10.0);
    }

    /// ④:焦点分屏的标题条要叠一层半透明 accent。
    ///
    /// 判据取「有一个覆盖整条标题条、且 alpha < 255 的填充」——
    /// 不能只数形状个数(加一个别的装饰就跟着变绿),也不能比精确颜色
    /// (`gamma_multiply` 的结果依赖 egui 内部的 gamma 曲线,钉死就成了
    /// 测 egui 而不是测我们)。
    ///
    /// 自证会变红:把 `if v.focused` 那段 `rect_filled` 删掉;或把
    /// `FOCUS_TINT` 改成 1.0(alpha 变 255,不再是「薄」的一层)。
    #[test]
    fn the_focused_panes_title_bar_is_tinted_with_translucent_accent() {
        let id = PaneId(3);
        let g = geom_800x600_title32(id.0, 1.0);
        let full = egui::Rect::from_min_size(
            egui::pos2(g.title_px.x as f32, g.title_px.y as f32),
            egui::vec2(g.title_px.w as f32, g.title_px.h as f32),
        );

        let tinted = |focused: bool| {
            let views = [TitleView {
                geom: g,
                index: 1,
                host: Some("h"),
                status: PaneStatus::Live,
                focused,
                appearance: None,
                cwd_leaf: None,
                tmux: None,
                notice: None,
            }];
            let ctx = egui::Context::default();
            let t = crate::theme::MULLION_DARK;
            // **必须显式把时间推过 `Area` 的 fade_in**(同 `click_button` /
            // `icon_backdrop_uses_the_pane_title_target_not_list_item` 的坑):
            // 淡入没走完时,连不透明的 `panel_bg` 底色本身都会被 opacity 缩成
            // 半透明,被误判成第二层「半透明覆盖」,现象是「有 accent 那层,
            // 但计数多了一个」。
            let mut shapes = Vec::new();
            for t_sec in [0.0, 1.0] {
                shapes = ctx
                    .run(
                        egui::RawInput {
                            time: Some(t_sec),
                            ..Default::default()
                        },
                        |ctx| {
                            show(ctx, &t, &views);
                        },
                    )
                    .shapes;
            }
            let mut hits = 0;
            for cs in &shapes {
                count_translucent_covers(&cs.shape, full, &mut hits);
            }
            hits
        };

        assert_eq!(tinted(true), 1, "焦点标题条上该有一层半透明 accent");
        assert_eq!(tinted(false), 0, "非焦点标题条不该有任何半透明覆盖");
    }

    fn count_translucent_covers(s: &egui::Shape, full: egui::Rect, hits: &mut usize) {
        match s {
            egui::Shape::Rect(r) => {
                let a = r.fill.a();
                if a > 0 && a < 255 && r.rect.contains_rect(full.shrink(0.5)) {
                    *hits += 1;
                }
            }
            egui::Shape::Vec(v) => {
                for x in v {
                    count_translucent_covers(x, full, hits);
                }
            }
            _ => {}
        }
    }

    /// ⑥:目录名取最后一级。
    #[test]
    fn dir_leaf_takes_the_last_component() {
        assert_eq!(dir_leaf(b"/home/dev/Mullion").as_deref(), Some("Mullion"));
        assert_eq!(dir_leaf(b"/home/dev/Mullion/").as_deref(), Some("Mullion"));
        assert_eq!(
            dir_leaf(b"/").as_deref(),
            Some("/"),
            "根目录自己就是最后一级"
        );
        assert_eq!(dir_leaf(b"~").as_deref(), Some("~"));
        assert_eq!(dir_leaf(b"~/Mullion").as_deref(), Some("Mullion"));
        assert_eq!(dir_leaf(b""), None, "空路径没有最后一级,别显示一个空标签");
    }

    /// 连续尾斜杠曾经会让整段目录名消失(`strip_suffix` 只剥一层,`/tmp//`
    /// 剥一次还剩 `/tmp/`,`leaf` 算出来是空串,直接 `return None`)。
    /// 这类值可能从 `mullion-term::parse_title`(按空白切 token,不做路径
    /// 规范化)流进来,不能假设远端只给单斜杠结尾。
    ///
    /// 自证会变红:把 `dir_leaf` 里剥全部尾斜杠的 `while` 循环换回单次
    /// `cwd.strip_suffix(b"/").unwrap_or(cwd)`。
    #[test]
    fn dir_leaf_strips_all_trailing_slashes_not_just_one() {
        assert_eq!(dir_leaf(b"/tmp//").as_deref(), Some("tmp"));
    }

    /// 全是斜杠、没有可显示的末级——**有意保留**的现状:剥完尾斜杠只剩空串,
    /// 走到「空路径没有最后一级」同一条分支,给 `None` 而不是伪造一个标签。
    #[test]
    fn dir_leaf_of_all_slashes_is_none() {
        assert_eq!(
            dir_leaf(b"//"),
            None,
            "剥完尾斜杠只剩空串,没有可显示的末级,有意给 None"
        );
    }

    /// 非 UTF-8 的远端路径不能让标题条整段消失 —— 显示层宁可出一个 `�`
    /// 也要把目录名摆出来(用户至少知道自己在哪个目录)。
    ///
    /// 自证会变红:把 `dir_leaf` 里的 `to_string_lossy` 换成
    /// `std::str::from_utf8(..).ok()?`。
    #[test]
    fn a_non_utf8_dir_name_is_shown_lossily_rather_than_dropped() {
        let leaf = dir_leaf(b"/tmp/\xff\xfe").expect("非 UTF-8 也要给出个东西来");
        assert!(!leaf.is_empty());
    }

    /// **右区不许把 `Area` 撑出 `title_px`**(本文件顶部越界坑第 1 条):
    /// 撑出去就会横向侵入右边邻居、纵向盖住终端第一行,吃掉本该属于终端的
    /// 指针事件(T8 变体)。长目录名 + 长 tmux 名 + 高 DPI 一起上。
    ///
    /// **这条测试实际守住的是 `show()` 末尾 `ui.allocate_rect(full, ..)` 这条
    /// 既有防线本身**(不是本任务新写的右区代码):`content` 是个独立
    /// `UiBuilder` 建的 `new_child`,不参与父 `ui` 的尺寸折算;`show()` 最后
    /// 显式 `allocate_rect(full, ..)` 把 `Area` 的占用尺寸硬钉成 `full`,跟
    /// `content` 里长什么样完全解耦。**因此把右区换成任何实现(哪怕是不截断
    /// 的 `Label::new(s)`,已实测)都验不出来**——那条防线挡在更外层,右区
    /// 自己怎么写都到不了能撑爆 `Area` 的位置。
    ///
    /// 自证会变红(冲着真正被这条测试钉住的机制去):把 `show()` 末尾的
    /// `ui.allocate_rect(full, egui::Sense::hover())` 换成
    /// `ui.allocate_rect(full.shrink(50.0), egui::Sense::hover())`。已实测:
    /// `ppp=1: Area 是 [750.0 0.0],该是 800×32`。
    #[test]
    fn a_long_cwd_and_tmux_name_do_not_push_the_area_past_title_px() {
        for ppp in [1.0_f32, 1.5, 2.0] {
            let id = PaneId(9);
            let g = geom_800x600_title32(id.0, ppp);
            let views = [TitleView {
                geom: g,
                index: 1,
                host: Some("dev@a-very-long-hostname-that-eats-the-row"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
                cwd_leaf: Some("a-directory-name-that-is-also-absurdly-long".to_string()),
                tmux: Some("a-tmux-session-name-that-is-long-too"),
                notice: None,
            }];

            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let t = crate::theme::MULLION_DARK;
            for _ in 0..2 {
                let _ = ctx.run(Default::default(), |ctx| {
                    show(ctx, &t, &views);
                });
            }
            let rect = ctx
                .memory(|m| m.area_rect(area_id(id)))
                .expect("标题条的 Area 该存在");
            let want_w = g.title_px.w as f32 / ppp;
            let want_h = g.title_px.h as f32 / ppp;
            assert!(
                (rect.width() - want_w).abs() < 0.5 && (rect.height() - want_h).abs() < 0.5,
                "ppp={ppp}: Area 是 {:?},该是 {want_w}×{want_h}",
                rect.size()
            );
        }
    }
}
