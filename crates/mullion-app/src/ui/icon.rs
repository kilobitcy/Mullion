//! 控制图标。**用 epaint 直接画,不走字体。**
//!
//! 走查 P0-5 报的「跳板链上那个 □ 看不出是删除」，读代码发现源码里写的
//! 其实是 `✕`(U+2715)——`□` 是**缺字形的豆腐块**。`ui::install_cjk_font`
//! 只装了 egui 内置拉丁字体 + 微软雅黑,U+2715 两边都没有。换成 `🗑`
//! 只会把豆腐换个位置。自绘是唯一不受字体覆盖面影响的做法。
//!
//! `shapes()` 拆成纯函数是为了让「↑ 画成了 ↓」这类 bug 能被单测抓到 ——
//! 它不会引发编译错误、不会 panic,只会让用户点「上移」时条目往下跑。

use egui::{pos2, Rect, Response, Shape, Stroke, Ui, Vec2};

/// 本项目用到的控制图标。加新图标时同步给 `shapes()` 补分支 ——
/// `match` 是穷尽的,漏了会编译不过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// 叉:移除/删除。
    Cross,
    /// 上移。
    ArrowUp,
    /// 下移。
    ArrowDown,
    /// ⓘ:说明性提示。挂在常驻灰字前面(走查 18)。
    Info,
    /// 刷新。顶掉 U+27F3 与 U+21BB 两个字符 —— 都不在 GBK,是豆腐。
    Refresh,
    /// 实心下三角:折叠面板的**展开**态。顶掉 U+25BE(GBK 外)。
    TriangleDown,
    /// 实心右三角:折叠面板的**折起**态。顶掉 U+25B8(GBK 外)。
    TriangleRight,
    /// F204:空心方框 —— 窗口「最大化」。顶掉 U+25A1:那个字符**可能**在
    /// GBK 内,但按 `ui::glyphs` 的纪律,登记白名单要先在 Windows 实机上
    /// 画出来看一眼,而自绘这条路根本不问字体。
    Maximize,
    /// F204:两个错位方框 —— 窗口「还原」。Windows 上的既有心智。
    Restore,
}

impl Glyph {
    /// 全部变体。**加变体时必须同步这里** —— 测试遍历的是它,漏了就等于
    /// 那个新图标没有任何越界守护。`shapes()` 的穷尽 `match` 拦得住「忘了
    /// 画」,拦不住「忘了登记进 ALL」。
    pub const ALL: &'static [Glyph] = &[
        Glyph::Cross,
        Glyph::ArrowUp,
        Glyph::ArrowDown,
        Glyph::Info,
        Glyph::Refresh,
        Glyph::TriangleDown,
        Glyph::TriangleRight,
        Glyph::Maximize,
        Glyph::Restore,
    ];
}

/// 图标笔画占 rect 的比例。留边是为了让图标在按钮里不顶着边框。
const INSET: f32 = 0.28;

