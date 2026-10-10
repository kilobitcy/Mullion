//! F320:抽屉与父 pane 之间那条分隔线的拖拽 —— 命中与比例换算的纯逻辑。
//!
//! 只认抽屉的那一条线(范围决策:普通分屏的分隔线本切片不动)。没有窗口、
//! 没有 egui:命中和比例都能脱离 GUI 单测,app 侧只负责收鼠标事件、把结果落地。
//!
//! 分隔线本身只有 `GAP_PX` = 1 物理像素,直接命中几乎点不中,所以命中区是
//! 以分界线为中心、上下各扩 [`HIT_HALF_PT`] 点的条带,横跨抽屉整宽。

use super::geom::{title_bar_px, PaneGeom, PxRect};
use super::Drawer;
use mullion_core::layout::PaneId;

/// 命中条带的半高(逻辑点)。上下各这么多,总高是它的两倍。
pub const HIT_HALF_PT: f32 = 3.0;

/// 每侧至少留几行终端(加上标题条),拖到顶/底时的夹紧下限。
pub const MIN_SIDE_ROWS: u32 = 3;

/// 命中到的一条抽屉分隔线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawerDivider {
    pub drawer: PaneId,
    /// 父 pane 与抽屉合起来的矩形(这一条 Split 占的全部像素)。
    pub span: PxRect,
    /// 当前分界线的 y(= 抽屉 `px` 的上沿)。
    pub boundary_y: u32,
}

/// 每侧最少多少像素:标题条 + [`MIN_SIDE_ROWS`] 行。
pub fn min_side_px(ppp: f32, cell_h: f32) -> u32 {
    title_bar_px(ppp) + (MIN_SIDE_ROWS as f32 * cell_h).ceil() as u32
}

/// 指针 `px`(窗口物理像素)压在哪条抽屉分隔线的命中条带上。
///
/// 抽屉必须紧贴在父 pane 正下方(同左缘同宽,父的下沿 = 抽屉的上沿)才算 ——
/// 树被重排成别的形状时宁可不命中,也不去拖一条形状不对的线。
pub fn drawer_divider_at(
    drawers: &[Drawer],
    geoms: &[PaneGeom],
    px: (f32, f32),
    ppp: f32,
) -> Option<DrawerDivider> {
    let half = HIT_HALF_PT * ppp;
    drawers.iter().find_map(|d| {
        let dg = geoms.iter().find(|g| g.id == d.id)?;
        let pg = geoms.iter().find(|g| g.id == d.parent)?;
        let (p, q) = (pg.px, dg.px);
        let stacked = p.x == q.x && p.w == q.w && p.y + p.h == q.y;
        let boundary = q.y as f32;
        let hit = stacked
            && px.0 >= q.x as f32
            && px.0 < (q.x + q.w) as f32
            && (px.1 - boundary).abs() <= half;
        hit.then_some(DrawerDivider {
            drawer: d.id,
            span: PxRect {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h + q.h,
            },
            boundary_y: q.y,
        })
    })
}

/// 指针 y 夹进「两侧都不小于 `min_side`」的范围。放不下两个最小侧时取正中。
pub fn clamp_y(div: &DrawerDivider, y: f32, min_side: u32) -> f32 {
    let top = div.span.y as f32;
    let total = div.span.h as f32;
    if total < 2.0 * min_side as f32 {
        return top + total / 2.0;
    }
    y.clamp(top + min_side as f32, top + total - min_side as f32)
}

