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
    /// F224:项目灯**亮**(实心圆)—— 有 pane 正 attach 在它的 tmux 上。
    LampLit,
    /// F224:项目灯**灭**(空心圆)—— 全部 pane 都上报过且无一命中。
    LampDark,
    /// F224:项目灯**未知**(空心圆 + 圆心一点)—— 还有 pane 没上报过。
    ///
    /// 三态**不许退化成两态**:把「未知」画成「灭」的话,用户会以为项目
    /// 没在跑而去开第二份 —— 同一个目录两个 Claude Code,正是这盏灯要防的。
    LampUnknown,
    /// F225③:文件夹轮廓 —— pane 标题条上的「切到另一个项目」。
    ///
    /// 自绘而不是找一个文件夹字符:同 [`Glyph::Maximize`] 的理由,字形缺失
    /// 只有人眼能发现,而 F21 允许用户换显示字体,换一次就可能再缺一次。
    Project,
    /// F278:放大镜 —— 文件面板路径条上的「在这个目录下递归搜索」。
    ///
    /// 自绘而不是找一个放大镜字符:U+1F50D 与 U+2315 都在 GBK 外,
    /// 两级字体链都画不出来,而这种缺失**只有人眼能发现**(T9)。
    Search,
    /// F340:便签 —— 项目行上「这个项目有备忘」的标记。
    ///
    /// 自绘理由同 [`Glyph::Search`]:任何便签/笔记字符都在 GBK 外(T9)。
    Memo,
    /// F383 起一批菜单图标(自绘理由同上,T9)。终端框 + `>_`:会话管理器。
    Terminal,
    /// 三层叠放:分组管理器。
    Layers,
    /// 上右下左两支箭头:隧道管理器。
    Tunnel,
    /// 箭头落进托盘:导入。
    Import,
    /// 箭头从托盘出来:导出。
    Export,
    /// 折角的纸:导出脱敏日志。与 [`Glyph::Memo`] 靠折角区分。
    Document,
    /// 带逆时针箭头的钟:恢复上次的现场。
    History,
    /// 电源:退出。
    Power,
    /// 顶部加粗的窗口:显示 / 隐藏 pane 标题条。
    PanelTop,
    /// 左侧加粗的窗口:文件面板。
    PanelLeft,
    /// 云 + 上箭头:立刻备份到云。
    CloudUp,
    /// 齿轮:设置。
    Gear,
    /// 两个分开的插头:断开。
    Unplug,
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
        Glyph::LampLit,
        Glyph::LampDark,
        Glyph::LampUnknown,
        Glyph::Project,
        Glyph::Search,
        Glyph::Memo,
        Glyph::Terminal,
        Glyph::Layers,
        Glyph::Tunnel,
        Glyph::Import,
        Glyph::Export,
        Glyph::Document,
        Glyph::History,
        Glyph::Power,
        Glyph::PanelTop,
        Glyph::PanelLeft,
        Glyph::CloudUp,
        Glyph::Gear,
        Glyph::Unplug,
    ];
}

/// 图标笔画占 rect 的比例。留边是为了让图标在按钮里不顶着边框。
const INSET: f32 = 0.28;

