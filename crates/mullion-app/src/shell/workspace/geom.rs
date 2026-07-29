//! 分屏几何:布局树 → 每 pane 的像素矩形 → 网格尺寸(F30/F80/F83)。
//!
//! 布局树跑在**像素**空间而不是格:32px 标题条和 1px 分隔线都不是格的整数倍,
//! 先算格再扣 chrome 会一路掉精度(设计文档 §4.1)。三步:
//!   1. `compute_rects(树, 整个终端区像素矩形)` → 每 pane 的像素矩形
//!   2. 扣掉 32px 标题条(若开)、扣掉 1px 分隔线让位 → 终端网格区像素矩形
//!   3. `grid::grid_size_for(终端区像素, cell_w, cell_h)` → (cols, rows)
//!
//! **渲染、鼠标命中、window_change 三者读的是同一份 `Vec<PaneGeom>`**,任何一处
//! 自己另算一遍,迟早三者对不上(字画在 A、点击命中 B、远端按 C 排版)。

use mullion_core::layout::{compute_rects, Node, PaneId, Rect};

/// 窗口像素矩形,左上原点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// pane 标题条高度(F83)。
pub const TITLE_BAR_PX: u32 = 32;

/// 相邻 pane 之间的分隔线宽度(F80)。
///
/// core 的布局语义是「严丝合缝拼满、不为分隔条留格」,这条**不动**;让位完全是
/// app 侧的事 —— 非最右/最下的 pane 在 `term_px` 上各让出 1px,分隔线画在让出来
/// 的缝里。改 core 去扣格会把「拼满不重叠」那条不变量一起破坏掉。
pub const GAP_PX: u32 = 1;

/// 一个 pane 的全部几何。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeom {
    pub id: PaneId,
    /// 整块(含标题条,不含让给分隔线的那 1px)。
    pub px: PxRect,
    /// 标题条;标题条关闭时 `h == 0`。
    pub title_px: PxRect,
    /// 终端网格区(已扣标题条与分隔线让位)。极小窗口下 `w`/`h` 可能降到 0 ——
    /// 这不代表 `grid` 也会是 0(见下),二者不同步是有意为之,不是 bug。
    pub term_px: PxRect,
    /// `term_px` 落成的 (cols, rows)。由 `grid_size_for` 夹到至少 `(1, 1)`,
    /// 即便 `term_px` 已经缩到 0 像素(PTY 侧不接受 0 列/0 行)。
    pub grid: (u16, u16),
}