/// 指针 y → 父 pane 占的比例(喂给 `Workspace::set_drawer_ratio`)。
pub fn ratio_for(div: &DrawerDivider, y: f32, min_side: u32) -> f32 {
    (clamp_y(div, y, min_side) - div.span.y as f32) / div.span.h as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::workspace::layout_geometry;
    use mullion_core::layout::{Dir, Node};

    const AREA: PxRect = PxRect {
        x: 0,
        y: 40,
        w: 800,
        h: 600,
    };

    fn setup() -> (Vec<Drawer>, Vec<PaneGeom>) {
        let tree = Node::Split {
            dir: Dir::Vertical,
            ratio: 0.8,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        let geoms = layout_geometry(&tree, AREA, (10.0, 20.0), true, 1.0);
        let drawers = vec![Drawer {
            id: PaneId(2),
            parent: PaneId(1),
            cwd: None,
        }];
        (drawers, geoms)
    }

    fn boundary(geoms: &[PaneGeom]) -> f32 {
        geoms.iter().find(|g| g.id == PaneId(2)).unwrap().px.y as f32
    }

    /// 条带内命中、条带外不中,上下对称;横向超出抽屉宽度也不中。
    ///
    /// 自证会变红:把 `<= half` 改成 `< 0.5`(退回只认 1px 线)。
    #[test]
    fn the_hit_band_is_wider_than_the_one_pixel_line_and_symmetric() {
        let (drawers, geoms) = setup();
        let b = boundary(&geoms);
        for dy in [-3.0, -1.0, 0.0, 1.0, 3.0] {
            assert!(
                drawer_divider_at(&drawers, &geoms, (400.0, b + dy), 1.0).is_some(),
                "dy={dy} 该命中"
            );
        }
        for dy in [-5.0, 5.0, 40.0] {
            assert!(
                drawer_divider_at(&drawers, &geoms, (400.0, b + dy), 1.0).is_none(),
                "dy={dy} 不该命中"
            );
        }
        assert!(drawer_divider_at(&drawers, &geoms, (900.0, b), 1.0).is_none());
    }

    /// 条带随缩放放大(逻辑点 × ppp)。
    #[test]
    fn the_band_scales_with_the_display_scale() {
        let (drawers, geoms) = setup();
        let b = boundary(&geoms);
        assert!(drawer_divider_at(&drawers, &geoms, (400.0, b + 5.0), 2.0).is_some());
        assert!(drawer_divider_at(&drawers, &geoms, (400.0, b + 5.0), 1.0).is_none());
    }

    /// 抽屉不在父正下方(形状被重排过)就不命中。
    #[test]
    fn a_drawer_that_is_not_directly_below_its_parent_is_not_draggable() {
        let (mut drawers, geoms) = setup();
        drawers[0].parent = PaneId(99);
        let b = boundary(&geoms);
        assert!(drawer_divider_at(&drawers, &geoms, (400.0, b), 1.0).is_none());
    }

    /// 比例换算:在中线是 0.5;拖到极上/极下被夹在「两侧各留最小」。
    ///
    /// 自证会变红:去掉 `clamp_y` 里的 `clamp`。
    #[test]
    fn the_ratio_follows_the_pointer_and_never_squeezes_a_side_below_the_minimum() {
        let (drawers, geoms) = setup();
        let div = drawer_divider_at(&drawers, &geoms, (400.0, boundary(&geoms)), 1.0).unwrap();
        let min = min_side_px(1.0, 20.0);
        let mid = div.span.y as f32 + div.span.h as f32 / 2.0;
        assert!((ratio_for(&div, mid, min) - 0.5).abs() < 1e-3);
        let hi = ratio_for(&div, -1000.0, min);
        let lo = ratio_for(&div, 100_000.0, min);
        assert!((hi - min as f32 / div.span.h as f32).abs() < 1e-3, "{hi}");
        assert!(
            (lo - (1.0 - min as f32 / div.span.h as f32)).abs() < 1e-3,
            "{lo}"
        );
    }

    /// 放不下两个最小侧时退化居中,不出负数/越界比例。
    #[test]
    fn a_span_too_small_for_two_minimum_sides_falls_back_to_the_middle() {
        let div = DrawerDivider {
            drawer: PaneId(2),
            span: PxRect {
                x: 0,
                y: 0,
                w: 100,
                h: 50,
            },
            boundary_y: 40,
        };
        assert!((ratio_for(&div, 0.0, 200) - 0.5).abs() < 1e-6);
    }
}
