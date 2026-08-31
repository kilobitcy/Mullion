//! ③④:分屏之间的分界线与当前焦点分屏的描边。
//!
//! **两件事都画在既有像素里,不改任何几何。** 分界线落在 `GAP_PX` 那条缝上
//! (`layout_geometry` 里非最右/最下的 pane 各让出 1 物理像素);焦点描边落在
//! pane `px` 的内边界上。给焦点 pane 缩终端区是不行的 —— 那会改 `grid`,
//! 每切一次焦点都要发 `window_change`(T4),远端 TUI 每点一下重排一次。
//!
//! **用 `ctx.layer_painter` 而不是 `egui::Area`**:`Area` 会 allocate 一块
//! 可交互矩形,盖在终端上就把指针事件吃了(T8 的指针路由是「先喂 egui
//! 后判」),划选当场失效。`layer_painter` 只画不占。层序取 `Order::Background`:
//! egui 整层 composite 在 wgpu 自绘的终端之上,所以 `Background` 也在终端
//! 之上,同时在面板/弹窗之下 —— 不会盖住模态框。

use crate::shell::workspace::{term_pad_px, PaneGeom, PxRect, GAP_PX};
use crate::theme::{self, Theme};
use crate::ui::pane_title::TitleView;

/// 这块 pane 让给分界线的两条缝:`(右缘竖线, 下缘横线)`。没让位就是 `None`。
///
/// `ppp` 收的是**算 `g` 时用的那个** `scale_factor`,内缩量由本函数自己
/// `term_pad_px(ppp)` 出来 —— 不收算好的 `pad: u32`:那是个没有单位的裸整数,
/// 传成 `GAP_PX` 或用错 ppp 算出来的值编译器都拦不住,只会在特定 DPI 下画错线。
///
/// 内缩量参与判据是因为:竖线判据不能再单纯看
/// 「`term_px.w` 比 `px.w` 小」:F80 之后**每个** pane 的 `term_px` 都会因为
/// 内缩而比 `px` 窄,不分是否真的挨着邻居。判据改成「比只扣内缩之后还窄
/// 超过 `2*pad`」——多出来的那部分才是让给分隔线的 `GAP_PX`。竖线的判断因此
/// 仍然与 `layout_geometry` 里 `at_right` 同源(只是隔着 `pad` 这个已知量间接
/// 对比,而不是重新推一遍「谁在边上」),横线不受横向内缩影响,判据不变。
///
/// 竖线只跨 `term_px` 的纵向范围:标题条那一段的同一列像素由标题条自己的
/// 底色填满,再画一道会在标题条上多出一截亮线。
///
/// 竖线的 `x` **不能**再用 `g.px.x + g.term_px.w` 算 —— 那条公式假设
/// `term_px.x == px.x`(F80 之前成立),F80 内缩后 `term_px.x = px.x + pad`,
/// 沿用旧公式会把竖线画到 `[px.x+term_px.w, ..)`,那正好落在 `term_px` 内部
/// (`term_px.w` 本身已经把 pad 扣过一次),等于把分隔线画穿了终端内容。
/// 让给分隔线的那 1px 恒是这块 pane **自己 `px` 区间的最后一列**(设计见模块
/// 文档:「非最右/最下的 pane 在 `term_px` 上各让出 1px,分隔线画在让出来的
/// 缝里」),所以直接从 `px` 反推:`x = px.x + px.w - GAP_PX`。
pub fn divider_lines_of(g: &PaneGeom, ppp: f32) -> (Option<PxRect>, Option<PxRect>) {
    let pad = term_pad_px(ppp);
    let right = (g.px.w.saturating_sub(g.term_px.w) > 2 * pad).then(|| PxRect {
        x: g.px.x + g.px.w - GAP_PX,
        y: g.term_px.y,
        w: GAP_PX,
        h: g.term_px.h,
    });
    let bottom = (g.term_px.y + g.term_px.h < g.px.y + g.px.h).then(|| PxRect {
        x: g.px.x,
        y: g.term_px.y + g.term_px.h,
        w: g.px.w,
        h: GAP_PX,
    });
    (right, bottom)
}

/// 焦点描边的宽度(逻辑点)。F206 起搬进 `theme`,三处共用一份 ——
/// 这里只是个转发别名,`shrink(RING_W / 2)` 那句读起来才不至于换行。
const RING_W: f32 = theme::FOCUS_RING_W;