/// 把一个图标摊成 epaint 形状。纯函数,不碰 `Ui`。
///
/// `rect` 是图标的**外框**(通常是按钮内容区的正方形部分),所有端点保证
/// 落在框内 —— 越界的笔画会画进邻居按钮的地盘,而 egui 对此毫无怨言。
pub fn shapes(rect: Rect, glyph: Glyph, stroke: Stroke) -> Vec<Shape> {
    // 取正方形内接区,再按 INSET 收边。非正方 rect 下箭头不会被拉扁。
    let side = rect.width().min(rect.height());
    let c = rect.center();
    let h = side * (0.5 - INSET);
    match glyph {
        Glyph::Cross => vec![
            Shape::LineSegment {
                points: [pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)],
                stroke: stroke.into(),
            },
            Shape::LineSegment {
                points: [pos2(c.x + h, c.y - h), pos2(c.x - h, c.y + h)],
                stroke: stroke.into(),
            },
        ],
        // 人字形(chevron),不画箭杆:16px 见方里画带杆的箭头笔画会糊成一团。
        Glyph::ArrowUp => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y + h * 0.5),
                pos2(c.x, c.y - h * 0.7),
                pos2(c.x + h, c.y + h * 0.5),
            ],
            stroke,
        )],
        Glyph::ArrowDown => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y - h * 0.5),
                pos2(c.x, c.y + h * 0.7),
                pos2(c.x + h, c.y - h * 0.5),
            ],
            stroke,
        )],
        // 圆圈 + i。「点」画成一段极短的竖线而不是 `Shape::circle_filled`:
        // 12px 见方里填充小圆会被反走样抹成一团灰,一段 1.5px 粗的线反而清楚。
        Glyph::Info => vec![
            Shape::circle_stroke(c, h, stroke),
            Shape::LineSegment {
                points: [pos2(c.x, c.y - h * 0.55), pos2(c.x, c.y - h * 0.4)],
                stroke: stroke.into(),
            },
            Shape::LineSegment {
                points: [pos2(c.x, c.y - h * 0.1), pos2(c.x, c.y + h * 0.55)],
                stroke: stroke.into(),
            },
        ],
        // 顺时针 270° 圆弧 + 端点箭头。epaint 没有 arc 图元,用 16 段折线
        // 近似 —— 16px 见方下肉眼分辨不出是折线。
        //
        // 半径取 `h * 0.6` 而不是贴着 `h`:箭头的两条翼各再伸出 `h * 0.35`,
        // 加起来 0.95h 仍在框内。贴边画的话箭头会捅进邻居按钮的地盘,而
        // `every_glyph_stays_inside_its_rect` 正是为此存在。
        Glyph::Refresh => {
            const SEGS: usize = 16;
            let r = h * 0.6;
            let a0 = std::f32::consts::FRAC_PI_2;
            let sweep = std::f32::consts::PI * 1.5;
            let pts: Vec<_> = (0..=SEGS)
                .map(|i| {
                    let a = a0 + sweep * (i as f32 / SEGS as f32);
                    pos2(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect();
            let tip = pts[SEGS];
            let wing = h * 0.35;
            vec![
                Shape::line(pts, stroke),
                Shape::LineSegment {
                    points: [tip, pos2(tip.x - wing, tip.y - wing * 0.4)],
                    stroke: stroke.into(),
                },
                Shape::LineSegment {
                    points: [tip, pos2(tip.x + wing * 0.4, tip.y + wing)],
                    stroke: stroke.into(),
                },
            ]
        }
        // 实心三角。**用填充不用描边**:12px 见方里空心三角的三条边会被
        // 反走样糊成一团灰。`convex_polygon` 产出的是 `Shape::Path`,
        // 测试里的 `points_of` 已经认得。
        Glyph::TriangleDown => vec![Shape::convex_polygon(
            vec![
                pos2(c.x - h * 0.7, c.y - h * 0.4),
                pos2(c.x + h * 0.7, c.y - h * 0.4),
                pos2(c.x, c.y + h * 0.6),
            ],
            stroke.color,
            Stroke::NONE,
        )],
        Glyph::TriangleRight => vec![Shape::convex_polygon(
            vec![
                pos2(c.x - h * 0.4, c.y - h * 0.7),
                pos2(c.x - h * 0.4, c.y + h * 0.7),
                pos2(c.x + h * 0.6, c.y),
            ],
            stroke.color,
            Stroke::NONE,
        )],
        // 一个闭合方框。用 `Shape::line` 首尾同点而不是 `rect_stroke`:
        // 后者产出 `Shape::Rect`,`points_of` 认不得,越界守护会当场 panic。
        Glyph::Maximize => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y - h),
                pos2(c.x + h, c.y - h),
                pos2(c.x + h, c.y + h),
                pos2(c.x - h, c.y + h),
                pos2(c.x - h, c.y - h),
            ],
            stroke,
        )],
        // 前框(左下)整个画完,后框(右上)只画露出来的那个「Γ」——
        // 被前框挡住的两条边不画,否则 12px 见方里六条线糊成一坨。
        Glyph::Restore => {
            let d = h * 0.5;
            vec![
                Shape::line(
                    vec![
                        pos2(c.x - h, c.y - d),
                        pos2(c.x + d, c.y - d),
                        pos2(c.x + d, c.y + h),
                        pos2(c.x - h, c.y + h),
                        pos2(c.x - h, c.y - d),
                    ],
                    stroke,
                ),
                Shape::line(
                    vec![
                        pos2(c.x - d, c.y - d),
                        pos2(c.x - d, c.y - h),
                        pos2(c.x + h, c.y - h),
                        pos2(c.x + h, c.y + d),
                        pos2(c.x + d, c.y + d),
                    ],
                    stroke,
                ),
            ]
        }
    }
}