/// F382:转圈 —— 一段 270° 的弧,`phase`(秒)每秒转一圈。按时间算角度而
/// 不是按帧数:帧率随负载变,按帧转会忽快忽慢。纯折线,零字体依赖(T9)。
pub fn spinner(rect: Rect, phase: f64, stroke: Stroke) -> Shape {
    let side = rect.width().min(rect.height());
    let c = rect.center();
    let r = side * (0.5 - INSET);
    let start = (phase.fract() * std::f64::consts::TAU) as f32;
    const N: usize = 18;
    let span = std::f32::consts::TAU * 0.75;
    let points = (0..=N)
        .map(|i| {
            let a = start + span * i as f32 / N as f32;
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    Shape::line(points, stroke)
}

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
        // F224 三态灯。半径取 `h * 0.7`:圆是三个里最「胖」的形状,贴着 `h`
        // 画的话描边的外半宽会越出 rect(`every_glyph_stays_inside_its_rect`
        // 拦的正是这个)。
        //
        // 实心用 `circle_filled`、空心用 `circle_stroke`,**不是同一个圈换
        // 颜色**:换颜色在深色底上几乎看不出差别,而这盏灯的全部价值就是
        // 一眼分辨。
        Glyph::LampLit => vec![Shape::circle_filled(c, h * 0.7, stroke.color)],
        Glyph::LampDark => vec![Shape::circle_stroke(c, h * 0.7, stroke)],
        // 空心圈 + 圆心一点。点画成一段极短的竖线而不是小实心圆,理由同
        // `Glyph::Info`:小尺寸下填充小圆会被反走样抹成一团灰。
        Glyph::LampUnknown => vec![
            Shape::circle_stroke(c, h * 0.7, stroke),
            Shape::LineSegment {
                points: [pos2(c.x, c.y - h * 0.12), pos2(c.x, c.y + h * 0.12)],
                stroke: stroke.into(),
            },
        ],
        // 文件夹轮廓:左上角一个凸起的标签,其余是个盒子。首尾同点的
        // `Shape::line`,同 `Maximize` 的理由(`rect_stroke` 产出的
        // `Shape::Rect` 越界守护认不得)。
        Glyph::Project => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y + h),
                pos2(c.x - h, c.y - h),
                pos2(c.x - h * 0.1, c.y - h),
                pos2(c.x + h * 0.15, c.y - h * 0.55),
                pos2(c.x + h, c.y - h * 0.55),
                pos2(c.x + h, c.y + h),
                pos2(c.x - h, c.y + h),
            ],
            stroke,
        )],
        // 圆环 + 一条向右下的柄。半径取 `h * 0.6`,柄从圆周 45° 处再伸
        // `h * 0.45`,合计 0.6*0.707 + 0.45 ≈ 0.87h,仍在框内 ——
        // `every_glyph_stays_inside_its_rect` 正是为此存在。
        // 一张纸:外框(首尾同点的 `Shape::line`,理由同 `Project`)+ 两道横线。
        Glyph::Memo => vec![
            Shape::line(
                vec![
                    pos2(c.x - h, c.y - h),
                    pos2(c.x + h, c.y - h),
                    pos2(c.x + h, c.y + h),
                    pos2(c.x - h, c.y + h),
                    pos2(c.x - h, c.y - h),
                ],
                stroke,
            ),
            Shape::LineSegment {
                points: [
                    pos2(c.x - h * 0.5, c.y - h * 0.3),
                    pos2(c.x + h * 0.5, c.y - h * 0.3),
                ],
                stroke: stroke.into(),
            },
            Shape::LineSegment {
                points: [
                    pos2(c.x - h * 0.5, c.y + h * 0.3),
                    pos2(c.x + h * 0.5, c.y + h * 0.3),
                ],
                stroke: stroke.into(),
            },
        ],
        Glyph::Terminal
        | Glyph::Layers
        | Glyph::Tunnel
        | Glyph::Import
        | Glyph::Export
        | Glyph::Document
        | Glyph::History
        | Glyph::Power
        | Glyph::PanelTop
        | Glyph::PanelLeft
        | Glyph::CloudUp
        | Glyph::Gear
        | Glyph::Unplug => menu_shapes(c, h, glyph, stroke),
        Glyph::Search => {
            let r = h * 0.6;
            // 圆心往左上挪一点,给柄腾地方,整体视觉重心才落在框中间。
            let o = pos2(c.x - h * 0.15, c.y - h * 0.15);
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let from = pos2(o.x + d, o.y + d);
            let to = pos2(o.x + d + h * 0.45, o.y + d + h * 0.45);
            vec![
                Shape::circle_stroke(o, r, stroke),
                Shape::LineSegment {
                    points: [from, to],
                    stroke: stroke.into(),
                },
            ]
        }
    }
}

