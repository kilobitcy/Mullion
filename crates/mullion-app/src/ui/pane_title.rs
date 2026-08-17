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
}

/// 标题条上的文字。抽成纯函数是因为格式会被人反复调,而它是唯一能自动验的部分。
pub fn title_text(index: usize, host: Option<&str>, status: PaneStatus) -> String {
    match (host, status) {
        (Some(h), PaneStatus::Live) => format!("{index} · {h}"),
        (Some(h), PaneStatus::Disconnected) => format!("{index} · {h} (已断开)"),
        // (None, Disconnected) 并进这条通配分支是安全的,不是漏判:状态机里
        // host == None 当且仅当 PaneState 还没挂上(见 Workspace::apply_preset),
        // 此时 status 走默认的 Live;一旦 PaneState 存在,host_ix 必指向真实的
        // HostConn,host 必为 Some。这个组合在当前状态机下不可达。
        (None, _) => format!("{index} · 连接中…"),
    }
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

/// 两个按钮的 id。**显式给**而不是让 egui 按布局位置自动生成:测试要
/// `ctx.read_response(id)` 取回矩形才点得中它们,而自动 id 依赖布局顺序,
/// 一改布局就失效。
fn close_id(id: PaneId) -> egui::Id {
    egui::Id::new(("pane_title_close", id.0))
}
fn rehost_id(id: PaneId) -> egui::Id {
    egui::Id::new(("pane_title_rehost", id.0))
}

/// 标题条上的一个小按钮。
///
/// **手动 `allocate` + `interact`,不用 `ui.small_button`**:后者的 id 由布局
/// 顺序自动生成,测试没法 `ctx.read_response(id)` 取回矩形去点它 ——「点了有没有
/// 反应」「两个挨着的按钮串没串」就永远测不到,而「想换节点结果把 pane 关了」
/// 是一次不可撤销的误操作。
///
/// 尺寸按字形实测宽度算,不写死:`×` 和 `⇆` 在不同字体下宽度不同,写死会让
/// 其中一个要么被裁一半、要么留一大块空白。
fn small_action_button(ui: &mut egui::Ui, id: egui::Id, glyph: &str, t: &Theme) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(glyph.to_string(), font, theme::c32(t.fg_muted));
    let size = galley.size() + egui::vec2(8.0, 2.0);
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
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, color);
    resp
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
                ui.painter().rect(
                    full,
                    0.0,
                    theme::c32(if v.focused { t.panel_head } else { t.panel_bg }),
                    theme::stroke(t),
                );
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
                    if small_action_button(ui, close_id(v.geom.id), "×", t)
                        .on_hover_text("关闭此分屏")
                        .clicked()
                    {
                        action.close = Some(v.geom.id);
                    }
                    // 用户要的入口:分屏之后在这里把这块 pane 换到别的节点。
                    // 放在 × 左边而不是做成"点主机名":主机名会被 `.truncate()`
                    // 截断,长主机名时点击靶子会缩到几个像素宽。
                    if small_action_button(ui, rehost_id(v.geom.id), "⇆", t)
                        .on_hover_text("把这块分屏换到别的节点")
                        .clicked()
                    {
                        action.rehost = Some(v.geom.id);
                    }
                    // 剩下的空间(已扣掉 × 按钮)左对齐摆状态点 + 主机名;主机名
                    // 排版用的 available_width 到这里已经是扣掉 × 之后的余量。
                    // × 不被顶出条外,靠的是 right_to_left 先占位 + 外层 set_clip_rect
                    // 裁剪(clip 同时裁交互,见 egui `Ui::interact`,egui-0.30 `ui.rs:1057`
                    // 的 `interact_rect: self.clip_rect().intersect(rect)`);
                    // Area 的外部几何已被
                    // `allocate_rect(full, ..)` 硬钉死,与内容截不截断完全解耦。
                    // `.truncate()` 现在唯一的作用是视觉观感(省略号 vs 硬裁切),
                    // 不是防止 × 被顶出条外的兜底——删掉它 12 条测试仍全绿。
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // F61:图标画在状态点之前。`content` 是个 `new_child` +
                        // `set_clip_rect`,画多了只会被裁掉,不会把 `Area` 撑大。
                        if let Some(icon) = v.appearance.and_then(|a| a.icon.as_ref()) {
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
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
                            PaneStatus::Disconnected => t.fg_dim,
                        };
                        ui.colored_label(theme::c32(dot), "●");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(title_text(v.index, v.host, v.status)).color(
                                    theme::c32(if v.focused { t.fg_strong } else { t.fg_muted }),
                                ),
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

    #[test]
    fn title_shows_index_and_host() {
        assert_eq!(
            title_text(2, Some("dev@build-01"), PaneStatus::Live),
            "2 · dev@build-01"
        );
    }

    /// §6.3:断开的 pane 内容留着可滚可复制,但状态必须写在脸上 ——
    /// 不然用户会对着一块不响应的终端反复敲键。
    #[test]
    fn disconnected_pane_says_so() {
        let s = title_text(1, Some("h"), PaneStatus::Disconnected);
        assert!(s.contains("已断开"), "断开状态没写进标题: {s}");
    }

    /// 预分配了叶子位但 channel 还没开好的空窗期(见 Workspace::apply_preset)。
    #[test]
    fn pane_without_a_host_yet_says_connecting() {
        assert_eq!(title_text(3, None, PaneStatus::Live), "3 · 连接中…");
    }

    fn geom_800x600_title32(id: u32) -> PaneGeom {
        use crate::shell::workspace::{PxRect, TITLE_BAR_PX};
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
                h: TITLE_BAR_PX,
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
        use crate::shell::workspace::TITLE_BAR_PX;
        for ppp in [1.0f32, 1.25, 1.5] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let views = [TitleView {
                geom: geom_800x600_title32(1),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
            }];
            let _ = ctx.run(Default::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, &views);
            });
            let rect = ctx
                .memory(|m| m.area_rect(area_id(PaneId(1))))
                .unwrap_or_else(|| panic!("ppp={ppp}: 标题条没画出任何 Area"));
            let want_w = 800.0 / ppp;
            let want_h = TITLE_BAR_PX as f32 / ppp;
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
        use crate::shell::workspace::{PxRect, TITLE_BAR_PX};
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
                h: TITLE_BAR_PX,
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
            geom: geom_800x600_title32(1),
            index: 1,
            host: Some("dev@build-01"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
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
            geom: geom_800x600_title32(1),
            index: 1,
            host: Some("dev@build-01"),
            status: PaneStatus::Live,
            focused: true,
            appearance,
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
        use crate::shell::workspace::TITLE_BAR_PX;
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
                geom: geom_800x600_title32(1),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: Some(&a),
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
                (rect.height() - TITLE_BAR_PX as f32 / ppp).abs() < 0.5,
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
                geom: geom_800x600_title32(1),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance,
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
}