/// 纯装饰图标,不可交互。给一行说明性灰字挂个 ⓘ 用。
///
/// 不走 `icon_button`:那会画出按钮底色和 hover 高亮,等于告诉用户这里能点。
pub fn icon_inline(ui: &mut Ui, glyph: Glyph, color: egui::Color32) {
    // 比按钮小一圈 —— 它跟的是正文字号的灰字,不是一个控件。
    let size = Vec2::splat(ui.spacing().interact_size.y * 0.75);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .extend(shapes(rect, glyph, Stroke::new(1.2, color)));
    }
}

/// 图标按钮。返回是否被点击。
///
/// `tooltip` 是**必填**参数而不是 `Option`:走查 P0-5 的另一半是
/// 「所有图标按钮都要有 hover tooltip」。做成必填,新加图标按钮时
/// 就不可能忘 —— 编译器会要求你传。
pub fn icon_button(ui: &mut Ui, glyph: Glyph, enabled: bool, tooltip: &str) -> bool {
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    // tooltip 无条件挂:禁用态(第一跳的「上移」)更需要说明它是什么,
    // 而 `on_disabled_hover_text` 只对 `add_enabled` 造出来的 Response 生效,
    // 这里的 Response 来自 `allocate_exact_size`,它永远算「启用」。
    let resp: Response = resp.on_hover_text(tooltip);
    // 图标按钮**一个字都不画**,不报的话它在 accesskit 树里是个没有名字的
    // 空节点 —— 屏幕阅读器只念得出「按钮」,F100 的自动候选也认不出它是谁。
    // 拿 tooltip 当名字:它本来就是这颗按钮的自述,不会另起一套说法走岔。
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tooltip));

    if ui.is_rect_visible(rect) {
        let (rounding, weak_bg, bg_stroke, fg) = if enabled {
            let v = ui.style().interact(&resp);
            (v.rounding, v.weak_bg_fill, v.bg_stroke, v.fg_stroke.color)
        } else {
            // 复刻 egui 标准禁用按钮的两步,一步都不能省:
            //
            // 1. 底色取 `inactive` 而**不是** `noninteractive`。`add_enabled`
            //    只是把 `ui.enabled` 置假,widget 内部照样走
            //    `Widgets::style(&response)`,而那里只有 `sense` 不可交互时
            //    才会落到 `noninteractive`(egui-0.30.0 style.rs)——标准禁用
            //    按钮落的是 `inactive`。本项目主题下这两档的前景色**不一样**
            //    (实测 gray_out 后 #535353 vs #676767),取错就比同一行里
            //    别的禁用控件暗一档。
            // 2. 每个颜色再 `gray_out` 一遍。`Ui::disable()` 的实际做法是给
            //    painter 挂 `fade_to_color`,把整幅绘制向
            //    `fade_out_to_color()` 淡出(painter.rs:225
            //    `tint_shape_towards`);`Visuals::gray_out` 就是它对单个
            //    颜色的公开等价物(style.rs:1017)。原先手写的
            //    `weak_text_color()` 不是这一档。
            //
            // 这里没法直接用 `ui.disable()`:那会连同 tooltip 的行为一起改,
            // 而这个按钮的 tooltip 在禁用态**更**需要显示(见上面 `resp` 那段)。
            let v = &ui.visuals().widgets.inactive;
            let g = |c| ui.visuals().gray_out(c);
            (
                v.rounding,
                g(v.weak_bg_fill),
                Stroke::new(v.bg_stroke.width, g(v.bg_stroke.color)),
                g(v.fg_stroke.color),
            )
        };
        ui.painter().rect(rect, rounding, weak_bg, bg_stroke);
        ui.painter()
            .extend(shapes(rect, glyph, Stroke::new(1.5, fg)));
    }
    enabled && resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Color32, Rect, Stroke};

    fn r() -> Rect {
        Rect::from_min_max(pos2(10.0, 20.0), pos2(26.0, 36.0))
    }
    fn s() -> Stroke {
        Stroke::new(1.5, Color32::WHITE)
    }

    /// 所有端点都得落在给定矩形内。越界的图标会画到邻居按钮的地盘上,
    /// 而 egui 不会因此报任何错 —— 只有人眼能看出来。
    #[test]
    fn every_glyph_stays_inside_its_rect() {
        for g in Glyph::ALL.iter().copied() {
            for p in points_of(&shapes(r(), g, s())) {
                assert!(r().contains(p), "{g:?} 的端点 {p:?} 跑出了 {:?}", r());
            }
        }
    }

    /// ↑ 和 ↓ 画反了不会有任何编译错误,也不会有任何 panic ——
    /// 用户点「上移」结果条目往下跑。这是本模块唯一真正会出的 bug。
    #[test]
    fn arrow_up_points_up_and_arrow_down_points_down() {
        let up = points_of(&shapes(r(), Glyph::ArrowUp, s()));
        let down = points_of(&shapes(r(), Glyph::ArrowDown, s()));
        let cy = r().center().y;
        // 尖端:离中心竖直方向最远的那个点。
        let apex_up = up
            .iter()
            .copied()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        let apex_down = down
            .iter()
            .copied()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        assert!(apex_up.y < cy, "ArrowUp 的尖端在中心线下方,画反了");
        assert!(apex_down.y > cy, "ArrowDown 的尖端在中心线上方,画反了");
        // 尖端必须在水平中线附近,否则画出来是个斜杠不是箭头。
        assert!((apex_up.x - r().center().x).abs() < 1.0);
        assert!((apex_down.x - r().center().x).abs() < 1.0);
    }

    /// 叉必须是两条**相交**的线,不是两条平行线也不是一条。
    #[test]
    fn cross_is_two_segments_that_actually_cross() {
        let sh = shapes(r(), Glyph::Cross, s());
        assert_eq!(sh.len(), 2, "叉是两笔");
        let pts = points_of(&sh);
        assert_eq!(pts.len(), 4);
        // 两条线的中点都应落在矩形中心。
        let m0 = (pts[0] + pts[1].to_vec2()) / 2.0;
        let m1 = (pts[2] + pts[3].to_vec2()) / 2.0;
        let c = r().center();
        assert!((m0 - c).length() < 0.01, "第一笔不过中心");
        assert!((m1 - c).length() < 0.01, "第二笔不过中心");
        // 斜率必须一正一负,否则是两条平行线。
        let k0 = (pts[1].y - pts[0].y) / (pts[1].x - pts[0].x);
        let k1 = (pts[3].y - pts[2].y) / (pts[3].x - pts[2].x);
        assert!(k0 * k1 < 0.0, "两笔同向,画出来是个等号不是叉");
    }

    /// ⓘ 得是「一个圈里装一个 i」:圈必须描边(不是实心,实心的话里面的
    /// i 看不见),i 的点必须在竖杠**上方**(倒过来就成了叹号)。
    #[test]
    fn info_is_a_ring_with_a_dot_above_a_stem() {
        let sh = shapes(r(), Glyph::Info, s());
        let ring = sh
            .iter()
            .find_map(|x| match x {
                Shape::Circle(cs) => Some(*cs),
                _ => None,
            })
            .expect("ⓘ 没有外圈");
        assert_eq!(
            ring.fill,
            Color32::TRANSPARENT,
            "外圈是实心的,里面的 i 会被盖住"
        );
        assert!(ring.radius > 0.0);

        let segs: Vec<_> = sh
            .iter()
            .filter_map(|x| match x {
                Shape::LineSegment { points, .. } => Some(*points),
                _ => None,
            })
            .collect();
        assert_eq!(segs.len(), 2, "i 是「点 + 竖杠」两笔");
        let mid_y = |p: [egui::Pos2; 2]| (p[0].y + p[1].y) / 2.0;
        let len = |p: [egui::Pos2; 2]| (p[1] - p[0]).length();
        // 短的那笔是点,它必须在长的那笔上面。
        let (dot, stem) = if len(segs[0]) < len(segs[1]) {
            (segs[0], segs[1])
        } else {
            (segs[1], segs[0])
        };
        assert!(
            mid_y(dot) < mid_y(stem),
            "点跑到竖杠下面了 —— 画出来是个叹号"
        );
        // 两笔都得竖直且共线,否则不成一个 i。
        for p in [dot, stem] {
            assert!((p[0].x - p[1].x).abs() < 0.01, "i 的笔画不竖直");
            assert!((p[0].x - r().center().x).abs() < 0.01, "i 没在圈中央");
        }
    }

    /// 图标随 rect 缩放,不能写死像素 —— 按钮高度跟字号走,
    /// 字号一变图标就该跟着变。
    #[test]
    fn glyphs_scale_with_the_rect() {
        let big = Rect::from_min_max(pos2(0.0, 0.0), pos2(64.0, 64.0));
        let small = Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0));
        let span = |rc: Rect| {
            let p = points_of(&shapes(rc, Glyph::Cross, s()));
            let xs: Vec<f32> = p.iter().map(|q| q.x).collect();
            xs.iter().cloned().fold(f32::MIN, f32::max)
                - xs.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(span(big) > span(small) * 3.0, "图标没跟着 rect 缩放");
    }

    /// 禁用态得跟 egui 自己的禁用控件用同一套压暗规则。
    ///
    /// 判据用**渲染出来的启用色**当输入去算期望值,而不是照抄生产代码里的
    /// `noninteractive.fg_stroke.color` —— 后者会变成重言式。这条断言额外
    /// 编码了「noninteractive 的前景色跟 inactive 一样」这个当前主题事实:
    /// 哪天换主题让二者不等,它会红,提醒重新审视这里该压哪一档。
    #[test]
    fn disabled_icon_uses_the_same_gray_out_as_every_other_disabled_widget() {
        let run = |enabled: bool| {
            let ctx = egui::Context::default();
            let mut gray_out_of = None;
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    icon_button(ui, Glyph::Cross, enabled, "删除");
                    let v = ui.visuals().clone();
                    gray_out_of = Some(move |c: Color32| v.gray_out(c));
                });
            });
            let color = out
                .shapes
                .iter()
                .find_map(|cs| match &cs.shape {
                    // `Shape::LineSegment` 拿的是 `PathStroke`,颜色是
                    // `ColorMode`(还可以是随位置变的渐变),`shapes()` 只产
                    // `Solid`,别的模式说明有人改了画法。
                    egui::Shape::LineSegment { stroke, .. } => match &stroke.color {
                        egui::epaint::ColorMode::Solid(c) => Some(*c),
                        other => panic!("图标笔画不再是纯色:{other:?}"),
                    },
                    _ => None,
                })
                .expect("没画出图标笔画");
            (color, gray_out_of.expect("闭包里没跑到"))
        };

        let (on, gray_out) = run(true);
        let (off, _) = run(false);
        assert_ne!(on, off, "禁用态跟启用态同色,用户看不出这个按钮点不动");
        assert_eq!(
            off,
            gray_out(on),
            "禁用态没走 egui 标准的 gray_out —— 会比同一行里别的禁用控件暗一档"
        );
    }

    /// 折叠三角必须朝对方向 —— 画反了不会编译错、不会 panic,只会让
    /// 「展开」看起来像「折起」。同 `arrow_up_points_up_and_arrow_down_points_down`
    /// 的理由。
    ///
    /// 自证会变红:把 `shapes()` 里 `TriangleDown` 和 `TriangleRight`
    /// 两个分支的返回值对调。
    #[test]
    fn the_collapse_triangles_point_down_and_right() {
        let down = points_of(&shapes(r(), Glyph::TriangleDown, s()));
        let right = points_of(&shapes(r(), Glyph::TriangleRight, s()));
        let c = r().center();
        // 尖端 = 离中心最远的那个点(三角只有三个点,尖端唯一)。
        let apex_down = down
            .iter()
            .copied()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        let apex_right = right
            .iter()
            .copied()
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .unwrap();
        assert!(apex_down.y > c.y, "TriangleDown 的尖端没朝下");
        assert!(
            (apex_down.x - c.x).abs() < 1.0,
            "TriangleDown 的尖端没在竖直中线上"
        );
        assert!(apex_right.x > c.x, "TriangleRight 的尖端没朝右");
        assert!(
            (apex_right.y - c.y).abs() < 1.0,
            "TriangleRight 的尖端没在水平中线上"
        );
    }

    /// `Glyph::ALL` 必须真的列全 —— 漏一个,`every_glyph_stays_inside_its_rect`
    /// 就悄悄不覆盖它了(本项目记过的「列举式门控在加档时必然漏」)。
    ///
    /// 没有办法让编译器数枚举变体,所以判据取「每个变体画出来的点集**互不
    /// 相同**」:至少能保证 ALL 里没有重复填充、凑数目。真正的闸门是
    /// `shapes()` 那个穷尽 `match` —— 加变体不补分支直接编译不过。
    ///
    /// 自证会变红:把 `ALL` 里的 `Glyph::TriangleRight` 改成再写一遍
    /// `Glyph::TriangleDown`。
    #[test]
    fn every_glyph_in_all_draws_something_distinct() {
        let mut seen: Vec<Vec<egui::Pos2>> = Vec::new();
        for g in Glyph::ALL.iter().copied() {
            let pts = points_of(&shapes(r(), g, s()));
            assert!(!pts.is_empty(), "{g:?} 什么都没画");
            assert!(
                !seen.contains(&pts),
                "{g:?} 与 ALL 里另一个变体画得一模一样"
            );
            seen.push(pts);
        }
    }

    /// F204:最大化 = **一个**方框,还原 = **两个错位**方框(Windows 惯例)。
    ///
    /// 两个都必须存在且长得不一样,否则用户按下「最大化」之后,那颗按钮
    /// 看上去什么都没变 —— 而它此刻的含义已经反过来了。
    ///
    /// 判据是「还原比最大化画得多」而不是「恰好 8 个点」:后者把实现细节
    /// (方框用一条闭合折线还是四段线)抄进了测试,换个画法就假红。
    ///
    /// 自证会变红:把 `Glyph::Restore` 的分支改成与 `Maximize` 同一份形状。
    #[test]
    fn restore_draws_two_offset_squares_so_it_reads_differently_from_maximize() {
        let max = points_of(&shapes(r(), Glyph::Maximize, s()));
        let res = points_of(&shapes(r(), Glyph::Restore, s()));
        assert!(!max.is_empty(), "Maximize 什么都没画");
        assert!(
            res.len() > max.len(),
            "Restore 画的点({})不比 Maximize({})多 —— 那它多半不是「两个方框」",
            res.len(),
            max.len()
        );
        // 最大化那一个必须是**闭合**的方框:四个角都得出现,缺一条边看着
        // 像个「L」,认不出是窗口。
        let (l, t, rr, b) = (
            max.iter().map(|p| p.x).fold(f32::MAX, f32::min),
            max.iter().map(|p| p.y).fold(f32::MAX, f32::min),
            max.iter().map(|p| p.x).fold(f32::MIN, f32::max),
            max.iter().map(|p| p.y).fold(f32::MIN, f32::max),
        );
        for corner in [(l, t), (rr, t), (l, b), (rr, b)] {
            assert!(
                max.iter()
                    .any(|p| (p.x - corner.0).abs() < 0.01 && (p.y - corner.1).abs() < 0.01),
                "Maximize 的方框缺了角 {corner:?} —— 画出来不是一个闭合的框"
            );
        }
    }

    /// 从形状里抠出所有端点,给上面几个测试用。
    fn points_of(shapes: &[egui::Shape]) -> Vec<egui::Pos2> {
        let mut out = Vec::new();
        for s in shapes {
            match s {
                egui::Shape::LineSegment { points, .. } => out.extend_from_slice(points),
                egui::Shape::Path(p) => out.extend_from_slice(&p.points),
                // 圆没有端点,拿它的外接正方形四个角代替 —— 越界检查要的
                // 正是「最远能画到哪」。
                egui::Shape::Circle(cs) => {
                    let (c, r) = (cs.center, cs.radius);
                    out.extend_from_slice(&[
                        pos2(c.x - r, c.y - r),
                        pos2(c.x + r, c.y - r),
                        pos2(c.x - r, c.y + r),
                        pos2(c.x + r, c.y + r),
                    ]);
                }
                other => panic!("图标里出现了没预期的形状:{other:?}"),
            }
        }
        out
    }
}