/// F383 菜单图标。坐标一律写成以 `c` 为心、`h` 为半边长的单位坐标 ——
/// 全部落在 `[-1, 1]` 内,`every_glyph_stays_inside_its_rect` 兜底。
fn menu_shapes(c: egui::Pos2, h: f32, glyph: Glyph, stroke: Stroke) -> Vec<Shape> {
    let p = |x: f32, y: f32| pos2(c.x + h * x, c.y + h * y);
    let seg = |a: egui::Pos2, b: egui::Pos2| Shape::LineSegment {
        points: [a, b],
        stroke: stroke.into(),
    };
    let line = |pts: Vec<egui::Pos2>| Shape::line(pts, stroke);
    // 角度单位是度、屏幕坐标(y 向下,角度增大 = 顺时针)。epaint 没有弧图元,折线近似。
    let arc = |cx: f32, cy: f32, r: f32, from: f32, sweep: f32| -> Vec<egui::Pos2> {
        const N: usize = 16;
        (0..=N)
            .map(|i| {
                let a = (from + sweep * i as f32 / N as f32).to_radians();
                p(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    let frame = || {
        line(vec![
            p(-1.0, -0.85),
            p(1.0, -0.85),
            p(1.0, 0.85),
            p(-1.0, 0.85),
            p(-1.0, -0.85),
        ])
    };
    let fill = |pts: Vec<egui::Pos2>| Shape::convex_polygon(pts, stroke.color, Stroke::NONE);
    let tray = || line(vec![p(-1.0, 0.2), p(-1.0, 1.0), p(1.0, 1.0), p(1.0, 0.2)]);
    match glyph {
        Glyph::Terminal => vec![
            frame(),
            line(vec![p(-0.6, -0.35), p(-0.2, 0.0), p(-0.6, 0.35)]),
            seg(p(0.0, 0.4), p(0.55, 0.4)),
        ],
        Glyph::Layers => vec![
            line(vec![
                p(0.0, -0.95),
                p(1.0, -0.45),
                p(0.0, 0.05),
                p(-1.0, -0.45),
                p(0.0, -0.95),
            ]),
            line(vec![p(-1.0, 0.05), p(0.0, 0.55), p(1.0, 0.05)]),
            line(vec![p(-1.0, 0.45), p(0.0, 0.95), p(1.0, 0.45)]),
        ],
        Glyph::Tunnel => vec![
            seg(p(-1.0, -0.4), p(1.0, -0.4)),
            line(vec![p(0.55, -0.8), p(1.0, -0.4), p(0.55, 0.0)]),
            seg(p(1.0, 0.4), p(-1.0, 0.4)),
            line(vec![p(-0.55, 0.0), p(-1.0, 0.4), p(-0.55, 0.8)]),
        ],
        Glyph::Import => vec![
            tray(),
            seg(p(0.0, -1.0), p(0.0, 0.5)),
            line(vec![p(-0.45, 0.05), p(0.0, 0.5), p(0.45, 0.05)]),
        ],
        Glyph::Export => vec![
            tray(),
            seg(p(0.0, 0.5), p(0.0, -1.0)),
            line(vec![p(-0.45, -0.55), p(0.0, -1.0), p(0.45, -0.55)]),
        ],
        Glyph::Document => vec![
            line(vec![
                p(-0.75, -1.0),
                p(0.3, -1.0),
                p(0.75, -0.55),
                p(0.75, 1.0),
                p(-0.75, 1.0),
                p(-0.75, -1.0),
            ]),
            line(vec![p(0.3, -1.0), p(0.3, -0.55), p(0.75, -0.55)]),
            seg(p(-0.4, 0.0), p(0.4, 0.0)),
            seg(p(-0.4, 0.45), p(0.4, 0.45)),
        ],
        Glyph::History => {
            // 圆弧从 210°(左上)顺时针扫 300°。逆时针读的话 210° 是终点,
            // 到达时的走向是左下;两翼朝反方向(-60°,右上)各偏 ±40°。
            let pts = arc(0.0, 0.0, 0.8, 210.0, 300.0);
            let tip = pts[0];
            let wing = |da: f32| {
                let a = (-60.0 + da).to_radians();
                pos2(tip.x + h * 0.35 * a.cos(), tip.y + h * 0.35 * a.sin())
            };
            vec![
                line(pts),
                line(vec![wing(40.0), tip, wing(-40.0)]),
                line(vec![p(0.0, -0.45), p(0.0, 0.0), p(0.35, 0.25)]),
            ]
        }
        Glyph::Power => vec![
            line(arc(0.0, 0.1, 0.8, -60.0, 300.0)),
            seg(p(0.0, -1.0), p(0.0, -0.1)),
        ],
        Glyph::PanelTop => vec![
            frame(),
            fill(vec![
                p(-1.0, -0.85),
                p(1.0, -0.85),
                p(1.0, -0.4),
                p(-1.0, -0.4),
            ]),
        ],
        Glyph::PanelLeft => vec![
            frame(),
            fill(vec![
                p(-1.0, -0.85),
                p(-0.45, -0.85),
                p(-0.45, 0.85),
                p(-1.0, 0.85),
            ]),
        ],
        Glyph::CloudUp => {
            let mut pts = arc(-0.5, 0.25, 0.35, 90.0, 180.0);
            pts.extend(arc(0.05, -0.1, 0.5, 200.0, 140.0));
            pts.extend(arc(0.55, 0.25, 0.35, -90.0, 180.0));
            pts.push(p(-0.5, 0.6));
            vec![
                line(pts),
                seg(p(0.0, 0.45), p(0.0, -0.05)),
                line(vec![p(-0.25, 0.2), p(0.0, -0.05), p(0.25, 0.2)]),
            ]
        }
        Glyph::Gear => {
            let mut v = vec![
                Shape::circle_stroke(c, h * 0.3, stroke),
                Shape::circle_stroke(c, h * 0.62, stroke),
            ];
            for k in 0..8 {
                let a = (k as f32 * 45.0).to_radians();
                v.push(seg(
                    p(0.62 * a.cos(), 0.62 * a.sin()),
                    p(0.95 * a.cos(), 0.95 * a.sin()),
                ));
            }
            v
        }
        Glyph::Unplug => vec![
            seg(p(-1.0, 0.0), p(-0.3, 0.0)),
            seg(p(-0.3, -0.5), p(-0.3, 0.5)),
            seg(p(-0.3, -0.25), p(-0.05, -0.25)),
            seg(p(-0.3, 0.25), p(-0.05, 0.25)),
            seg(p(1.0, 0.0), p(0.3, 0.0)),
            seg(p(0.3, -0.5), p(0.3, 0.5)),
        ],
        _ => unreachable!("menu_shapes 只画 F383 那批"),
    }
}

/// F383:带图标的菜单项。图标槽固定在文字左边,文字用 `leading_space` 让开 ——
/// 走的仍是 egui 自己的 `Button`(menu 的整行宽、hover 底色、禁用灰、点击后
/// 收菜单的行为一样不少)。文字色用 `PLACEHOLDER`,由 `Button` 按交互态填。
pub fn menu_item(ui: &mut Ui, glyph: Glyph, text: &str, enabled: bool) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    // 可见笔画跨度 ≈ 字号。`shapes` 按 `INSET` 收边、只画中间 `1 - 2·INSET`,
    // 直接拿字号当 rect 边长会缩成 6px 左右的小点 —— 反推 rect,留位只按可见宽。
    let side = font.size;
    let rect_side = side / (1.0 - 2.0 * INSET);
    let gap = ui.spacing().icon_spacing.max(4.0);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        side + gap,
        egui::TextFormat {
            font_id: font,
            color: egui::Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    let resp = ui.add_enabled(enabled, egui::Button::new(job));
    if ui.is_rect_visible(resp.rect) {
        let pad = ui.spacing().button_padding.x;
        let r = Rect::from_center_size(
            pos2(resp.rect.left() + pad + side / 2.0, resp.rect.center().y),
            Vec2::splat(rect_side),
        );
        // 禁用态同 `icon_button`:`inactive` 那档再 `gray_out`。
        let color = if enabled {
            ui.style().interact(&resp).text_color()
        } else {
            ui.visuals()
                .gray_out(ui.visuals().widgets.inactive.text_color())
        };
        ui.painter()
            .extend(shapes(r, glyph, Stroke::new(1.4, color)));
    }
    resp
}

/// 纯装饰图标,不可交互。给一行说明性灰字挂个 ⓘ 用。
///
/// 不走 `icon_button`:那会画出按钮底色和 hover 高亮,等于告诉用户这里能点。
pub fn icon_inline(ui: &mut Ui, glyph: Glyph, color: egui::Color32) {
    icon_inline_rect(ui, glyph, color);
}

/// 同 `icon_inline`,并返回刚画的那一格的矩形(F337:登记 F100 标注要用,
/// 不能拿 `ui.min_rect()` 凑 —— 那是整个 ui 已占的区域)。
pub fn icon_inline_rect(ui: &mut Ui, glyph: Glyph, color: egui::Color32) -> egui::Rect {
    // 比按钮小一圈 —— 它跟的是正文字号的灰字,不是一个控件。
    let size = Vec2::splat(ui.spacing().interact_size.y * 0.75);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .extend(shapes(rect, glyph, Stroke::new(1.2, color)));
    }
    rect
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

    /// **F224 三态灯必须互相画得出区别。** 三个画成同一个圈的话,这盏灯
    /// 等于不存在 —— 而编译、测试、日志全静默,只有人眼能看出来(T9 同族)。
    ///
    /// 自证会变红:把 `Glyph::LampDark` 的分支照抄成 `LampLit` 的。
    #[test]
    fn the_three_lamp_states_are_actually_drawn_differently() {
        let filled = |sh: &[egui::Shape]| {
            sh.iter().any(|s| match s {
                egui::Shape::Circle(c) => c.fill != Color32::TRANSPARENT,
                _ => false,
            })
        };
        let lit = shapes(r(), Glyph::LampLit, s());
        let dark = shapes(r(), Glyph::LampDark, s());
        let unknown = shapes(r(), Glyph::LampUnknown, s());
        assert!(filled(&lit), "亮着的灯该是实心的");
        assert!(!filled(&dark), "灭的灯不该是实心的,否则跟「亮」一个样");
        assert_ne!(
            dark.len(),
            unknown.len(),
            "「灭」和「未知」画成了同一个东西 —— 三态退化成两态"
        );
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
    /// 没有办法让编译器数枚举变体,所以判据取「每个变体画出来的**点集 +
    /// 各圆的实心与否**互不相同」:至少能保证 ALL 里没有重复填充、凑数目。
    /// 真正的闸门是 `shapes()` 那个穷尽 `match` —— 加变体不补分支直接编译不过。
    ///
    /// **填充维度不是装饰**:`points_of` 对 `circle_filled` 与 `circle_stroke`
    /// 给出完全相同的点集,而 F224 的「亮」与「灭」正是同一个圆的实心/空心
    /// 两态 —— 只比点集的话,这条会把一对**确实不同**的图标误判成重复。
    ///
    /// 自证会变红:把 `ALL` 里的 `Glyph::TriangleRight` 改成再写一遍
    /// `Glyph::TriangleDown`。
    #[test]
    fn every_glyph_in_all_draws_something_distinct() {
        let mut seen: Vec<(Vec<egui::Pos2>, Vec<bool>)> = Vec::new();
        for g in Glyph::ALL.iter().copied() {
            let sh = shapes(r(), g, s());
            let fills: Vec<bool> = sh
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Circle(c) => Some(c.fill != Color32::TRANSPARENT),
                    _ => None,
                })
                .collect();
            let pts = points_of(&sh);
            assert!(!pts.is_empty(), "{g:?} 什么都没画");
            let key = (pts, fills);
            assert!(
                !seen.contains(&key),
                "{g:?} 与 ALL 里另一个变体画得一模一样"
            );
            seen.push(key);
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
    /// F383:菜单项的文字让出图标槽,图标画在槽里、不压字;文字色交给
    /// `Button` 按交互态填(不是写死的灰)。
    ///
    /// 自证会变红:`leading_space` 传 0(第一条红);或 `TextFormat` 不设
    /// `PLACEHOLDER`(文字成了默认灰,第三条红)。
    #[test]
    fn a_menu_item_leaves_room_for_its_icon_left_of_the_text() {
        let ctx = egui::Context::default();
        let mut btn = Rect::NOTHING;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                btn = menu_item(ui, Glyph::Gear, "设置…", true).rect;
            });
        });
        let text = out
            .shapes
            .iter()
            .find_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == "设置…" => Some(t.clone()),
                _ => None,
            })
            .expect("没画出菜单文字");
        let glyph_left = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Circle(cs) => Some(cs.center.x + cs.radius),
                _ => None,
            })
            .fold(f32::MIN, f32::max);
        let first_char_x = text.pos.x + text.galley.rows[0].glyphs[0].pos.x;
        assert!(
            first_char_x > glyph_left,
            "文字压在图标上:{first_char_x} <= {glyph_left}"
        );
        assert!(glyph_left > btn.left(), "图标不在按钮里");
        assert_ne!(
            text.galley.job.sections[0].format.color,
            egui::Color32::GRAY,
            "文字色写死成默认灰,hover/禁用不会变"
        );
    }

    /// 菜单图标的可见跨度要与字号相当。实报:按字号当 rect 边长传进 `shapes`,
    /// 收边之后只剩 6px 上下,菜单里看着像个小点。
    ///
    /// 自证会变红:把 `menu_item` 里的 `rect_side` 改回 `side`。
    #[test]
    fn a_menu_icon_is_about_as_big_as_the_text_next_to_it() {
        let ctx = egui::Context::default();
        let mut size = 0.0;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                size = egui::TextStyle::Button.resolve(ui.style()).size;
                menu_item(ui, Glyph::Gear, "设置…", true);
            });
        });
        let shapes: Vec<egui::Shape> = out
            .shapes
            .iter()
            .map(|c| c.shape.clone())
            .filter(|s| {
                !matches!(
                    s,
                    egui::Shape::Text(_) | egui::Shape::Rect(_) | egui::Shape::Noop
                )
            })
            .collect();
        let pts = points_of(&shapes);
        assert!(!pts.is_empty(), "没画出图标");
        let (lo, hi) = pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        });
        assert!(
            hi - lo >= size * 0.8,
            "图标横向只有 {} px,字号 {size}",
            hi - lo
        );
    }

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