/// 算出每个 pane 的完整几何。
///
/// `area` 是整个终端区的像素矩形(窗口扣掉 egui 菜单栏/工具栏/状态栏之后剩下的)。
/// `cell` 是字元像素尺寸 `(宽, 高)`。`title_bars` 对应 F83 开关。
pub fn layout_geometry(
    tree: &Node,
    area: PxRect,
    cell: (f32, f32),
    title_bars: bool,
) -> Vec<PaneGeom> {
    // u16 承载像素:4K 宽 3840 远低于 65535。超大屏(理论上 >65535px)饱和截断,
    // 画面会不对但不会静默回绕成小值。
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let root = Rect {
        col: clamp(area.x),
        row: clamp(area.y),
        cols: clamp(area.w),
        rows: clamp(area.h),
    };
    let right_end = u32::from(root.col) + u32::from(root.cols);
    let bottom_end = u32::from(root.row) + u32::from(root.rows);

    compute_rects(tree, root)
        .into_iter()
        .map(|(id, r)| {
            let px = PxRect {
                x: u32::from(r.col),
                y: u32::from(r.row),
                w: u32::from(r.cols),
                h: u32::from(r.rows),
            };
            // 标题条不能比 pane 本身还高(窗口被拖到极小时会发生)。
            let title_h = if title_bars {
                TITLE_BAR_PX.min(px.h)
            } else {
                0
            };
            let at_right = px.x + px.w >= right_end;
            let at_bottom = px.y + px.h >= bottom_end;
            let term_px = PxRect {
                x: px.x,
                y: px.y + title_h,
                w: px.w.saturating_sub(if at_right { 0 } else { GAP_PX }),
                h: px
                    .h
                    .saturating_sub(title_h)
                    .saturating_sub(if at_bottom { 0 } else { GAP_PX }),
            };
            PaneGeom {
                id,
                px,
                title_px: PxRect {
                    x: px.x,
                    y: px.y,
                    w: px.w,
                    h: title_h,
                },
                term_px,
                grid: crate::grid::grid_size_for(term_px.w, term_px.h, cell.0, cell.1),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: u32) -> Node {
        Node::Leaf(PaneId(id))
    }
    fn hsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: mullion_core::layout::Dir::Horizontal,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }
    fn vsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: mullion_core::layout::Dir::Vertical,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// 800x600 的终端区,字元 10x20。
    const AREA: PxRect = PxRect {
        x: 0,
        y: 100,
        w: 800,
        h: 600,
    };
    const CELL: (f32, f32) = (10.0, 20.0);

    #[test]
    fn single_pane_fills_area_and_yields_no_gap() {
        let g = layout_geometry(&leaf(1), AREA, CELL, false);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].px, AREA);
        assert_eq!(g[0].term_px, AREA, "只有一个 pane 时不该让任何像素给分隔线");
        assert_eq!(g[0].grid, (80, 30));
    }

    #[test]
    fn inner_pane_yields_one_pixel_for_the_divider_f80() {
        let g = layout_geometry(&hsplit(0.5, leaf(1), leaf(2)), AREA, CELL, false);
        assert_eq!(g[0].px.w, 400);
        assert_eq!(g[0].term_px.w, 399, "左 pane 要让 1px 给竖分隔线");
        assert_eq!(g[1].px.w, 400);
        assert_eq!(g[1].term_px.w, 400, "最右 pane 右边没有邻居,不让位");
        // 让位只吃掉不足一格的余量,列数不变。
        assert_eq!(g[0].grid, (39, 30));
        assert_eq!(g[1].grid, (40, 30));
    }

    /// 镜像 `inner_pane_yields_one_pixel_for_the_divider_f80`,验证纵向(`Dir::Vertical`,
    /// 上下堆叠、分隔线是横线)那条让位路径 —— 之前只有横向(`Dir::Horizontal`)有断言覆盖,
    /// `at_bottom` 判定与 `term_px.h` 的双重 `saturating_sub` 从未被跑过。
    ///
    /// 期望值手算:AREA 高 600、y=100,vsplit(0.5) → 上 pane px.h=300(row 100..400),
    /// 下 pane px.h=300(row 400..700,恰好等于 bottom_end=700 → at_bottom=true)。
    /// 上 pane 不挨底,让 1px:term_px.h=299 → grid.1 = floor(299/20) = 14。
    /// 下 pane 挨底,不让位:term_px.h=300 → grid.1 = 300/20 = 15。
    #[test]
    fn inner_pane_yields_one_pixel_for_the_horizontal_divider_f80() {
        let g = layout_geometry(&vsplit(0.5, leaf(1), leaf(2)), AREA, CELL, false);
        assert_eq!(g[0].px.h, 300);
        assert_eq!(g[0].term_px.h, 299, "上 pane 要让 1px 给横分隔线");
        assert_eq!(g[1].px.h, 300);
        assert_eq!(g[1].term_px.h, 300, "最下 pane 下边没有邻居,不让位");
        assert_eq!(g[0].grid, (80, 14));
        assert_eq!(g[1].grid, (80, 15));
    }

    #[test]
    fn grid_excludes_title_bar_f83() {
        let off = layout_geometry(&leaf(1), AREA, CELL, false);
        let on = layout_geometry(&leaf(1), AREA, CELL, true);
        assert_eq!(on[0].title_px.h, TITLE_BAR_PX);
        assert_eq!(off[0].title_px.h, 0);
        assert_eq!(
            on[0].term_px.y,
            AREA.y + TITLE_BAR_PX,
            "终端区必须从标题条下沿开始,否则首行被标题条盖住"
        );
        assert_eq!(on[0].term_px.h, AREA.h - TITLE_BAR_PX);
    }

    /// F83 开关会改行数 → 必须重发 window_change(T4)。这条锁住「行数确实变了」,
    /// 免得后来有人把标题条画成 overlay(不占空间)却忘了同步改注释。
    #[test]
    fn title_bar_toggle_changes_rows_f83() {
        let off = layout_geometry(&leaf(1), AREA, CELL, false);
        let on = layout_geometry(&leaf(1), AREA, CELL, true);
        assert_eq!(off[0].grid, (80, 30));
        assert_eq!(on[0].grid, (80, 28), "600-32=568px / 20px = 28 行");
        assert_ne!(off[0].grid, on[0].grid);
    }

    #[test]
    fn panes_tile_the_area_without_overlap_f30() {
        // 2x2:整块矩形(px,不是 term_px)必须严丝合缝拼满 area。
        // AREA.x 全局恒为 0,单用它 x 下界断言判了也是死比较(clippy 会拒绝),
        // 这条 x != 0 的路径也就从未被跑过;这里换一个 x 非零的矩形,让 x
        // 下界断言变得有意义(能抓住比如误用 right_end 时手滑漏加 col 的偏移 bug)。
        let area = PxRect { x: 50, ..AREA };
        let tree = mullion_core::layout::Node::Split {
            dir: mullion_core::layout::Dir::Vertical,
            ratio: 0.5,
            a: Box::new(hsplit(0.5, leaf(1), leaf(2))),
            b: Box::new(hsplit(0.5, leaf(3), leaf(4))),
        };
        let g = layout_geometry(&tree, area, CELL, true);
        assert_eq!(g.len(), 4);
        let covered: u64 = g
            .iter()
            .map(|p| u64::from(p.px.w) * u64::from(p.px.h))
            .sum();
        assert_eq!(covered, u64::from(area.w) * u64::from(area.h), "未拼满");
        // 面积和相等 + 两两不重叠仍排除不掉「某 pane 越界 + 别处等大空洞」的病态
        // 情形,补一条每个 px 都被 area 完全包含(现在 x 下界也一并判)。
        for p in &g {
            assert!(
                p.px.x >= area.x
                    && p.px.y >= area.y
                    && p.px.x + p.px.w <= area.x + area.w
                    && p.px.y + p.px.h <= area.y + area.h,
                "pane 越界: {:?}",
                p.px
            );
        }
        for (i, a) in g.iter().enumerate() {
            for b in g.iter().skip(i + 1) {
                let overlap = a.px.x < b.px.x + b.px.w
                    && b.px.x < a.px.x + a.px.w
                    && a.px.y < b.px.y + b.px.h
                    && b.px.y < a.px.y + a.px.h;
                assert!(!overlap, "pane 重叠: {:?} vs {:?}", a.px, b.px);
            }
        }
    }

    /// 2x2 嵌套 + 标题条同时生效的组合路径:此前 `title_bars=true` 只在单叶子场景
    /// 用过,`panes_tile_the_area_without_overlap_f30` 虽也是 2x2+标题条,却只断言
    /// `px`(整块外框),从没验证过非边缘 pane 的 `term_px`/`title_px` 精确数值 ——
    /// 而设计文档强调的「三处(渲染/命中/window_change)共用一份几何」,最容易在
    /// 「标题条 + 分隔线让位」同时生效且不在边缘时出岔子(经评审指出)。
    ///
    /// 期望值手算:
    /// tree = vsplit(0.5, hsplit(0.5,l1,l2), hsplit(0.5,l3,l4))
    /// AREA = {x:0,y:100,w:800,h:600} → right_end=800,bottom_end=700。
    ///
    /// vsplit 先把 AREA 纵向对半:上半 y=100..400(h=300),下半 y=400..700(h=300),
    /// x/w 不变(0..800)。每一半再用 hsplit 横向对半,得到四块 px:
    ///   pane1(左上) {x:0,  y:100,w:400,h:300}
    ///   pane2(右上) {x:400,y:100,w:400,h:300}
    ///   pane3(左下) {x:0,  y:400,w:400,h:300}
    ///   pane4(右下) {x:400,y:400,w:400,h:300}
    ///
    /// at_right = px.x+px.w >= 800:pane1/3 → 400,false;pane2/4 → 800,true。
    /// at_bottom = px.y+px.h >= 700:pane1/2 → 400,false;pane3/4 → 700,true。
    /// title_bars=true → title_h = min(32, px.h=300) = 32,四个 pane 一样。
    ///
    /// term_px.x = px.x;term_px.y = px.y+32;
    /// term_px.w = px.w - (at_right?0:1);term_px.h = px.h-32-(at_bottom?0:1)。
    ///
    /// pane1: term_px={x:0,  y:132,w:399,h:267}(不挨右不挨底,双让位都扣)
    /// pane2: term_px={x:400,y:132,w:400,h:267}(挨右不挨底,只扣纵向让位)
    /// pane3: term_px={x:0,  y:432,w:399,h:268}(不挨右挨底,只扣横向让位)
    /// pane4: term_px={x:400,y:432,w:400,h:268}(挨右挨底,两个让位都不扣)
    /// title_px 四个都是 {x:px.x, y:px.y, w:400, h:32}。
    #[test]
    fn nested_2x2_with_title_bars_yields_correct_term_and_title_px_f30_f83() {
        let tree = mullion_core::layout::Node::Split {
            dir: mullion_core::layout::Dir::Vertical,
            ratio: 0.5,
            a: Box::new(hsplit(0.5, leaf(1), leaf(2))),
            b: Box::new(hsplit(0.5, leaf(3), leaf(4))),
        };
        let g = layout_geometry(&tree, AREA, CELL, true);
        assert_eq!(g.len(), 4);

        assert_eq!(
            g[0].px,
            PxRect {
                x: 0,
                y: 100,
                w: 400,
                h: 300
            },
            "左上 pane 整块"
        );
        assert_eq!(
            g[0].title_px,
            PxRect {
                x: 0,
                y: 100,
                w: 400,
                h: 32
            },
            "左上 pane 标题条"
        );
        assert_eq!(
            g[0].term_px,
            PxRect {
                x: 0,
                y: 132,
                w: 399,
                h: 267
            },
            "左上 pane:不挨右不挨底,标题条与横向/纵向让位都要同时扣掉"
        );

        assert_eq!(
            g[1].px,
            PxRect {
                x: 400,
                y: 100,
                w: 400,
                h: 300
            },
            "右上 pane 整块"
        );
        assert_eq!(
            g[1].title_px,
            PxRect {
                x: 400,
                y: 100,
                w: 400,
                h: 32
            },
            "右上 pane 标题条"
        );
        assert_eq!(
            g[1].term_px,
            PxRect {
                x: 400,
                y: 132,
                w: 400,
                h: 267
            },
            "右上 pane:挨右不挨底,只扣标题条 + 纵向让位"
        );

        assert_eq!(
            g[2].px,
            PxRect {
                x: 0,
                y: 400,
                w: 400,
                h: 300
            },
            "左下 pane 整块"
        );
        assert_eq!(
            g[2].title_px,
            PxRect {
                x: 0,
                y: 400,
                w: 400,
                h: 32
            },
            "左下 pane 标题条"
        );
        assert_eq!(
            g[2].term_px,
            PxRect {
                x: 0,
                y: 432,
                w: 399,
                h: 268
            },
            "左下 pane:不挨右挨底,只扣标题条 + 横向让位"
        );

        assert_eq!(
            g[3].px,
            PxRect {
                x: 400,
                y: 400,
                w: 400,
                h: 300
            },
            "右下 pane 整块"
        );
        assert_eq!(
            g[3].title_px,
            PxRect {
                x: 400,
                y: 400,
                w: 400,
                h: 32
            },
            "右下 pane 标题条"
        );
        assert_eq!(
            g[3].term_px,
            PxRect {
                x: 400,
                y: 432,
                w: 400,
                h: 268
            },
            "右下 pane:挨右挨底,只扣标题条,两个让位都不扣"
        );
    }

    /// 窗口被拖到极小时不许 panic、不许算出 0 行 —— grid_size_for 夹到至少 1。
    #[test]
    fn tiny_area_does_not_panic_and_keeps_at_least_one_cell() {
        let tiny = PxRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        let g = layout_geometry(&leaf(1), tiny, CELL, true);
        assert_eq!(g[0].title_px.h, 4, "标题条不能比 pane 还高");
        assert_eq!(g[0].grid, (1, 1));
    }
}