/// 画分界线 + 焦点描边。
///
/// `views` 就是本帧交给 [`crate::ui::pane_title::show`] 的那一份 ——
/// 几何与焦点都从它取,**不新开第二条几何来源**(开了就会有两份真值,
/// 布局一改分界线就跟 pane 错位)。
pub fn paint(ctx: &egui::Context, t: &Theme, views: &[TitleView<'_>]) {
    let ppp = ctx.pixels_per_point();
    // `layer_painter` 而不是 `Area`:见模块文档(T8)。
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("pane_edges"),
    ));
    // 物理像素 → 逻辑点。**不做 `.max(1.0)` 之类的加粗**:`PxRect` 的坐标
    // 是整数物理像素,除以 ppp 再乘回去正好落在像素边界上,1 物理像素宽的
    // 矩形光栅化出来就是 1 个像素,不糊。
    let to_pt = |r: PxRect| {
        egui::Rect::from_min_size(
            egui::pos2(r.x as f32 / ppp, r.y as f32 / ppp),
            egui::vec2(r.w as f32 / ppp, r.h as f32 / ppp),
        )
    };
    for v in views {
        let (right, bottom) = divider_lines_of(&v.geom, ppp);
        for line in [right, bottom].into_iter().flatten() {
            p.rect_filled(to_pt(line), 0.0, theme::c32(t.divider));
        }
        if v.focused {
            // `shrink(RING_W / 2)`:egui 的描边以路径为中心向两侧各铺一半,
            // 不缩的话有半个像素落在 pane 之外、压到邻居身上。
            p.rect_stroke(
                to_pt(v.geom.px).shrink(RING_W / 2.0),
                theme::FOCUS_RING_ROUNDING,
                theme::focus_ring(t),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::workspace::layout_geometry;
    use mullion_core::layout::{Dir, Node, PaneId};

    const AREA: PxRect = PxRect {
        x: 0,
        y: 0,
        w: 800,
        h: 600,
    };
    const CELL: (f32, f32) = (10.0, 20.0);

    // 与 `geom.rs::tests` 同款的树构造小工具 —— core 没有 `Tree` 类型,
    // 布局树就是 `Node`,这里照抄那边既有的写法。
    fn leaf(id: u32) -> Node {
        Node::Leaf(PaneId(id))
    }
    fn hsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }
    fn vsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// 单 pane 没有邻居,一条线都不该画 —— 画了就是在终端最外一列/一行上
    /// 糊一道亮线,而那里根本没有缝。
    ///
    /// 自证会变红:把 `divider_lines_of` 的两个判断改成恒 `true`。
    #[test]
    fn a_lone_pane_has_no_divider_at_all() {
        let g = layout_geometry(&leaf(1), AREA, CELL, false, 1.0);
        assert_eq!(divider_lines_of(&g[0], 1.0), (None, None));
    }

    /// ③ 的核心几何约定:竖线必须落在**两块 pane 的 `term_px` 都没占用**的
    /// 那 1 像素上。压进任一块 `term_px` 就是盖掉终端最外一列字形 ——
    /// 用户会看到行尾/行首的字被切掉一条,而且只在分屏时出现,极难归因。
    ///
    /// 自证会变红:把竖线的 x 改成 `g.px.x + g.term_px.w - 1`(压左 pane)
    /// 或 `g.px.x + g.px.w`(压右 pane)。
    #[test]
    fn the_vertical_divider_lands_in_the_gap_that_no_pane_owns() {
        let tree = hsplit(0.5, leaf(1), leaf(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        assert_eq!(g.len(), 2, "该分出两块");

        let (right, bottom) = divider_lines_of(&g[0], 1.0);
        let line = right.expect("左 pane 该有一条右缘竖线");
        assert_eq!(bottom, None, "左右分屏不该有横线");
        assert_eq!(line.w, GAP_PX, "分界线就是那条缝的宽度,不许更宽");

        for p in &g {
            let te = p.term_px;
            assert!(
                line.x >= te.x + te.w || line.x + line.w <= te.x,
                "竖线 {line:?} 压在 pane {:?} 的终端区 {te:?} 上",
                p.id
            );
        }
        assert_eq!(
            divider_lines_of(&g[1], 1.0),
            (None, None),
            "最右那块没让出缝,不该再画线(会画到窗口边缘外/邻居身上)"
        );
    }

    /// 上下分屏的对偶。**单独一条**而不是并进上面那条:横线画在上 pane 的
    /// **下缘**,与竖线画在**右缘**是两条独立的几何路径(对应 `layout_geometry`
    /// 里 `term_px.h` 的双重 `saturating_sub` 与 `term_px.w` 的单重),算错的话
    /// 线会压进上 pane 的终端区、切掉最后一行。
    ///
    /// 自证会变红:把横线的 y 改成 `g.term_px.y`(整条线跳到终端区顶上,
    /// 横穿第一行)。
    ///
    /// **注意 `g.px.y + g.px.h - 1` 这个写法验不出来** —— 让出缝的 pane 恒有
    /// `term_px.y + term_px.h == px.y + px.h - GAP_PX`,而 `GAP_PX == 1`,两个
    /// 表达式代数恒等。真正守住这条线的是下面「不许压在任一 pane 的 term_px
    /// 上」那个不等式,不是这个自证操作。
    #[test]
    fn the_horizontal_divider_lands_in_the_gap_that_no_pane_owns() {
        let tree = vsplit(0.5, leaf(1), leaf(2));
        let g = layout_geometry(&tree, AREA, CELL, true, 1.0);

        let (right, bottom) = divider_lines_of(&g[0], 1.0);
        assert_eq!(right, None, "上下分屏不该有竖线");
        let line = bottom.expect("上 pane 该有一条下缘横线");
        assert_eq!(line.h, GAP_PX);

        for p in &g {
            let te = p.term_px;
            assert!(
                line.y >= te.y + te.h || line.y + line.h <= te.y,
                "横线 {line:?} 压在 pane {:?} 的终端区 {te:?} 上",
                p.id
            );
        }
        assert_eq!(divider_lines_of(&g[1], 1.0), (None, None));
    }

    fn view(g: PaneGeom, index: usize, focused: bool) -> TitleView<'static> {
        TitleView {
            geom: g,
            index,
            host: Some("h"),
            status: crate::shell::workspace::PaneStatus::Live,
            focused,
            appearance: None,
            cwd_leaf: None,
            tmux: None,
            notice: None,
        }
    }

    /// 画一帧,返回所有形状。两帧是因为 egui 的部件首帧只是「上帧」——
    /// 本文件不建 `Area`,一帧其实就够,但保持与 `pane_title` 测试同一套
    /// 惯例,免得将来加了 `Area` 才发现要补第二帧。
    fn run_shapes(views: &[TitleView<'_>]) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let mut out = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(Default::default(), |ctx| paint(ctx, &t, views));
            out = full.shapes.into_iter().map(|c| c.shape).collect();
        }
        out
    }

    fn strokes_colored(shapes: &[egui::Shape], want: egui::Color32) -> usize {
        shapes
            .iter()
            .filter(|s| {
                matches!(s, egui::Shape::Rect(r)
                    if r.stroke.width > 0.0 && r.stroke.color == want)
            })
            .count()
    }

    /// ④:焦点分屏要有一圈 accent 描边,**标题条被 F83 关掉时也要有** ——
    /// 关掉之后标题条那层 tint 就不存在了,描边是唯一的焦点提示。用户的要求
    /// 是「一眼从众多分屏中找到当前获得焦点的那个」。
    ///
    /// 自证会变红:把 `paint` 里 `if v.focused` 那段删掉;或给它加一条
    /// 「标题条关了就不画」的短路。
    #[test]
    fn the_focused_pane_gets_an_accent_ring_even_without_a_title_bar() {
        let tree = hsplit(0.5, leaf(1), leaf(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, true), view(g[1], 2, false)];

        let accent = theme::c32(crate::theme::MULLION_DARK.accent);
        assert_eq!(
            strokes_colored(&run_shapes(&views), accent),
            1,
            "该恰好一圈 accent 描边(焦点那块),不多不少"
        );
    }

    /// 没有焦点(极端情形:焦点 pane 刚被关掉、下一帧才补上)时一圈都不画 ——
    /// 画错位置比不画更糟:用户会以为焦点在别处,对着错的分屏敲键。
    ///
    /// 自证会变红:把 `if v.focused` 改成恒 `true`。
    #[test]
    fn no_ring_is_painted_when_nothing_has_focus() {
        let g = layout_geometry(&leaf(1), AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, false)];
        let accent = theme::c32(crate::theme::MULLION_DARK.accent);
        assert_eq!(strokes_colored(&run_shapes(&views), accent), 0);
    }

    /// ③ 的绘制腿:分界线真的画出来了,且用的是 `divider` 那一档色
    /// (不是 `stroke` 的白 6%,那个在 1px + 深底下看不见 —— 见
    /// `theme::tests::the_divider_is_visible_but_not_loud_against_the_terminal_background`)。
    ///
    /// 自证会变红:把 `rect_filled` 的颜色改成 `theme::c32(t.term_bg)`;
    /// 或把画分界线那个 `for` 循环删掉。
    #[test]
    fn the_divider_is_actually_filled_with_the_divider_color() {
        let tree = hsplit(0.5, leaf(1), leaf(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, false), view(g[1], 2, false)];

        let want = theme::c32(crate::theme::MULLION_DARK.divider);
        let n = run_shapes(&views)
            .iter()
            .filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == want))
            .count();
        assert_eq!(n, 1, "左右分屏该恰好一条 divider 色的填充(那条竖线)");
    }
}
